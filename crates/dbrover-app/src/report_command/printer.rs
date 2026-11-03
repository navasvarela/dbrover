//! What `dbrover report run` prints as a run goes (`docs/reporting.md`, "What it prints"):
//! lines for a person ([`HumanLines`]), or the worker's own events as JSON Lines ([`Events`]),
//! the types the app reads, so there is no format of the command's own to keep.

use std::io::{self, Write};

use dbrover_report::{RunEvent, RunFailure, Step, TypedValues};

use super::args::OutputStyle;
use super::human::HumanLines;
use crate::report_worker::Events;

pub trait Printer {
    /// One of the run's events, as the worker sent it or the command made it.
    fn event(&mut self, event: &RunEvent);

    /// What kept the run from starting: the report, its parameters or the settings folder.
    /// `heading` says which, for a person.
    fn not_started(&mut self, heading: &str, problems: &[String]);
}

/// The printer for `style`, on the process's standard output and error. `values` are the
/// parameters the run was given.
pub fn for_style(style: OutputStyle, values: &TypedValues) -> Box<dyn Printer> {
    match style {
        OutputStyle::Human => Box::new(HumanLines::new(
            io::stdout().lock(),
            io::stderr().lock(),
            values.clone(),
        )),
        OutputStyle::JsonLines => Box::new(Events::new(io::stdout().lock())),
    }
}

impl<W: Write> Printer for Events<W> {
    fn event(&mut self, event: &RunEvent) {
        self.send(event);
    }

    /// As the worker says it: a run that failed in the report step, a problem a line.
    fn not_started(&mut self, _heading: &str, problems: &[String]) {
        let failure = RunFailure::new(Step::Report, problems.join("\n"));
        self.send(&RunEvent::Failed(failure));
    }
}
