//! How `dbrover report run` ended, and the exit code that says so (`docs/reporting.md`, "Exit
//! codes"), so a cron wrapper or a CI step can tell "raise a limit" from "the database is down"
//! from "fix the file".

use dbrover_report::{RunEvent, RunFailure, Step};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The report ran and wrote its files.
    Finished,
    /// It failed otherwise: the combine query, an output, the worker.
    Failed,
    /// The command line is wrong.
    CommandLine,
    /// The run couldn't start: the report file, what it names, its parameters, or the
    /// settings folder.
    Report,
    /// A source failed: connecting to its data source, or its query.
    Source,
    /// A guardrail stopped a step, and said which key raises it.
    Guardrail,
    /// The run was stopped from outside: its own time limit, or a signal.
    Stopped,
}

impl Outcome {
    /// The process's exit code. Part of the command's contract: scripts compare against these
    /// numbers, so a variant's code never changes.
    pub fn code(self) -> i32 {
        match self {
            Self::Finished => 0,
            Self::Failed => 1,
            Self::CommandLine => 2,
            Self::Report => 3,
            Self::Source => 4,
            Self::Guardrail => 5,
            Self::Stopped => 6,
        }
    }

    /// How the run ended, from the event that ends it; `None` for an event that doesn't.
    pub fn of_end(event: &RunEvent) -> Option<Self> {
        match event {
            RunEvent::Finished { .. } => Some(Self::Finished),
            RunEvent::Failed(failure) => Some(Self::of_failure(failure)),
            RunEvent::Cancelled => Some(Self::Stopped),
            _ => None,
        }
    }

    /// A step's limit is a guardrail; the run's own time limit, which the worker can't report
    /// itself because it is enforced by killing it, is told apart by the worker as its step.
    fn of_failure(failure: &RunFailure) -> Self {
        match (&failure.step, failure.limit.is_some()) {
            (Step::Worker, true) => Self::Stopped,
            (_, true) => Self::Guardrail,
            (Step::Report, false) => Self::Report,
            (Step::Source { .. }, false) => Self::Source,
            (Step::Combine | Step::Output { .. } | Step::Worker, false) => Self::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use dbrover_report::LimitHit;

    use super::*;

    fn failed(step: Step) -> RunEvent {
        RunEvent::Failed(RunFailure::new(step, "it failed"))
    }

    fn at_limit(step: Step) -> RunEvent {
        RunEvent::Failed(RunFailure::at_limit(
            step,
            "it stopped",
            LimitHit {
                key: "max_rows".to_owned(),
                value: "10 rows".to_owned(),
                raise_in: "the [[source]] \"lines\"".to_owned(),
            },
        ))
    }

    fn source() -> Step {
        Step::Source {
            id: "lines".to_owned(),
        }
    }

    fn output() -> Step {
        Step::Output {
            number: 1,
            file: "sales.csv".to_owned(),
        }
    }

    #[test]
    fn gives_each_way_a_run_ends_its_exit_code() {
        let cases = [
            (RunEvent::Finished { elapsed_ms: 5 }, 0),
            (failed(Step::Combine), 1),
            (failed(output()), 1),
            (failed(Step::Worker), 1),
            (failed(Step::Report), 3),
            (failed(source()), 4),
            (at_limit(source()), 5),
            (at_limit(Step::Combine), 5),
            (at_limit(output()), 5),
            (at_limit(Step::Worker), 6),
            (RunEvent::Cancelled, 6),
        ];
        for (event, code) in cases {
            assert_eq!(
                Outcome::of_end(&event).map(Outcome::code),
                Some(code),
                "{event:?}"
            );
        }
    }

    #[test]
    fn only_the_last_event_ends_a_run() {
        let progress = RunEvent::Progress {
            step: Step::Combine,
            rows: 1,
            bytes: 1,
            elapsed_ms: 1,
            batch: None,
        };
        assert_eq!(Outcome::of_end(&progress), None);
    }

    #[test]
    fn the_command_line_has_a_code_of_its_own() {
        assert_eq!(Outcome::CommandLine.code(), 2);
    }
}
