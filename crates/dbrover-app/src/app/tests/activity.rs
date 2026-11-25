//! Server activity (W15) through the whole app, on the PostgreSQL test server: right-click a
//! data source → **Server activity**, a session waiting on another's row lock drawn under it,
//! Kill asking first on a careful data source and the history recording it, and a read-only
//! data source's buttons disabled.

use dbrover_core::SafetyLevel;
use egui_kittest::kittest::NodeT;

use super::*;

/// A table of the test's own with one row, a connection holding that row in an open
/// transaction, and another waiting on it in the background.
struct HeldRow {
    runtime: tokio::runtime::Runtime,
    admin: Box<dyn dbrover_core::Connection>,
    holder: Option<Box<dyn dbrover_core::Connection>>,
    waiter: Option<tokio::task::JoinHandle<()>>,
    table: String,
}

impl HeldRow {
    fn start(server: &PostgresTestServer, test: &str) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let table = format!("w15_app_{test}_{}", std::process::id());
        let mut admin = server.open(&runtime, &server.database);
        for sql in [
            format!("DROP TABLE IF EXISTS {table}"),
            format!("CREATE TABLE {table} (id int PRIMARY KEY, n int NOT NULL)"),
            format!("INSERT INTO {table} VALUES (1, 0)"),
        ] {
            first_value(&runtime, &mut admin, &sql);
        }
        let mut holder = server.open(&runtime, &server.database);
        first_value(&runtime, &mut holder, "BEGIN");
        first_value(
            &runtime,
            &mut holder,
            &format!("UPDATE {table} SET n = 1 WHERE id = 1"),
        );
        let mut waiter = server.open(&runtime, &server.database);
        let wait = format!("UPDATE {table} SET n = 2 WHERE id = 1");
        let waiter = runtime.spawn(async move {
            // Fails, freeing the test, if nothing ever ends the holder.
            if let Ok(cursor) = waiter.execute("SET lock_timeout = '30s'").await {
                let _ = cursor.finish().await;
            }
            if let Ok(cursor) = waiter.execute(&wait).await {
                let _ = cursor.finish().await;
            }
        });
        Self {
            runtime,
            admin,
            holder: Some(holder),
            waiter: Some(waiter),
            table,
        }
    }

    /// The holder's pid, as the list shows it.
    fn holder_pid(&mut self) -> String {
        let holder = self.holder.as_mut().unwrap();
        first_value(&self.runtime, holder, "SELECT pg_backend_pid()").unwrap()
    }
}

impl Drop for HeldRow {
    fn drop(&mut self) {
        if let Some(mut holder) = self.holder.take() {
            let _ = self.runtime.block_on(async {
                if let Ok(cursor) = holder.execute("ROLLBACK").await {
                    let _ = cursor.finish().await;
                }
                holder.close().await
            });
        }
        if let Some(waiter) = self.waiter.take() {
            let _ = self.runtime.block_on(waiter);
        }
        let table = self.table.clone();
        first_value(
            &self.runtime,
            &mut self.admin,
            &format!("DROP TABLE {table}"),
        );
    }
}

fn start(
    server: &PostgresTestServer,
    safety: SafetyLevel,
    dir: &Path,
) -> Harness<'static, DbRoverApp> {
    let mut source = server.data_source(&server.database, "production");
    source.safety = safety;
    DataSourceFile::new(dir.join("data_sources.toml"))
        .save(std::slice::from_ref(&source))
        .unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = crate::test_support::start_app(
        harness_at_default_size(),
        dir,
        feed,
        server.secrets(&source),
    );
    step_until_visible(&mut harness, "production");
    harness.get_by_label("production").click_secondary();
    harness.run_steps(2);
    harness.get_by_label("Server activity").click();
    step_until_visible(&mut harness, "Read at");
    harness
}

#[test]
fn kills_the_session_holding_a_lock_after_asking_on_a_careful_data_source() {
    let Some(server) = PostgresTestServer::from_env() else {
        return;
    };
    let mut held = HeldRow::start(&server, "kill");
    let pid = held.holder_pid();
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start(&server, SafetyLevel::Careful, dir.path());

    // Other tests' sessions on the same server may be listed too, so this test's own are
    // found by the holder's pid.
    step_until(&mut harness, "the waiter listed", |harness| {
        harness.query_all_by_label("waiting").next().is_some()
    });
    step_until_visible(&mut harness, &pid);
    harness.get_by_label(&pid).click();
    step_until_visible(&mut harness, &format!("Session {pid}"));
    harness.get_by_label("Kill\u{2026}").click();
    step_until_visible(
        &mut harness,
        &format!("Kill session {pid} on \u{201c}production\u{201d}?"),
    );
    // A modal is measured on its first frame and placed on the next.
    harness.run_steps(2);
    harness.get_by_label("Kill").click();
    step_until_visible(&mut harness, &format!("Ended session {pid}."));
    step_until(&mut harness, "the holder's row going", |harness| {
        harness.query_all_by_label(&pid).next().is_none()
    });
    held.holder.take();
}

#[test]
fn a_read_only_data_source_shows_sessions_but_cannot_stop_them() {
    let Some(server) = PostgresTestServer::from_env() else {
        return;
    };
    let mut held = HeldRow::start(&server, "read_only");
    let pid = held.holder_pid();
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start(&server, SafetyLevel::ReadOnly, dir.path());

    step_until_visible(&mut harness, &pid);
    harness.get_by_label_contains("is read-only: its sessions are shown");
    harness.get_by_label(&pid).click();
    step_until_visible(&mut harness, &format!("Session {pid}"));
    for button in ["Cancel query", "Kill\u{2026}"] {
        assert!(
            harness.get_by_label(button).accesskit_node().is_disabled(),
            "{button} is disabled"
        );
    }
}
