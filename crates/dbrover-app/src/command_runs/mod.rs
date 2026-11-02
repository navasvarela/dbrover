//! Command actions' programs, and export programs copying to the clipboard (W8d), from
//! choosing one to showing what it wrote: the question before a command first runs, the
//! programs still running, and the windows their output or failure opens. The engine runs them
//! (see `docs/hackability.md` and `docs/exports.md`).

mod windows;

pub use windows::{ProgramFacts, program_facts};

use std::collections::BTreeMap;

use dbrover_core::{DataSourceId, TemplateValues};
use dbrover_engine::{
    Command, Event, ExportProgramRequest, ProgramFailure, ProgramId, ProgramOutput, ProgramRequest,
};
use dbrover_export::{CustomFormat, ProgramSpec};
use dbrover_store::{
    ActionId, CommandAction, CommandOutput, CustomAction, StdinFormat, TrustedCommand,
    TrustedCommands,
};

/// Command actions the app has asked the engine to run, or is asking the user about.
#[derive(Default)]
pub struct CommandRuns {
    trusted: TrustedCommands,
    next_id: u64,
    running: BTreeMap<ProgramId, Run>,
    /// A command that hasn't been trusted yet, shown in full until the user answers.
    asking: Option<Question>,
    windows: Vec<OutputWindow>,
    /// How the last run ended, when it opened no window: stopped, or cut short into a console.
    notice: Option<String>,
}

/// A program running, and where its output goes.
struct Run {
    name: String,
    output: Destination,
    data_source: DataSourceId,
}

/// Where a finished program's output goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// Where its command action says.
    Action(CommandOutput),
    /// The clipboard: Copy as an export program.
    Clipboard,
}

/// The question before a command's first run.
struct Question {
    name: String,
    run: Pending,
}

/// A run waiting for the user to trust its command.
enum Pending {
    Action(ProgramRequest),
    Copy(ExportProgramRequest),
}

impl Pending {
    fn trusted(&self) -> TrustedCommand {
        match self {
            Self::Action(request) => trusted(request),
            Self::Copy(request) => TrustedCommand::export(&request.spec.id, &request.spec.program),
        }
    }

    fn program(&self) -> ProgramId {
        match self {
            Self::Action(request) => request.program,
            Self::Copy(request) => request.program,
        }
    }

    fn data_source(&self) -> DataSourceId {
        match self {
            Self::Action(request) => request.data_source,
            Self::Copy(request) => request.data_source,
        }
    }

    fn destination(&self) -> Destination {
        match self {
            Self::Action(request) => Destination::Action(request.command.output),
            Self::Copy(_) => Destination::Clipboard,
        }
    }

    fn into_command(self) -> Command {
        match self {
            Self::Action(request) => Command::RunProgram(Box::new(request)),
            Self::Copy(request) => Command::RunExportProgram(Box::new(request)),
        }
    }
}

/// What a finished program opens: its output, or why it failed.
enum OutputWindow {
    Output {
        program: ProgramId,
        name: String,
        output: ProgramOutput,
    },
    Failure {
        program: ProgramId,
        name: String,
        failure: ProgramFailure,
    },
}

/// What the app does with a finished program that the runs can't do themselves.
pub enum Finished {
    /// Open the program's output, SQL it wrote, in a new console on the data source.
    OpenConsole {
        data_source: DataSourceId,
        text: String,
    },
    /// Put what an export program wrote on the clipboard.
    Copy(String),
}

impl CommandRuns {
    /// Runs `command` on `values`, if it is trusted as it is now; otherwise asks first and
    /// returns nothing until the user answers.
    pub fn start(
        &mut self,
        action: &CustomAction,
        command: &CommandAction,
        data_source: DataSourceId,
        values: TemplateValues,
    ) -> Option<Command> {
        self.next_id += 1;
        let request = ProgramRequest {
            program: ProgramId(self.next_id),
            data_source,
            action: action.id.clone(),
            command: command.clone(),
            values,
        };
        self.start_or_ask(action.name.clone(), Pending::Action(request))
    }

    /// Copies the selected cells, `stdin` as JSON Lines, through `format`'s program `spec`, if
    /// it is trusted as it is now; otherwise asks first.
    pub fn start_copy(
        &mut self,
        format: &CustomFormat,
        spec: ProgramSpec,
        data_source: DataSourceId,
        stdin: String,
    ) -> Option<Command> {
        self.next_id += 1;
        let request = ExportProgramRequest {
            program: ProgramId(self.next_id),
            data_source,
            spec,
            stdin,
        };
        self.start_or_ask(format.name.clone(), Pending::Copy(request))
    }

