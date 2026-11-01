//! The history as an audit trail, through the engine on SQLite: where each statement came
//! from, a grid edit's change, no secrets in the file, and retention.

use dbrover_core::TableName;
use dbrover_engine::RowsImport;
use dbrover_export::ExportSpec;
use dbrover_store::{HistoryEntry, HistorySource, NamedCell, RowChange, RowChangeKind, Separator};

use super::*;

/// Every entry of the data source, newest first.
fn history_of(engine: &mut TestEngine, data_source: DataSourceId) -> Vec<HistoryEntry> {
    engine.send(Command::SearchHistory(HistoryQuery::all(data_source)));
    loop {
        match next_answer(engine) {
            Event::HistoryFound {
                result: Ok(entries),
                ..
            } => return entries,
            Event::TransactionChanged { .. } => {}
            other => panic!("expected the history, got {other:?}"),
        }
    }
}

fn apply_ddl(engine: &mut TestEngine, data_source: DataSourceId, sql: &str) {
    engine.send(Command::ApplyDdl {
        data_source,
        statements: vec![sql.to_owned()],
    });
    loop {
        match next_answer(engine) {
            Event::DdlApplied { result, .. } => return result.unwrap(),
            Event::TransactionChanged { .. } => {}
            other => panic!("expected DdlApplied, got {other:?}"),
        }
    }
}

/// A console run, an object helper's DDL, a grid edit, an export of every row and a CSV import,
/// each recorded with where it came from; the import once, without its rows.
#[test]
fn records_where_each_statement_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());
    execute(&engine, id, 1, &["SELECT 1"]);
    finish_run(&mut engine, 1);
    apply_ddl(
        &mut engine,
        id,
        "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT)",
    );
    let added = RowChange {
        table: "main.people".to_owned(),
        kind: RowChangeKind::Inserted {
            values: vec![NamedCell {
                column: "name".to_owned(),
                value: Some("ada".to_owned()),
            }],
        },
        revert: None,
    };
    engine.send(Command::ApplyEdits {
        console: CONSOLE,
        statements: vec![RowEdit {
            sql: "INSERT INTO people (id, name) VALUES ('1', 'ada')".to_owned(),
            change: Some(added.clone()),
        }],
    });
    assert_eq!(expect_edits(&mut engine), Ok(()));
    let exported = dir.path().join("people.csv");
    let (_, written) = grid::export_all(
        &mut engine,
        2,
        "SELECT id, name FROM people",
        (&exported, ExportSpec::Csv(Default::default())),
    );
    assert_eq!(written, Ok(1));
    let imported = dir.path().join("more.csv");
    std::fs::write(&imported, "2,bob\n3,cy\n").unwrap();
    let (_, result) = grid::import_rows(
        &mut engine,
        id,
        RowsImport {
            table: TableName {
                schema: "main".to_owned(),
                name: "people".to_owned(),
            },
            file: imported.clone(),
            separator: Separator::Comma,
            header: false,
            mapping: vec![("id".to_owned(), 0), ("name".to_owned(), 1)],
        },
    );
    assert_eq!(result, Ok(2));

    let found: Vec<(HistorySource, String, Option<u64>)> = history_of(&mut engine, id)
        .into_iter()
        .map(|entry| (entry.source, entry.sql, entry.rows))
        .collect();
    assert_eq!(
        found,
        [
            (
                HistorySource::Import { file: imported },
                "INSERT INTO \"main\".\"people\" (\"id\", \"name\") VALUES\n\u{2026}".to_owned(),
                Some(2)
            ),
            (
                HistorySource::Export,
                "SELECT id, name FROM people".to_owned(),
                Some(1)
            ),
            (
                HistorySource::GridEdit(Some(added)),
                "INSERT INTO people (id, name) VALUES ('1', 'ada')".to_owned(),
                Some(1)
            ),
            (
                HistorySource::ObjectHelper,
                "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT)".to_owned(),
                None
            ),
            (HistorySource::Console, "SELECT 1".to_owned(), None),
        ]
    );
    engine.shut_down();
}

