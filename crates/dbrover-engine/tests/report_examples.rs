//! The example reports in `examples/reports/`, run on the ledger `ledger.sql` makes, with their
//! CSV files compared with the ones checked in beside them, so the manual's examples keep
//! working. Set `DBROVER_UPDATE_EXPECTED=1` to write the checked-in files from this run instead,
//! after a change that is meant to change them.

// Helpers panic on failure like assertions do; clippy's test exemption covers only `#[test]`
// functions themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{MemorySecrets, sqlite_data_source};
use dbrover_core::DataSource;
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels};
use dbrover_report::{Plan, RunFolders, read_report, run};
use dbrover_store::{SecretStore, TrustedCommands};

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/reports")
}

/// The data source `ledger`, a SQLite file in `folder` made by `ledger.sql`.
fn ledger(folder: &Path) -> DataSource {
    let path = folder.join("ledger.db");
    let script = std::fs::read_to_string(examples().join("ledger.sql")).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    DataSource {
        name: "ledger".to_owned(),
        ..sqlite_data_source(path)
    }
}

/// Runs the example `name` for October 2026 into `out`.
async fn run_example(name: &str, ledger: &DataSource, out: &Path) {
    let text = std::fs::read_to_string(examples().join(format!("{name}.toml"))).unwrap();
    let plan = Plan::new(
        read_report(&text).unwrap(),
        std::slice::from_ref(ledger),
        &[],
    )
    .unwrap();
    let values = plan
        .values(
            &[("since", "2026-10-01"), ("until", "2026-11-01")]
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        )
        .unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
    let tunnels = Tunnels::new(out.join("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let access = SourceAccess {
        secrets: &secrets,
        tunnels: &tunnels,
        commands: &commands,
    };
    let staging = out.join(".run");
    let folders = RunFolders {
        folder: out,
        staging: &staging,
    };
    run(&plan, &values, &access, folders, &mut |_| {})
        .await
        .unwrap();
}

/// Whether `file` in `out` is the one checked in under `expected/`.
fn compare(out: &Path, file: &str) {
    let written = std::fs::read_to_string(out.join(file)).unwrap();
    let expected_path = examples().join("expected").join(file);
    if std::env::var_os("DBROVER_UPDATE_EXPECTED").is_some() {
        std::fs::write(&expected_path, &written).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&expected_path).unwrap();
    // Git may have checked the file out with the platform's line endings; the writer's are
    // CSV's own, CRLF.
    let expected = expected.replace("\r\n", "\n").replace('\n', "\r\n");
    assert!(
        written == expected,
        "{file} differs from {}; run with DBROVER_UPDATE_EXPECTED=1 if that is meant",
        expected_path.display()
    );
}

#[tokio::test]
async fn the_trial_balance_example_matches_its_checked_in_file() {
    let folder = tempfile::tempdir().unwrap();
    let ledger = ledger(folder.path());
    run_example("trial-balance", &ledger, folder.path()).await;
    compare(folder.path(), "trial-balance-2026-10.csv");
    assert!(folder.path().join("trial-balance-2026-10.xlsx").is_file());
}

#[tokio::test]
async fn the_general_ledger_example_matches_its_checked_in_file() {
    let folder = tempfile::tempdir().unwrap();
    let ledger = ledger(folder.path());
    run_example("general-ledger", &ledger, folder.path()).await;
    compare(folder.path(), "general-ledger-2026-10.csv");
    assert!(folder.path().join("general-ledger-2026-10.xlsx").is_file());
}
