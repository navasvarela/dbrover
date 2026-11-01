//! Programs from `exports.toml` (W8d), run for real: `dbrover-test-program`, built from this
//! crate, is the program, so the tests behave the same on Linux, macOS and Windows. The rows
//! come from SQLite, which needs no server.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{TestEngine, sqlite_data_source, sqlite_database};
use dbrover_core::{CommandTemplate, DataSource};
use dbrover_engine::{
    Command, ConsoleId, Event, ExportAll, ExportProgramRequest, ProgramFailure, ProgramId, RunId,
    TableExport,
};
use dbrover_export::{CustomFormatId, ExportSpec, ProgramFormat, ProgramSpec, SqlTable};
use dbrover_store::TrustedCommand;

const PROGRAM: &str = env!("CARGO_BIN_EXE_dbrover-test-program");
const CONSOLE: ConsoleId = ConsoleId(1);

/// More rows than a page, so the program gets them over several writes.
const ROWS: usize = 1_200;

struct Exports {
    engine: TestEngine,
    data_source: DataSource,
    dir: tempfile::TempDir,
}

impl Exports {
    /// An engine with one SQLite data source, whose table `t` is empty.
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
        engine.expect_started(vec![]);
        let data_source = sqlite_data_source(sqlite_database(dir.path()));
        engine.send(Command::SaveDataSource {
            data_source: data_source.clone(),
            password: dbrover_engine::PasswordUpdate::Keep,
            ssh_secret: dbrover_engine::PasswordUpdate::Keep,
        });
        engine.expect(Event::DataSourceSaved(data_source.clone()));
        Self {
            engine,
            data_source,
            dir,
        }
    }

    fn trust(&mut self, spec: &ProgramSpec) {
        let trusted = TrustedCommand::export(&spec.id, &spec.program);
        self.engine.send(Command::TrustCommand(trusted.clone()));
        assert!(matches!(
            self.engine.next_event(),
            Event::TrustedCommandsLoaded(loaded) if loaded.contains(&trusted)
        ));
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Runs `sql` straight into `path` through `spec`'s program; how it ended.
    fn run_to_file(&mut self, sql: &str, path: &Path, spec: ProgramSpec) -> Result<usize, String> {
        self.engine.send(Command::RunToFile {
            console: CONSOLE,
            data_source: self.data_source.id,
            export: ExportAll {
                run: RunId(1),
                sql: sql.to_owned(),
                parameters: dbrover_sql::ParameterValues::default(),
                path: path.to_owned(),
                spec: ExportSpec::Program(Box::new(spec)),
            },
        });
        self.export_finished()
    }

    /// How the export ended, past the sessions' and the export's progress on the way.
    fn export_finished(&mut self) -> Result<usize, String> {
        loop {
            if let Event::ExportFinished { result, .. } = self.engine.next_event() {
                return result;
            }
        }
    }
}

fn spec(arguments: &[&str], timeout: Duration) -> ProgramSpec {
    let command: Vec<String> = std::iter::once(PROGRAM)
        .chain(arguments.iter().copied())
        .map(str::to_owned)
        .collect();
    ProgramSpec {
        id: CustomFormatId::new("rows").unwrap(),
        program: Arc::new(ProgramFormat {
            command: CommandTemplate::parse(&command).unwrap(),
            timeout,
            text: true,
        }),
        table: None,
        prepared: None,
    }
}

/// `count` rows of an id and a label `width` characters wide.
fn numbers(count: usize, width: usize) -> String {
    format!(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {count}) \
         SELECT i, printf('%0{width}d', i) AS label FROM n"
    )
}

