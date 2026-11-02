//! Command actions' programs (H2), run for real: `dbrover-test-program`, built from this crate,
//! is the program, so the tests behave the same on Linux, macOS and Windows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{MemorySecrets, TestEngine, closing_server};
use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, Password, PostgresConfig, SelectedCells,
    ServerConfig, TemplateValues, TlsMode,
};
use dbrover_engine::{
    Command, Event, PasswordUpdate, ProgramFailure, ProgramId, ProgramOutput, ProgramRequest,
};
use dbrover_store::{ActionBody, CommandAction, StdinFormat, TrustedCommand, parse_actions};

const PROGRAM: &str = env!("CARGO_BIN_EXE_dbrover-test-program");

/// A table name a shell would split at its space, end the command at its semicolon, and choke
/// on its quote.
const HOSTILE: &str = "it's; DROP TABLE y; --";

const PASSWORD: &str = "pa55-w0rd-n0b0dy-sees";
const SSH_PASSPHRASE: &str = "ssh-s3cret-n0b0dy-sees";

struct Programs {
    engine: TestEngine,
    data_source: DataSource,
    next: u64,
    _dir: tempfile::TempDir,
}

impl Programs {
    /// An engine with one PostgreSQL data source, never connected, its passwords saved.
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self::start_in(dir)
    }

    fn start_in(dir: tempfile::TempDir) -> Self {
        let file = dir.path().join("data_sources.toml");
        let mut engine = TestEngine::start(&file, Arc::new(MemorySecrets::default()));
        engine.expect_started(vec![]);
        let data_source = shop();
        engine.send(Command::SaveDataSource {
            data_source: data_source.clone(),
            password: PasswordUpdate::Set(Password::new(PASSWORD.to_owned())),
            ssh_secret: PasswordUpdate::Set(Password::new(SSH_PASSPHRASE.to_owned())),
        });
        engine.expect(Event::DataSourceSaved(data_source.clone()));
        Self {
            engine,
            data_source,
            next: 0,
            _dir: dir,
        }
    }

    fn trust(&mut self, command: &CommandAction) {
        self.engine.send(Command::TrustCommand(trusted(command)));
        assert!(matches!(
            self.engine.next_event(),
            Event::TrustedCommandsLoaded(trusted) if trusted.contains(&self::trusted(command))
        ));
    }

    fn start_program(&mut self, command: CommandAction, values: TemplateValues) -> ProgramId {
        self.next += 1;
        let program = ProgramId(self.next);
        self.engine
            .send(Command::RunProgram(Box::new(ProgramRequest {
                program,
                data_source: self.data_source.id,
                action: action_id(),
                command,
                values,
            })));
        program
    }

    fn finished(&mut self, program: ProgramId) -> Result<ProgramOutput, ProgramFailure> {
        match self.engine.next_event() {
            Event::ProgramFinished {
                program: finished,
                result,
            } if finished == program => result,
            other => panic!("expected program {program:?} to finish, got {other:?}"),
        }
    }

    fn run(
        &mut self,
        command: CommandAction,
        values: TemplateValues,
    ) -> Result<ProgramOutput, ProgramFailure> {
        let program = self.start_program(command, values);
        self.finished(program)
    }
}

fn shop() -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "o'brien's shop".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                host: "127.0.0.1".to_owned(),
                port: closing_server().into(),
                database: "shop".to_owned(),
                user: "ada".to_owned(),
                tls: TlsMode::Disable,
                ssh: None,
                password_command: None,
                cloud_sign_in: None,
            },
        }),
    }
}

fn action_id() -> dbrover_store::ActionId {
    dbrover_store::ActionId::new("test").unwrap()
}

/// A command action as `actions.toml` would hold it, running the test program.
fn command(arguments: &[&str], fields: &str) -> CommandAction {
    let items: Vec<String> = std::iter::once(PROGRAM)
        .chain(arguments.iter().copied())
        .map(|item| format!("{item:?}"))
        .collect();
    let checked = parse_actions(&format!(
        "[[action]]\nid = \"test\"\nname = \"Test\"\napplies_to = [\"grid.selection\"]\n\
         command = [{}]\n{fields}\n",
        items.join(", ")
    ));
    let Some(ActionBody::Command(command)) = checked.value.into_iter().next().map(|a| a.body)
    else {
        panic!("{:?}", checked.problems);
    };
    command
}

fn trusted(command: &CommandAction) -> TrustedCommand {
    TrustedCommand::of(&action_id(), command)
}

fn selection() -> SelectedCells {
    SelectedCells {
        columns: vec!["id".to_owned(), "email".to_owned()],
        selected: vec![0, 1],
        rows: vec![
            vec![Some("1".to_owned()), Some("ada@example.com".to_owned())],
            vec![Some("2".to_owned()), None],
        ],
    }
}

