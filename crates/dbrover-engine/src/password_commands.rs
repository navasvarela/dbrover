//! Running a data source's password command (W9) on the program machinery command actions use
//! (H2): only once trusted, without a shell, under a timeout, with its output capped.
//!
//! Its standard output, trimmed, is the password. It is kept only in memory, for the one
//! connection it was run for: never in the keychain, a log, the history or the session's
//! passwords, so every connection, reconnecting included, runs the command again and gets a
//! token that hasn't expired.

use std::sync::Arc;
use std::time::Duration;

use dbrover_core::{
    ConnectionConfig, DataSourceFields, DataSourceId, Password, PasswordCommand, RenderError,
    Variables,
};
use dbrover_store::{TrustedCommand, TrustedCommands, TrustedCommandsFile};
use tokio::sync::watch;

use crate::environment::ProcessEnvironment;
use crate::programs::{self, Invocation, ProgramFailure, ProgramRunner};

/// How long a password command may take. Chosen rather than measured: making a cloud token is
/// a local signature (`aws`) or one HTTPS request (`gcloud`, `az`), but `op read` or a vault
/// login may wait for the user to unlock it with a fingerprint or in a browser. The connection
/// timeout starts only after it, so a slow unlock doesn't eat into it.
pub const PASSWORD_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a password command gave no password.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PasswordCommandError {
    /// Nobody has said yes to running it yet. The app asks; `dbrover mcp`, which can't, says
    /// where to.
    #[error(
        "the password command isn't trusted to run yet; connecting to the data source in the \
         DbRover app asks"
    )]
    NotTrusted(PasswordCommand),
    #[error("could not fill in the password command: {0}")]
    Unfillable(RenderError),
    #[error("could not get the password: {}", describe_failure(.0))]
    Failed(ProgramFailure),
    #[error("could not get the password: the password command printed nothing")]
    Empty,
    #[error("could not get the password: the password command printed more than DbRover keeps")]
    TooLong,
}

/// A failure with what the program wrote to stderr, which says why far better than its exit
/// status alone.
fn describe_failure(failure: &ProgramFailure) -> String {
    match failure {
        ProgramFailure::Exited { stderr, .. } if !stderr.trim().is_empty() => {
            format!("{failure}: {}", stderr.trim())
        }
        _ => failure.to_string(),
    }
}

/// What running password commands needs: something to start programs, and which commands
/// may run.
#[derive(Clone)]
pub struct PasswordCommands {
    runner: Arc<dyn ProgramRunner>,
    trust: Trust,
}

#[derive(Clone)]
enum Trust {
    /// Only the commands in `trusted_commands.toml` run, as they stand when connecting.
    Recorded(watch::Receiver<TrustedCommands>),
    /// The data source dialog's connection test, which runs the command the dialog shows.
    /// Asking "run this?" about the command the user is looking at, and has just chosen Test
    /// for, tells them nothing new. Nothing is recorded, so connecting still asks once.
    Shown,
}

impl PasswordCommands {
    /// Runs the commands in `trusted`, for front ends that don't ask the user to trust
    /// commands themselves (`dbrover mcp`).
    pub fn new(trusted: TrustedCommands, runner: Arc<dyn ProgramRunner>) -> Self {
        let (_, trusted) = watch::channel(trusted);
        Self::recorded(trusted, runner)
    }

    /// The commands trusted in `file`, as it is now. A file that can't be read trusts
    /// nothing.
    pub async fn load(file: &TrustedCommandsFile, runner: Arc<dyn ProgramRunner>) -> Self {
        Self::new(programs::load_trusted(file).await, runner)
    }

    /// Runs the commands `trusted` holds when connecting, which the engine keeps up to date as
    /// the user trusts more.
    pub(crate) fn recorded(
        trusted: watch::Receiver<TrustedCommands>,
        runner: Arc<dyn ProgramRunner>,
    ) -> Self {
        Self {
            runner,
            trust: Trust::Recorded(trusted),
        }
    }