    fn start_or_ask(&mut self, name: String, run: Pending) -> Option<Command> {
        if self.trusted.contains(&run.trusted()) {
            return Some(self.run(name, run));
        }
        self.asking = Some(Question { name, run });
        None
    }

    fn run(&mut self, name: String, run: Pending) -> Command {
        self.notice = None;
        self.running.insert(
            run.program(),
            Run {
                name,
                output: run.destination(),
                data_source: run.data_source(),
            },
        );
        run.into_command()
    }

    /// The user's answer to the question: trust the command and run it, or forget it.
    fn answer(&mut self, run: bool) -> Vec<Command> {
        let Some(Question { name, run: pending }) = self.asking.take() else {
            return Vec::new();
        };
        if !run {
            return Vec::new();
        }
        let trusted = pending.trusted();
        self.trusted.trust(trusted.clone());
        // In this order: the engine trusts it before it is asked to run it.
        vec![Command::TrustCommand(trusted), self.run(name, pending)]
    }

    /// Trusts `command` here at once, as the engine is told to, so what is shown follows.
    pub fn trust(&mut self, command: TrustedCommand) {
        self.trusted.trust(command);
    }

    /// Whether the export program `format` runs without asking.
    pub fn trusts(&self, command: &TrustedCommand) -> bool {
        self.trusted.contains(command)
    }

    pub fn is_trusted(&self, id: &ActionId, command: &CommandAction) -> bool {
        self.trusted.contains(&TrustedCommand::of(id, command))
    }

    pub fn is_running(&self) -> bool {
        !self.running.is_empty()
    }

    /// Stops every running program.
    pub fn stop_all(&self) -> Vec<Command> {
        self.running
            .keys()
            .map(|&program| Command::StopProgram(program))
            .collect()
    }

    /// Follows the engine: what is trusted, and how each program ended.
    pub fn apply(&mut self, event: &Event) -> Option<Finished> {
        match event {
            Event::TrustedCommandsLoaded(trusted) => {
                self.trusted = trusted.clone();
                None
            }
            Event::ProgramFinished { program, result } => self.finished(*program, result),
            _ => None,
        }
    }

    fn finished(
        &mut self,
        program: ProgramId,
        result: &Result<ProgramOutput, ProgramFailure>,
    ) -> Option<Finished> {
        let run = self.running.remove(&program)?;
        let output = match result {
            Ok(output) => output,
            Err(ProgramFailure::Stopped) => {
                self.notice = Some(format!("{} stopped", run.name));
                return None;
            }
            Err(failure) => {
                self.windows.push(OutputWindow::Failure {
                    program,
                    name: run.name,
                    failure: failure.clone(),
                });
                return None;
            }
        };
        let output_to = match run.output {
            Destination::Action(output_to) => output_to,
            Destination::Clipboard => {
                if output.truncated {
                    self.notice = Some(format!("{}: {}", run.name, windows::CUT_SHORT));
                }
                return Some(Finished::Copy(output.stdout.clone()));
            }
        };
        match output_to {
            CommandOutput::Viewer => self.windows.push(OutputWindow::Output {
                program,
                name: run.name,
                output: output.clone(),
            }),
            CommandOutput::Console => {
                if output.truncated {
                    self.notice = Some(format!("{}: {}", run.name, windows::CUT_SHORT));
                }
                return Some(Finished::OpenConsole {
                    data_source: run.data_source,
                    text: output.stdout.clone(),
                });
            }
            CommandOutput::Log => log(&run.name, output),
            CommandOutput::None => {}
        }
        None
    }
}

fn trusted(request: &ProgramRequest) -> TrustedCommand {
    TrustedCommand::of(&request.action, &request.command)
}

/// Sends a program's output to the log console, a line an entry, stderr as warnings.
fn log(name: &str, output: &ProgramOutput) {
    for line in output.stdout.lines() {
        tracing::info!("{name}: {line}");
    }
    for line in output.stderr.lines() {
        tracing::warn!("{name}: {line}");
    }
    if output.truncated {
        tracing::warn!("{name}: {}", windows::CUT_SHORT);
    }
}

/// A command as `actions.toml` writes it, every argument quoted, so where one ends is plain.
pub fn command_text(source: &[String]) -> String {
    let items: Vec<String> = source.iter().map(|item| format!("{item:?}")).collect();
    format!("[{}]", items.join(", "))
}

/// As `actions.toml` writes a timeout, "30s" or "1m 30s".
pub fn timeout_text(timeout: std::time::Duration) -> String {
    match jiff::SignedDuration::try_from(timeout) {
        // `#` selects jiff's friendly format ("30s") over ISO 8601 ("PT30S").
        Ok(duration) => format!("{duration:#}"),
        Err(_) => format!("{}s", timeout.as_secs()),
    }
}

