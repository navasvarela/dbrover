//! What `dbrover report run` prints as a run goes (`docs/reporting.md`, "What it prints"):
//! lines for a person ([`HumanLines`]), or the worker's own events as JSON Lines ([`Events`]),
//! the types the app reads, so there is no format of the command's own to keep.

use std::io::{self, IsTerminal as _, Write};
use std::time::Instant;

use dbrover_report::{RunEvent, RunFailure, Step, TimedEvent, TypedValues};

use super::args::OutputStyle;
use super::human::HumanLines;
use crate::report_worker::Events;

pub trait Printer {
    /// One of the run's events, as the worker sent it or the command made it.
    fn event(&mut self, event: &TimedEvent);

    /// What kept the run from starting: the report, its parameters or the settings folder.
    /// `heading` says which, for a person.
    fn not_started(&mut self, heading: &str, problems: &[String]);
}

/// The printer for `style`, on the process's standard output and error. `values` are the
/// parameters the run was given. `progress` asks for a live line, which only a terminal gets:
/// in a pipe or cron's mail, a line rewritten in place is noise.
pub fn for_style(style: OutputStyle, values: &TypedValues, progress: bool) -> Box<dyn Printer> {
    match style {
        OutputStyle::Human => {
            let lines = HumanLines::new(io::stdout().lock(), io::stderr().lock(), values.clone());
            if progress && io::stderr().is_terminal() {
                Box::new(lines.with_live_line())
            } else {
                Box::new(lines)
            }
        }
        OutputStyle::JsonLines => Box::new(Events::new(io::stdout().lock(), Instant::now())),
    }
}

impl<W: Write> Printer for Events<W> {
    fn event(&mut self, event: &TimedEvent) {
        // A closed standard output (`| head`) isn't the run's failure: its exit code says how
        // it ended.
        let _ = self.forward(event);
    }

    /// As the worker says it: a run that failed in the report step, a problem a line.
    fn not_started(&mut self, _heading: &str, problems: &[String]) {
        let failure = RunFailure::new(Step::Report, problems.join("\n"));
        let _ = self.send(&RunEvent::Failed(failure));
    }
}