    /// These commands, for the dialog's connection test.
    pub(crate) fn shown(&self) -> Self {
        Self {
            runner: Arc::clone(&self.runner),
            trust: Trust::Shown,
        }
    }

    fn may_run(&self, data_source: DataSourceId, command: &PasswordCommand) -> bool {
        match &self.trust {
            Trust::Recorded(trusted) => trusted
                .borrow()
                .contains(&TrustedCommand::password(data_source, command)),
            Trust::Shown => true,
        }
    }

    /// Runs `command` for the data source whose resolved settings are `config`, and returns
    /// the password it prints.
    ///
    /// # Errors
    ///
    /// [`PasswordCommandError`] if the command isn't trusted, can't be filled in, fails, or
    /// prints no password.
    pub(crate) async fn password(
        &self,
        data_source: DataSourceId,
        command: &PasswordCommand,
        config: &ConnectionConfig,
    ) -> Result<Password, PasswordCommandError> {
        if !self.may_run(data_source, command) {
            return Err(PasswordCommandError::NotTrusted(command.clone()));
        }
        let invocation = invocation(command, config, &ProcessEnvironment)?;
        let program = invocation.program.clone();
        tracing::info!(%data_source, "running the password command {program}");
        let output = tokio::time::timeout(PASSWORD_COMMAND_TIMEOUT, self.runner.run(invocation))
            .await
            .map_err(|_| {
                PasswordCommandError::Failed(ProgramFailure::TimedOut {
                    after: PASSWORD_COMMAND_TIMEOUT,
                })
            })?
            .map_err(PasswordCommandError::Failed)?;
        if output.truncated {
            return Err(PasswordCommandError::TooLong);
        }
        let password = output.stdout.trim();
        if password.is_empty() {
            return Err(PasswordCommandError::Empty);
        }
        tracing::info!(%data_source, "the password command {program} gave a password");
        Ok(Password::new(password))
    }
}