fn hostile_table() -> TemplateValues {
    TemplateValues {
        qualified_name: Some(format!("\"public\".\"{HOSTILE}\"")),
        schema: Some("public".to_owned()),
        table: Some(HOSTILE.to_owned()),
        data_source: "o'brien's shop".to_owned(),
        cells: Some(selection()),
        ..TemplateValues::default()
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

#[test]
fn a_program_gets_its_arguments_context_and_selection_but_never_a_password() {
    let mut programs = Programs::start();
    let dump = command(
        &[
            "dump",
            "{table}",
            "--name={qualified_name}",
            "{data_source}",
            "postgres://{user}@{host}:{port}/{database}",
            "{value}",
        ],
        "stdin = \"json\"",
    );
    programs.trust(&dump);
    let Ok(port) = programs.data_source.config.server().unwrap().port_number() else {
        panic!("the port is a number");
    };

    let output = programs.run(dump, hostile_table()).unwrap();

    let dumped: serde_json::Value = serde_json::from_str(&output.stdout).unwrap();
    assert_eq!(
        dumped["arguments"],
        serde_json::json!([
            HOSTILE,
            format!("--name=\"public\".\"{HOSTILE}\""),
            "o'brien's shop",
            format!("postgres://ada@127.0.0.1:{port}/shop"),
            "1",
        ])
    );
    let environment = &dumped["environment"];
    for (name, value) in [
        ("DBROVER_DATA_SOURCE", "o'brien's shop"),
        ("DBROVER_DRIVER", "postgres"),
        ("DBROVER_DATABASE", "shop"),
        ("DBROVER_SCHEMA", "public"),
        ("DBROVER_TABLE", HOSTILE),
        ("DBROVER_FORMAT", "json"),
    ] {
        assert_eq!(environment[name], value, "{name}");
    }
    assert_eq!(
        dumped["stdin"].as_str(),
        Some(StdinFormat::Json.encode(&selection()).as_str())
    );
    let directory = PathBuf::from(dumped["directory"].as_str().unwrap());
    assert_eq!(
        canonical(&directory),
        canonical(&std::env::home_dir().unwrap())
    );
    for secret in [PASSWORD, SSH_PASSPHRASE] {
        assert!(
            !output.stdout.contains(secret),
            "a saved secret reached the program: {}",
            output.stdout
        );
    }
    programs.engine.shut_down();
}

#[test]
fn a_failing_program_says_how_it_ended_and_what_it_wrote_to_stderr() {
    let mut programs = Programs::start();
    let fail = command(&["fail"], "");
    programs.trust(&fail);

    let failure = programs.run(fail, TemplateValues::default()).unwrap_err();

    let ProgramFailure::Exited {
        program,
        status,
        stderr,
    } = failure
    else {
        panic!("{failure:?}");
    };
    assert_eq!(program, PROGRAM);
    assert!(status.ends_with('3'), "{status}");
    assert_eq!(stderr, "it went wrong");
    programs.engine.shut_down();
}

#[test]
fn a_runaway_program_is_stopped_by_its_timeout_and_by_a_stop() {
    let mut programs = Programs::start();
    let quick = command(&["wait"], "timeout = \"300ms\"");
    programs.trust(&quick);
    assert_eq!(
        programs.run(quick, TemplateValues::default()),
        Err(ProgramFailure::TimedOut {
            after: Duration::from_millis(300)
        })
    );

    let patient = command(&["wait"], "");
    programs.trust(&patient);
    let program = programs.start_program(patient, TemplateValues::default());
    programs.engine.send(Command::StopProgram(program));
    assert_eq!(programs.finished(program), Err(ProgramFailure::Stopped));
    programs.engine.shut_down();
}

#[test]
fn output_past_the_limit_stops_the_program_and_keeps_what_fits() {
    let mut programs = Programs::start();
    let flood = command(&["flood"], "");
    programs.trust(&flood);

    let output = programs.run(flood, TemplateValues::default()).unwrap();

    assert!(output.truncated);
    assert_eq!(output.stdout.len(), 4 * 1024 * 1024);
    programs.engine.shut_down();
}

#[test]
fn runs_only_a_command_trusted_as_it_is_now_and_remembers_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data_sources.toml");
    let mut programs = Programs::start_in(dir);
    let fail = command(&["fail"], "");
    assert_eq!(
        programs.run(fail.clone(), TemplateValues::default()),
        Err(ProgramFailure::NotTrusted)
    );
    programs.trust(&fail);
    let changed = command(&["fail", "--louder"], "");
    assert_eq!(
        programs.run(changed, TemplateValues::default()),
        Err(ProgramFailure::NotTrusted)
    );

    let Programs { engine, _dir, .. } = programs;
    engine.shut_down();
    let mut restarted = TestEngine::start(&file, Arc::default());
    restarted.expect(Event::Started);
    restarted.expect_keymap(Ok(dbrover_store::KeymapBindings::new()));
    restarted.expect_settings(Ok(dbrover_store::Settings::default()));
    restarted.expect_no_custom_files();
    assert!(matches!(
        restarted.next_event(),
        Event::TrustedCommandsLoaded(trusted) if trusted.contains(&self::trusted(&fail))
    ));
    restarted.shut_down();
}

#[test]
fn a_program_that_isnt_there_names_the_path_it_was_looked_for_in() {
    let mut programs = Programs::start();
    let mut missing = command(&[], "");
    missing.command =
        dbrover_core::CommandTemplate::parse(&["dbrover-no-such-program".to_owned()]).unwrap();
    programs.trust(&missing);

    let failure = programs
        .run(missing, TemplateValues::default())
        .unwrap_err();

    let ProgramFailure::CouldNotStart(message) = failure else {
        panic!("{failure:?}");
    };
    assert!(
        message.starts_with("dbrover-no-such-program was not found in DbRover's PATH ("),
        "{message}"
    );
    programs.engine.shut_down();
}
