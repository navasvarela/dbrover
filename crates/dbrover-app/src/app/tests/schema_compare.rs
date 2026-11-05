//! Schema compare (W13) through the whole app, on the PostgreSQL test server: **Compare
//! schema…** on a schema, the right side picked, the differences listed and the script opened
//! in a console. Running the script and comparing again is the engine's exit test
//! (`dbrover-engine/tests/ddl/schema_compare.rs`).

use super::*;

/// A database of the test's own, with two schemas that differ by a column and an index,
/// dropped when the test ends.
struct CompareDatabase<'a> {
    runtime: &'a tokio::runtime::Runtime,
    admin: Box<dyn dbrover_core::Connection>,
    name: String,
}

impl<'a> CompareDatabase<'a> {
    /// `test` tells the database apart from other tests' in the same run.
    fn create(
        server: &PostgresTestServer,
        runtime: &'a tokio::runtime::Runtime,
        test: &str,
    ) -> Self {
        let name = format!("dbrover_w13_{test}_{}", std::process::id());
        let mut admin = server.open(runtime, &server.database);
        first_value(
            runtime,
            &mut admin,
            &format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
        );
        first_value(runtime, &mut admin, &format!("CREATE DATABASE {name}"));
        let mut shop = server.open(runtime, &name);
        for schema in ["dev", "prod"] {
            first_value(runtime, &mut shop, &format!("CREATE SCHEMA {schema}"));
            first_value(
                runtime,
                &mut shop,
                &format!(
                    "CREATE TABLE {schema}.orders (id serial PRIMARY KEY, total numeric(12,2))"
                ),
            );
        }
        first_value(
            runtime,
            &mut shop,
            "ALTER TABLE prod.orders ADD COLUMN note text",
        );
        first_value(
            runtime,
            &mut shop,
            "CREATE INDEX orders_total_idx ON prod.orders (total)",
        );
        let _ = runtime.block_on(shop.close());
        Self {
            runtime,
            admin,
            name,
        }
    }
}

impl Drop for CompareDatabase<'_> {
    fn drop(&mut self) {
        let drop = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name);
        first_value(self.runtime, &mut self.admin, &drop);
    }
}

#[test]
fn compares_two_postgres_schemas_and_opens_the_script() {
    let Some(server) = PostgresTestServer::from_env() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let database = CompareDatabase::create(&server, &runtime, "script");
    let dir = tempfile::tempdir().unwrap();
    let source = server.data_source(&database.name, "pg");
    DataSourceFile::new(dir.path().join("data_sources.toml"))
        .save(std::slice::from_ref(&source))
        .unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = crate::test_support::start_app(
        harness_at_default_size(),
        dir.path(),
        feed,
        server.secrets(&source),
    );
    for (expand, appears) in [
        ("Expand pg", format!("Expand {}", database.name)),
        (
            &*format!("Expand {}", database.name),
            "Expand dev".to_owned(),
        ),
    ] {
        step_until_visible(&mut harness, expand);
        harness.get_by_label(expand).click();
        step_until_visible(&mut harness, &appears);
    }

    node_menu(&mut harness, "dev", "Compare schema\u{2026}");
    step_until_visible(&mut harness, "Compare");
    harness
        .get_by(|node| {
            node.role() == Role::ComboBox && node.value().as_deref() == Some("Pick a data source")
        })
        .click();
    harness.run_steps(2);
    harness.get_all_by_label("pg").last().unwrap().click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, "Right")
        .focus();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, "Right")
        .type_text("prod");
    harness.run_steps(2);
    harness.get_by_label("Compare").click();
    step_until_visible(&mut harness, "2 differences");
    harness.get_by_label("~ orders");
    harness.get_by_label("+ orders_total_idx");

    harness.get_by_label("Open script in console").click();
    step_until(&mut harness, "a console with the script", |harness| {
        harness.state().consoles.open_consoles().any(|console| {
            console
                .text()
                .contains("ALTER TABLE dev.orders ADD COLUMN note text;")
        })
    });
}

/// VC3 in the app: **Save schema snapshot…** on `dev` writes its snapshot, and a compare tab
/// with that file as its left side lists what `prod` has that it doesn't.
#[test]
fn saves_a_snapshot_and_compares_it_with_a_live_schema() {
    let Some(server) = PostgresTestServer::from_env() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let database = CompareDatabase::create(&server, &runtime, "snapshot");
    let dir = tempfile::tempdir().unwrap();
    let source = server.data_source(&database.name, "pg");
    DataSourceFile::new(dir.path().join("data_sources.toml"))
        .save(std::slice::from_ref(&source))
        .unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = crate::test_support::start_app(
        harness_at_default_size(),
        dir.path(),
        feed,
        server.secrets(&source),
    );
    let snapshot = dir.path().join("dev.schema.toml");
    let picked = snapshot.clone();
    harness.state_mut().pick_schema_snapshot_path = Box::new(move |schema| {
        assert_eq!(schema, "dev");
        Some(picked.clone())
    });
    let picked = snapshot.clone();
    harness.state_mut().pick_schema_snapshot = Box::new(move || Some(picked.clone()));
    for (expand, appears) in [
        ("Expand pg", format!("Expand {}", database.name)),
        (
            &*format!("Expand {}", database.name),
            "Expand dev".to_owned(),
        ),
    ] {
        step_until_visible(&mut harness, expand);
        harness.get_by_label(expand).click();
        step_until_visible(&mut harness, &appears);
    }

    node_menu(&mut harness, "dev", "Save schema snapshot\u{2026}");
    step_until_visible(
        &mut harness,
        &format!("Schema snapshot saved to {}", snapshot.display()),
    );
    let saved = std::fs::read_to_string(&snapshot).unwrap();
    assert!(saved.contains("[tables.orders]\n"), "{saved}");

    node_menu(&mut harness, "prod", "Compare schema\u{2026}");
    step_until_visible(&mut harness, "Compare");
    harness
        .get_by(|node| node.role() == Role::ComboBox && node.value().as_deref() == Some("pg"))
        .click();
    harness.run_steps(2);
    harness.get_by_label("Snapshot file\u{2026}").click();
    harness.run_steps(2);
    step_until_visible(&mut harness, "dev.schema.toml");
    harness
        .get_by(|node| {
            node.role() == Role::ComboBox && node.value().as_deref() == Some("Pick a data source")
        })
        .click();
    harness.run_steps(2);
    harness.get_all_by_label("pg").last().unwrap().click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, "Right")
        .focus();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, "Right")
        .type_text("prod");
    harness.run_steps(2);
    harness.get_by_label("Compare").click();
    step_until_visible(&mut harness, "2 differences");
    harness.get_by_label("~ orders");
    harness.get_by_label("+ orders_total_idx");
}