fn stdin_text(format: StdinFormat) -> &'static str {
    match format {
        StdinFormat::Json => "the selected cells, as JSON",
        StdinFormat::Csv => "the selected cells, as CSV",
        StdinFormat::Tsv => "the selected cells, as TSV",
        StdinFormat::None => "nothing",
    }
}

fn output_text(output: CommandOutput) -> &'static str {
    match output {
        CommandOutput::Viewer => "shown in a window",
        CommandOutput::Console => "opened in a new console",
        CommandOutput::Log => "sent to the log",
        CommandOutput::None => "not shown",
    }
}

#[cfg(test)]
mod tests {
    use dbrover_store::{ActionBody, parse_actions};

    use super::*;

    fn action(output: &str) -> CustomAction {
        let checked = parse_actions(&format!(
            "[[action]]\nid = \"jq\"\nname = \"Send to jq\"\napplies_to = [\"grid.selection\"]\n\
             command = [\"jq\", \".\"]\nstdin = \"json\"\noutput = {output:?}\n"
        ));
        checked.value.into_iter().next().unwrap()
    }

    fn start(runs: &mut CommandRuns, action: &CustomAction) -> Option<Command> {
        let ActionBody::Command(command) = &action.body else {
            panic!("{action:?}");
        };
        runs.start(
            action,
            command,
            DataSourceId::generate(),
            TemplateValues::default(),
        )
    }

    fn finish(
        runs: &mut CommandRuns,
        result: Result<ProgramOutput, ProgramFailure>,
    ) -> Option<Finished> {
        let program = *runs.running.keys().next().unwrap();
        runs.apply(&Event::ProgramFinished { program, result })
    }

    #[test]
    fn asks_before_the_first_run_and_not_after() {
        let jq = action("viewer");
        let mut runs = CommandRuns::default();

        assert!(start(&mut runs, &jq).is_none());
        let commands = runs.answer(true);
        assert!(matches!(
            commands.as_slice(),
            [Command::TrustCommand(_), Command::RunProgram(_)]
        ));
        assert!(runs.is_running());

        assert!(matches!(
            start(&mut runs, &jq),
            Some(Command::RunProgram(_))
        ));
    }

    #[test]
    fn cancelling_the_question_runs_and_trusts_nothing() {
        let jq = action("viewer");
        let mut runs = CommandRuns::default();
        assert!(start(&mut runs, &jq).is_none());

        assert!(runs.answer(false).is_empty());

        assert!(!runs.is_running());
        assert!(start(&mut runs, &jq).is_none(), "it asks again");
    }

    #[test]
    fn output_goes_where_the_action_says_and_a_failure_always_opens_a_window() {
        let output = ProgramOutput {
            stdout: "SELECT 1;\n".to_owned(),
            ..ProgramOutput::default()
        };
        let cases = [
            ("viewer", 1, false),
            ("console", 0, true),
            ("log", 0, false),
            ("none", 0, false),
        ];
        for (written, windows, opens_console) in cases {
            let mut runs = CommandRuns::default();
            let jq = action(written);
            start(&mut runs, &jq);
            runs.answer(true);
            let finished = finish(&mut runs, Ok(output.clone()));
            assert_eq!(runs.windows.len(), windows, "{written}");
            assert_eq!(
                matches!(finished, Some(Finished::OpenConsole { ref text, .. }) if text == "SELECT 1;\n"),
                opens_console,
                "{written}"
            );

            start(&mut runs, &jq);
            finish(
                &mut runs,
                Err(ProgramFailure::TimedOut {
                    after: std::time::Duration::from_secs(1),
                }),
            );
            assert_eq!(runs.windows.len(), windows + 1, "{written}");
            assert!(!runs.is_running());
        }
    }

    #[test]
    fn a_stop_is_a_notice_not_a_window() {
        let mut runs = CommandRuns::default();
        start(&mut runs, &action("viewer"));
        runs.answer(true);
        assert_eq!(runs.stop_all().len(), 1);

        finish(&mut runs, Err(ProgramFailure::Stopped));

        assert!(runs.windows.is_empty());
        assert_eq!(runs.notice.as_deref(), Some("Send to jq stopped"));
    }

    #[test]
    fn writes_commands_and_timeouts_as_actions_toml_does() {
        let source = ["jq".to_owned(), "-r".to_owned(), ".[] | \"x\"".to_owned()];
        assert_eq!(command_text(&source), r#"["jq", "-r", ".[] | \"x\""]"#);
        assert_eq!(timeout_text(std::time::Duration::from_secs(90)), "1m 30s");
        assert_eq!(timeout_text(std::time::Duration::from_millis(500)), "500ms");
    }
}