/// What the file holds is what the program wrote, after the rows as JSON Lines.
#[test]
fn writes_every_row_through_a_trusted_program() {
    let mut exports = Exports::start();
    let spec = spec(&["copy"], Duration::from_secs(60));
    exports.trust(&spec);
    let path = exports.path("rows.jsonl");

    let result = exports.run_to_file(&numbers(ROWS, 4), &path, spec);

    assert_eq!(result, Ok(ROWS));
    let written = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = written.lines().collect();
    assert_eq!(lines.len(), ROWS);
    assert_eq!(lines[0], r#"{"i":"1","label":"0001"}"#);
    assert_eq!(lines[ROWS - 1], r#"{"i":"1200","label":"1200"}"#);
}

/// A table's export fills in `{table}` and the context, and the program gets every row.
#[test]
fn a_program_gets_its_arguments_and_context() {
    let mut exports = Exports::start();
    let mut spec = spec(
        &["dump", "{table}", "{data_source}"],
        Duration::from_secs(60),
    );
    spec.id = CustomFormatId::new("dump").unwrap();
    // As the app gives it, from the rows' source.
    let table = SqlTable {
        schema: None,
        name: "t".to_owned(),
    };
    spec.table = Some(table.clone());
    exports.trust(&spec);
    let path = exports.path("t.json");
    exports.engine.send(Command::ExportTable {
        data_source: exports.data_source.id,
        export: TableExport {
            run: RunId(1),
            table,
            path: path.clone(),
            spec: ExportSpec::Program(Box::new(spec)),
        },
    });
    assert_eq!(exports.export_finished(), Ok(0));
    let dumped: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(dumped["arguments"], serde_json::json!(["t", "app"]));
    let environment = &dumped["environment"];
    for (name, value) in [
        ("DBROVER_EXPORT", "dump"),
        ("DBROVER_FORMAT", "jsonl"),
        ("DBROVER_TABLE", "t"),
        ("DBROVER_DATA_SOURCE", "app"),
        ("DBROVER_DRIVER", "sqlite"),
    ] {
        assert_eq!(environment[name], value, "{name}");
    }
    assert_eq!(dumped["stdin"], "");
}

#[test]
fn runs_only_a_program_trusted_as_it_is_now() {
    let mut exports = Exports::start();
    let path = exports.path("rows.jsonl");
    let result = exports.run_to_file(
        &numbers(3, 1),
        &path,
        spec(&["copy"], Duration::from_secs(60)),
    );
    assert_eq!(result, Err(ProgramFailure::NotTrusted.to_string()));
    assert!(!path.exists());

    exports.trust(&spec(&["copy"], Duration::from_secs(60)));
    let changed = spec(&["copy", "--more"], Duration::from_secs(60));
    let result = exports.run_to_file(&numbers(3, 1), &path, changed);
    assert_eq!(result, Err(ProgramFailure::NotTrusted.to_string()));
}

/// A program that fails leaves no file, half-written or otherwise, and says why.
#[test]
fn a_failing_program_says_why_and_leaves_no_file() {
    let mut exports = Exports::start();
    let spec = spec(&["fail"], Duration::from_secs(60));
    exports.trust(&spec);
    let path = exports.path("rows.pdf");

    let error = exports
        .run_to_file(&numbers(ROWS, 4), &path, spec)
        .unwrap_err();

    // `exit status: 3` on Unix, `exit code: 3` on Windows.
    assert!(error.contains(": 3)"), "{error}");
    assert!(error.ends_with("it went wrong"), "{error}");
    let left: Vec<String> = std::fs::read_dir(exports.dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("rows.pdf"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

/// A program that stops taking rows is stopped once its timeout passes without progress.
#[test]
fn a_program_that_takes_no_more_rows_is_stopped() {
    let mut exports = Exports::start();
    let spec = spec(&["wait"], Duration::from_secs(1));
    exports.trust(&spec);
    let path = exports.path("rows.jsonl");
    let started = Instant::now();

    // Wide rows, more than a pipe's buffer holds, so writing them waits for the program.
    let error = exports
        .run_to_file(&numbers(ROWS, 200), &path, spec)
        .unwrap_err();

    assert!(error.contains("took no more rows for 1s"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(!path.exists());
}

/// Copy as runs the program on the selected cells, its output kept to put on the clipboard.
#[test]
fn copies_the_selected_cells_through_a_program() {
    let mut exports = Exports::start();
    let spec = spec(&["copy"], Duration::from_secs(60));
    exports.trust(&spec);
    exports
        .engine
        .send(Command::RunExportProgram(Box::new(ExportProgramRequest {
            program: ProgramId(7),
            data_source: exports.data_source.id,
            spec,
            stdin: "{\"id\":\"1\"}\n".to_owned(),
        })));
    match exports.engine.next_event() {
        Event::ProgramFinished {
            program: ProgramId(7),
            result: Ok(output),
        } => assert_eq!(output.stdout, "{\"id\":\"1\"}\n"),
        other => panic!("expected the copy to finish, got {other:?}"),
    }
}