/// The command filled in from the data source's settings, with nothing on stdin, run in the
/// home folder as command actions are.
fn invocation(
    command: &PasswordCommand,
    config: &ConnectionConfig,
    variables: &dyn Variables,
) -> Result<Invocation, PasswordCommandError> {
    let arguments = command
        .render(&DataSourceFields::from(config))
        .map_err(PasswordCommandError::Unfillable)?;
    Ok(Invocation {
        program: command.program().to_owned(),
        arguments,
        environment: Vec::new(),
        stdin: String::new(),
        directory: variables.home(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use dbrover_core::{Port, PostgresConfig, ServerConfig, TlsMode};
    use dbrover_store::TrustedCommand;

    use super::*;
    use crate::ProgramOutput;

    /// Answers every run with `answer`, keeping what it was asked to run.
    struct Scripted {
        answer: Result<ProgramOutput, ProgramFailure>,
        ran: Mutex<Vec<Invocation>>,
    }

    #[async_trait]
    impl ProgramRunner for Scripted {
        async fn run(&self, invocation: Invocation) -> Result<ProgramOutput, ProgramFailure> {
            self.ran
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(invocation);
            self.answer.clone()
        }

        fn start_export(
            &self,
            _: Invocation,
            _: std::fs::File,
            _: Duration,
        ) -> std::io::Result<Box<dyn dbrover_export::ExportPipe>> {
            unreachable!("password commands are never export programs")
        }
    }

    fn scripted(answer: Result<ProgramOutput, ProgramFailure>) -> Arc<Scripted> {
        Arc::new(Scripted {
            answer,
            ran: Mutex::new(Vec::new()),
        })
    }

    fn printed(stdout: &str) -> Result<ProgramOutput, ProgramFailure> {
        Ok(ProgramOutput {
            stdout: stdout.to_owned(),
            ..ProgramOutput::default()
        })
    }

    fn command(items: &[&str]) -> PasswordCommand {
        let items: Vec<String> = items.iter().map(|&item| item.to_owned()).collect();
        PasswordCommand::parse(&items).unwrap()
    }

    fn postgres(port: Port) -> ConnectionConfig {
        ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                host: "db.example.com".to_owned(),
                port,
                database: "shop".to_owned(),
                user: "ada".to_owned(),
                tls: TlsMode::default(),
                ssh: None,
                password_command: None,
            },
        })
    }

    fn trusting(
        runner: Arc<Scripted>,
        trusted: &[TrustedCommand],
    ) -> (watch::Sender<TrustedCommands>, PasswordCommands) {
        let mut commands = TrustedCommands::default();
        for command in trusted {
            commands.trust(command.clone());
        }
        let (sender, receiver) = watch::channel(commands);
        (sender, PasswordCommands::recorded(receiver, runner))
    }

    #[tokio::test]
    async fn runs_only_once_trusted_and_trims_what_it_prints() {
        let id = DataSourceId::generate();
        let token = command(&["token", "--host={host}", "{port}", "{user}", "{database}"]);
        let runner = scripted(printed("  secret-token\n"));
        let (trust, commands) = trusting(Arc::clone(&runner), &[]);
        let config = postgres(Port::Number(5433));

        let refused = commands.password(id, &token, &config).await;
        assert_eq!(
            refused,
            Err(PasswordCommandError::NotTrusted(token.clone()))
        );
        assert!(runner.ran.lock().unwrap().is_empty(), "nothing ran");

        trust.send_modify(|trusted| trusted.trust(TrustedCommand::password(id, &token)));
        let password = commands.password(id, &token, &config).await.unwrap();
        assert_eq!(password.expose(), "secret-token");
        let ran = runner.ran.lock().unwrap();
        assert_eq!(ran[0].program, "token");
        assert_eq!(
            ran[0].arguments,
            ["--host=db.example.com", "5433", "ada", "shop"]
        );
        assert_eq!(ran[0].stdin, "");
    }

    #[tokio::test]
    async fn the_dialogs_test_runs_the_command_it_shows() {
        let runner = scripted(printed("dbrover"));
        let (_trust, commands) = trusting(runner, &[]);
        let password = commands
            .shown()
            .password(
                DataSourceId::generate(),
                &command(&["printf", "dbrover"]),
                &postgres(Port::Number(5432)),
            )
            .await
            .unwrap();
        assert_eq!(password.expose(), "dbrover");
    }

    #[tokio::test]
    async fn says_why_there_is_no_password() {
        let failed = Err(ProgramFailure::Exited {
            program: "vault".to_owned(),
            status: "exit status: 2".to_owned(),
            stderr: "permission denied\n".to_owned(),
        });
        let too_long = Ok(ProgramOutput {
            stdout: "x".repeat(8),
            stderr: String::new(),
            truncated: true,
        });
        let cases = [
            (
                failed,
                "could not get the password: vault failed (exit status: 2): permission denied",
            ),
            (
                printed(" \n"),
                "could not get the password: the password command printed nothing",
            ),
            (
                too_long,
                "could not get the password: the password command printed more than DbRover \
                 keeps",
            ),
        ];
        let id = DataSourceId::generate();
        let vault = command(&["vault", "read", "{host}"]);
        for (answer, message) in cases {
            let runner = scripted(answer);
            let (_trust, commands) = trusting(runner, &[TrustedCommand::password(id, &vault)]);
            let error = commands
                .password(id, &vault, &postgres(Port::Number(5432)))
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), message);
        }
    }

    #[tokio::test]
    async fn a_port_left_as_a_variable_cant_be_filled_in() {
        let id = DataSourceId::generate();
        let token = command(&["token", "{port}"]);
        let (_trust, commands) = trusting(
            scripted(printed("never")),
            &[TrustedCommand::password(id, &token)],
        );
        let error = commands
            .password(
                id,
                &token,
                &postgres(Port::Placeholder("${PGPORT}".to_owned())),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "could not fill in the password command: nothing here fills {port}"
        );
    }
}
