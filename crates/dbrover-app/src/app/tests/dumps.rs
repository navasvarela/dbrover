//! Dump and restore (W17) through the whole app, on the PostgreSQL test server: right-click a
//! connected data source → **Dump…**, the program DbRover chose shown and the dump running in
//! the status bar; **Restore…** disabled on a read-only data source; a careful one's existing
//! database restored into only once its name is typed; and a `pg_dump` too old for the server
//! refused with how to install one.
//!
//! The tests that run programs skip unless `DBROVER_TEST_CLIENTS` names a folder holding them,
//! as the engine's dump tests do.

use std::path::PathBuf;

use dbrover_core::SafetyLevel;
use dbrover_dump::Program;
use dbrover_store::{TrustedCommand, TrustedCommands};
use egui_kittest::kittest::NodeT;

use super::*;

/// The folder `DBROVER_TEST_CLIENTS` names, or `None`, with a note, so the test skips.
fn clients() -> Option<PathBuf> {
    let Ok(folder) = std::env::var("DBROVER_TEST_CLIENTS") else {
        eprintln!("skipping: DBROVER_TEST_CLIENTS is not set");
        return None;
    };
    let folder = PathBuf::from(folder);
    Some(if folder.is_absolute() {
        folder
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(folder)
    })
}

/// `[dumps.programs]` naming `programs`, each trusted as if typed in Settings › Dumps.
fn set_programs(dir: &Path, programs: &[(Program, PathBuf)]) {
    let lines: String = programs
        .iter()
        .map(|(program, path)| {
            format!(
                "{} = {:?}\n",
                program.setting_key(),
                path.display().to_string()
            )
        })
        .collect();
    std::fs::write(
        dir.join("settings.toml"),
        format!("[dumps.programs]\n{lines}"),
    )
    .unwrap();
    let mut trusted = TrustedCommands::default();
    for (program, path) in programs {
        trusted.trust(TrustedCommand::dump_program(program.setting_key(), path));
    }
    TrustedCommandsFile::new(dir.join("trusted_commands.toml"))
        .save(&trusted)
        .unwrap();
}

/// The app with the test server saved as `production` at `safety`, connected.
fn start_connected(
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
    harness.get_by_label("Connect").click();
    step_until(&mut harness, "the data source connecting", |harness| {
        harness
            .state()
            .state
            .data_source(source.id)
            .is_some_and(|entry| entry.dump_engine().is_some())
    });
    harness
}

/// Clicks `label` as a pointer does: resting on it for a frame first. Pressed the frame it
/// arrives, a press lands on what was under the pointer the frame before.
fn click(harness: &mut Harness<'_, DbRoverApp>, label: &str) {
    harness.get_by_label(label).hover();
    harness.run_steps(2);
    harness.get_by_label(label).click();
    harness.run_steps(2);
}

fn open_menu_item(harness: &mut Harness<'_, DbRoverApp>, item: &str) {
    harness.get_by_label("production").click_secondary();
    harness.run_steps(2);
    harness.get_by_label(item).click();
    // A modal is measured on its first frame and placed on the next.
    harness.run_steps(2);
}

#[test]
fn dumps_from_the_menu_with_the_program_it_names() {
    let (Some(server), Some(clients)) = (PostgresTestServer::from_env(), clients()) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    set_programs(dir.path(), &[(Program::PgDump, clients.join("pg_dump"))]);
    let mut harness = start_connected(&server, SafetyLevel::Normal, dir.path());
    let file = dir.path().join("production.dump");
    let picked = file.clone();
    harness.state_mut().pick_dump_file = Box::new(move |_| Some(picked.clone()));

    open_menu_item(&mut harness, "Dump\u{2026}");
    step_until_visible(&mut harness, "Dump from production");
    step_until(&mut harness, "the program chosen", |harness| {
        harness
            .query_by_label_contains("for a server of version")
            .is_some()
    });
    click(&mut harness, "Browse…");
    // A pointer rests on a button before it presses it; pressed the frame it arrives, the
    // press lands on what was under it the frame before.
    harness.get_by_role_and_label(Role::Button, "Dump").hover();
    harness.run_steps(2);
    harness.get_by_role_and_label(Role::Button, "Dump").click();
    step_until(&mut harness, "the dump ending", |harness| {
        harness
            .query_by_label_contains("Dumped dbrover from production")
            .is_some()
    });
    assert!(file.is_file());
}

#[test]
fn a_read_only_data_source_cannot_be_restored_into() {
    let Some(server) = PostgresTestServer::from_env() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_connected(&server, SafetyLevel::ReadOnly, dir.path());
    harness.get_by_label("production").click_secondary();
    harness.run_steps(2);
    assert!(
        !harness
            .get_by_label("Dump\u{2026}")
            .accesskit_node()
            .is_disabled()
    );
    assert!(
        harness
            .get_by_label("Restore\u{2026}")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn a_careful_data_source_asks_for_the_database_name_before_restoring_into_it() {
    let (Some(server), Some(clients)) = (PostgresTestServer::from_env(), clients()) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    set_programs(dir.path(), &[(Program::Psql, clients.join("psql"))]);
    let dump = dir.path().join("shop.sql");
    std::fs::write(&dump, "--\n-- PostgreSQL database dump\n--\nSELECT 1;\n").unwrap();
    let mut harness = start_connected(&server, SafetyLevel::Careful, dir.path());
    harness.state_mut().pick_restore_file = Box::new(move || Some(dump.clone()));

    open_menu_item(&mut harness, "Restore\u{2026}");
    step_until_visible(&mut harness, "Restore into production");
    step_until(&mut harness, "the program chosen", |harness| {
        harness
            .query_by_label_contains("for a server of version")
            .is_some()
    });
    click(&mut harness, "An existing database");
    click(&mut harness, "Restore…");
    let database = server.database.clone();
    step_until_visible(
        &mut harness,
        &format!("Type {database} to restore into it."),
    );
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Restore")
            .accesskit_node()
            .is_disabled()
    );
    harness
        .get_by_role_and_label(Role::TextInput, "Database name")
        .focus();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, "Database name")
        .type_text(&database);
    harness.run_steps(2);
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Restore")
            .accesskit_node()
            .is_disabled()
    );
    // Not clicked: it would restore into the server every test shares.
    click(&mut harness, "Back");
    harness.get_by_label("Cancel").click();
    step_until(&mut harness, "the dialog closing", |harness| {
        harness.query_by_label("Restore into production").is_none()
    });
}

#[cfg(unix)]
#[test]
fn a_pg_dump_too_old_for_the_server_says_how_to_install_one() {
    use std::os::unix::fs::PermissionsExt;

    let Some(server) = PostgresTestServer::from_env() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("pg_dump");
    std::fs::write(&old, "#!/bin/sh\necho 'pg_dump (PostgreSQL) 9.6.0'\n").unwrap();
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
    set_programs(dir.path(), &[(Program::PgDump, old)]);
    let mut harness = start_connected(&server, SafetyLevel::Normal, dir.path());

    open_menu_item(&mut harness, "Dump\u{2026}");
    step_until(&mut harness, "the program refused", |harness| {
        harness
            .query_by_label_contains("pg_dump 9.6 can't dump PostgreSQL")
            .is_some()
    });
    assert!(harness.query_by_label_contains("client tools").is_some());
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Dump")
            .accesskit_node()
            .is_disabled()
    );
}