/// No secret reaches the history file, whichever way the statement was sent: a console run
/// and an object helper's DDL (SQLite refuses `CREATE USER`, which is still recorded).
#[test]
fn keeps_secrets_out_of_the_history_file() {
    const SECRET: &str = "Hunter2Secret";
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());
    let conninfo = format!("SELECT 'host=db user=app password={SECRET}' AS conninfo");
    execute(&engine, id, 1, &[&conninfo]);
    finish_run(&mut engine, 1);
    engine.send(Command::ApplyDdl {
        data_source: id,
        statements: vec![format!("CREATE USER app WITH PASSWORD '{SECRET}'")],
    });
    loop {
        match next_answer(&mut engine) {
            Event::DdlApplied { result, .. } => break assert!(result.is_err()),
            Event::TransactionChanged { .. } => {}
            other => panic!("expected DdlApplied, got {other:?}"),
        }
    }

    let recorded: Vec<String> = history_of(&mut engine, id)
        .into_iter()
        .map(|entry| entry.sql)
        .collect();
    assert_eq!(
        recorded,
        [
            "CREATE USER app WITH PASSWORD '<redacted>'",
            "SELECT 'host=db user=app password=<redacted>' AS conninfo",
        ]
    );
    engine.shut_down();
    for name in ["history.sqlite", "history.sqlite-wal"] {
        let Ok(bytes) = std::fs::read(dir.path().join(name)) else {
            continue;
        };
        assert!(
            !bytes
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes()),
            "{name} holds the secret"
        );
    }
}

/// Statements older than the retention go at start-up, and saving a tighter setting prunes at
/// once.
#[test]
fn prunes_at_start_up_and_when_the_retention_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let id = DataSourceId::generate();
    let history = dbrover_store::HistoryFile::new(dir.path().join("history.sqlite"))
        .open()
        .unwrap();
    let now = jiff::Timestamp::now();
    for (days_ago, sql) in [
        (40, "SELECT 'old'"),
        (2, "SELECT 'recent'"),
        (1, "SELECT 'newest'"),
    ] {
        let executed_at = now
            .checked_sub(jiff::SignedDuration::from_hours(24 * days_ago))
            .unwrap();
        history
            .record(&HistoryEntry {
                data_source: id,
                executed_at,
                sql: sql.to_owned(),
                duration: None,
                rows: None,
                outcome: HistoryOutcome::Ok,
                source: HistorySource::Console,
            })
            .unwrap();
    }
    drop(history);
    std::fs::write(
        dir.path().join("settings.toml"),
        "[history]\nmax_age_days = 30\n",
    )
    .unwrap();

    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    let mut settings = dbrover_store::Settings::default();
    settings.history.max_age_days = std::num::NonZeroU32::new(30);
    engine.expect(Event::Started);
    engine.expect_keymap(Ok(dbrover_store::KeymapBindings::new()));
    engine.expect_settings(Ok(settings.clone()));
    engine.expect_no_custom_files();
    engine.expect_nothing_trusted();
    engine.expect(Event::ProjectsLoaded(dbrover_store::Projects::default()));
    engine.expect(Event::DataSourcesLoaded(vec![]));
    engine.expect(Event::ConsolesLoaded(vec![]));
    engine.expect(Event::DiagramsLoaded(vec![]));
    let sqls = |engine: &mut TestEngine| -> Vec<String> {
        history_of(engine, id)
            .into_iter()
            .map(|entry| entry.sql)
            .collect()
    };
    assert_eq!(sqls(&mut engine), ["SELECT 'newest'", "SELECT 'recent'"]);

    settings.history.max_per_data_source = std::num::NonZeroU32::new(1);
    engine.send(Command::SaveSettings(settings.clone()));
    match next_answer(&mut engine) {
        Event::SettingsLoaded { result, .. } => assert_eq!(result, Ok(settings)),
        other => panic!("expected the settings, got {other:?}"),
    }
    assert_eq!(sqls(&mut engine), ["SELECT 'newest'"]);
    engine.shut_down();
}

/// A search written to a file, after the statements the console just ran.
#[test]
fn exports_a_search_to_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let (mut engine, id) = engine_with_sqlite(dir.path());
    execute(&engine, id, 1, &["SELECT 1", "SELECT 2"]);
    finish_run(&mut engine, 1);
    let path = dir.path().join("history.csv");
    engine.send(Command::ExportHistory {
        query: HistoryQuery {
            text: "2".to_owned(),
            ..HistoryQuery::all(id)
        },
        path: path.clone(),
        spec: ExportSpec::Csv(Default::default()),
        names: std::collections::HashMap::from([(id, "shop".to_owned())]),
    });
    loop {
        match next_answer(&mut engine) {
            Event::HistoryExported {
                path: written,
                result,
            } => {
                assert_eq!((written, result), (path.clone(), Ok(1)));
                break;
            }
            Event::TransactionChanged { .. } => {}
            other => panic!("expected the export, got {other:?}"),
        }
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        lines[1].contains(",shop,")
            && lines[1].contains(",console,")
            && lines[1].ends_with(",SELECT 2,"),
        "{text}"
    );
    engine.shut_down();
}
