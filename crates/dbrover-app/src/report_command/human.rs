//! What `dbrover report run` prints for a person, or for cron's mail: a line on standard error
//! as the run starts and as each step ends, and each file written on standard output, its full
//! path a line, so a script can pipe them on (`docs/reporting.md`, "What it prints").

use std::io::Write;

use dbrover_core::{BlobId, ByteCount};
use dbrover_report::{RunEvent, RunFailure, Step, TimedEvent, TypedValues, WrittenFile, grouped};

use super::printer::Printer;
use super::progress::{CLEAR_LINE, LiveLine};

/// Writes the lines for each event of a run.
pub struct HumanLines<O: Write, E: Write> {
    out: O,
    err: E,
    values: TypedValues,
    /// The SQL a combine step in PRQL compiled to, as the run's start said, printed under a
    /// combine step's error, whose line numbers point into it.
    combine_sql: Option<String>,
    /// With `--progress` on a terminal: the line rewritten in place as steps run.
    live: Option<LiveLine>,
    /// Whether the live line is on the terminal now, to clear before another line.
    live_shown: bool,
}

impl<O: Write, E: Write> Printer for HumanLines<O, E> {
    fn event(&mut self, timed: &TimedEvent) {
        let event = &timed.event;
        let live_changed = self.live.as_mut().is_some_and(|live| live.apply(event));
        let shown_before = self.live_shown;
        self.lines(event);
        // Redrawn when it changed, or when a line of the run's took its place.
        if live_changed || (shown_before && !self.live_shown) {
            self.draw_live();
        }
    }

    fn not_started(&mut self, heading: &str, problems: &[String]) {
        self.error_line(heading);
        for problem in problems {
            self.error_line(&format!("  {problem}"));
        }
    }
}

impl<O: Write, E: Write> HumanLines<O, E> {
    /// The lines `event` prints.
    fn lines(&mut self, event: &RunEvent) {
        match event {
            RunEvent::Started {
                report,
                combine_sql,
                version,
                ..
            } => {
                let line = started(report, &self.values, version.as_ref());
                self.error_line(&line);
                self.combine_sql.clone_from(combine_sql);
            }
            RunEvent::SourceDone {
                id,
                rows,
                bytes,
                elapsed_ms,
                warnings,
                batches,
                ..
            } => {
                let batches = match batches {
                    Some(1) => ", 1 batch".to_owned(),
                    Some(batches) => format!(", {batches} batches"),
                    None => String::new(),
                };
                let line = format!(
                    "Source {id}: {}{batches}",
                    amounts(*rows, *bytes, *elapsed_ms)
                );
                self.error_line(&line);
                self.warnings(&format!("source {id}"), warnings);
            }
            RunEvent::SourceAttached { id, .. } => {
                self.error_line(&format!("Source {id}: attached, read in place"));
            }
            RunEvent::CombineDone {
                rows,
                bytes,
                elapsed_ms,
                ..
            } => {
                let line = format!("Combine: {}", amounts(*rows, *bytes, *elapsed_ms));
                self.error_line(&line);
            }
            RunEvent::OutputWritten(file) => self.written(file),
            RunEvent::Finished { elapsed_ms } => {
                self.error_line(&format!("Finished in {}.", seconds(*elapsed_ms)));
            }
            RunEvent::Failed(failure) => {
                for line in failed(failure) {
                    self.error_line(&line);
                }
                if failure.step == Step::Combine
                    && let Some(sql) = self.combine_sql.take()
                {
                    for line in numbered_sql(&sql) {
                        self.error_line(&line);
                    }
                }
            }
            RunEvent::Cancelled => self.error_line("Cancelled. No file was written."),
            RunEvent::Progress { .. }
            | RunEvent::Preview(_)
            | RunEvent::WorkerUsage { .. }
            | RunEvent::Explained(_) => {}
        }
    }

    /// `values` are the parameters the run was given, named in its first line.
    pub fn new(out: O, err: E, values: TypedValues) -> Self {
        Self {
            out,
            err,
            values,
            combine_sql: None,
            live: None,
            live_shown: false,
        }
    }

    /// With a live progress line, for a terminal.
    pub fn with_live_line(self) -> Self {
        Self {
            live: Some(LiveLine::default()),
            ..self
        }
    }

    /// Writes the live line in place of the one shown, or clears it once no step runs.
    fn draw_live(&mut self) {
        let Some(text) = self.live.as_ref().map(LiveLine::text) else {
            return;
        };
        // As for any line on standard error: nothing is left to tell if it is gone.
        let _ = match &text {
            Some(text) => write!(self.err, "{CLEAR_LINE}{text}"),
            None if self.live_shown => write!(self.err, "{CLEAR_LINE}"),
            None => Ok(()),
        }
        .and_then(|()| self.err.flush());
        self.live_shown = text.is_some();
    }

    fn written(&mut self, file: &WrittenFile) {
        let path = file.path.display().to_string();
        // Standard output is the files' list a script reads; an error writing it (a pipe
        // closed by `head`, say) isn't the run's failure, which the exit code says.
        let _ = writeln!(self.out, "{path}").and_then(|()| self.out.flush());
        self.warnings(&path, &file.warnings);
    }

    fn warnings(&mut self, about: &str, warnings: &[String]) {
        for warning in warnings {
            self.error_line(&format!("Warning, {about}: {warning}"));
        }
    }

    fn error_line(&mut self, line: &str) {
        if std::mem::take(&mut self.live_shown) {
            let _ = write!(self.err, "{CLEAR_LINE}");
        }
        // As for standard output: nothing is left to tell if standard error is gone.
        let _ = writeln!(self.err, "{line}");
    }
}

/// `Sales by category (since = 2026-10-01, until = 2026-11-01)`.
/// The report's title, the values it runs with, and the version of its file that runs, as
/// Git abbreviates a blob id.
fn started(report: &str, values: &TypedValues, version: Option<&BlobId>) -> String {
    let mut line = report.to_owned();
    if !values.is_empty() {
        let values: Vec<String> = values
            .iter()
            .map(|(name, value)| format!("{name} = {value}"))
            .collect();
        line.push_str(&format!(" ({})", values.join(", ")));
    }
    if let Some(version) = version {
        line.push_str(&format!(", version {}", version.short()));
    }
    line
}

/// The SQL a combine step's PRQL compiled to, its lines numbered as DuckDB's errors count them.
fn numbered_sql(sql: &str) -> Vec<String> {
    let lines: Vec<&str> = sql.lines().collect();
    let width = lines.len().to_string().len();
    std::iter::once("The combine step's SQL, compiled from its PRQL:".to_owned())
        .chain(
            lines
                .iter()
                .enumerate()
                .map(|(index, line)| format!("{:>width$} | {line}", index + 1)),
        )
        .collect()
}

/// The failure, and for a limit, where to raise it, as the report's tab says it.
fn failed(failure: &RunFailure) -> Vec<String> {
    let mut lines = vec![format!("Failed in {}: {}", failure.step, failure.message)];
    if let Some(limit) = &failure.limit {
        lines.push(limit.advice());
    }
    lines
}

/// `1,250 rows, 61.4 KB in 0.1 s`.
fn amounts(rows: u64, bytes: u64, elapsed_ms: u64) -> String {
    format!(
        "{} rows, {} in {}",
        grouped(rows),
        ByteCount(bytes),
        seconds(elapsed_ms)
    )
}

/// `1.2 s`, as the report's tab writes a time.
fn seconds(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use std::time::Duration;

    #[test]
    fn keeps_a_live_line_under_the_finished_steps() {
        let mut human =
            HumanLines::new(Vec::new(), Vec::new(), TypedValues::new()).with_live_line();
        let events = [
            RunEvent::Progress {
                step: Step::Source {
                    id: "lines".to_owned(),
                },
                rows: 100,
                bytes: 1_000,
                elapsed_ms: 1_000,
                batch: None,
                busy: None,
            },
            RunEvent::SourceDone {
                id: "lines".to_owned(),
                rows: 246,
                bytes: 6_500,
                first_row_ms: Some(1),
                elapsed_ms: 1_500,
                table: Vec::new(),
                warnings: Vec::new(),
                batches: None,
                phases: None,
            },
            RunEvent::Progress {
                step: Step::Combine,
                rows: 13,
                bytes: 500,
                elapsed_ms: 2_000,
                batch: None,
                busy: None,
            },
            RunEvent::Finished { elapsed_ms: 2_100 },
        ];
        for event in &events {
            human.event(&event.clone().at(Duration::ZERO));
        }
        let err = String::from_utf8(human.err).unwrap();
        assert_eq!(
            err,
            "\r\x1b[2KSource lines: 100 rows, 1.0 KB, 1 s\
             \r\x1b[2KSource lines: 246 rows, 6.5 KB in 1.5 s\n\
             \r\x1b[2KCombine: 13 rows, 500 bytes, 2 s\
             \r\x1b[2KFinished in 2.1 s.\n"
        );
    }

    #[test]
    fn prints_a_prql_combine_steps_sql_under_its_error() {
        let started = |combine_sql: Option<&str>| RunEvent::Started {
            report: "Sales by month".to_owned(),
            sources: Vec::new(),
            combine: true,
            combine_sql: combine_sql.map(str::to_owned),
            version: None,
            combine_limits: None,
            timeout_ms: None,
            machine: None,
        };
        let failed = RunEvent::Failed(RunFailure::new(
            Step::Combine,
            "Binder Error: column \"amont\" not found, LINE 2",
        ));
        let (_, err) = lines(
            &[],
            &[
                started(Some("SELECT\n  amont\nFROM\n  lines")),
                failed.clone(),
            ],
        );
        assert_eq!(
            err,
            "Sales by month\n\
             Failed in the combine step: Binder Error: column \"amont\" not found, LINE 2\n\
             The combine step's SQL, compiled from its PRQL:\n\
             1 | SELECT\n\
             2 |   amont\n\
             3 | FROM\n\
             4 |   lines\n"
        );
        // A combine step in SQL has its SQL in the file.
        let (_, err) = lines(&[], &[started(None), failed]);
        assert!(!err.contains("compiled"), "{err}");
    }

    fn lines(values: &[(&str, &str)], events: &[RunEvent]) -> (String, String) {
        let values = values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let mut human = HumanLines::new(Vec::new(), Vec::new(), values);
        for event in events {
            human.event(&event.clone().at(Duration::ZERO));
        }
        let HumanLines { out, err, .. } = human;
        (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn tells_a_finished_run_and_lists_its_files() {
        let events = [
            RunEvent::Started {
                report: "Sales by category".to_owned(),
                sources: Vec::new(),
                combine: true,
                combine_sql: None,
                version: None,
                combine_limits: None,
                timeout_ms: None,
                machine: None,
            },
            RunEvent::Progress {
                step: Step::Combine,
                rows: 3,
                bytes: 30,
                elapsed_ms: 1_000,
                batch: None,
                busy: None,
            },
            RunEvent::SourceDone {
                id: "lines".to_owned(),
                rows: 1_250,
                bytes: 61_400,
                first_row_ms: Some(1),
                elapsed_ms: 120,
                table: Vec::new(),
                warnings: vec!["amount has more digits than DuckDB keeps".to_owned()],
                batches: Some(3),
                phases: None,
            },
            RunEvent::SourceAttached {
                id: "prices".to_owned(),
                path: PathBuf::from("prices.duckdb"),
            },
            RunEvent::CombineDone {
                rows: 27,
                bytes: 2_000,
                first_row_ms: Some(1),
                elapsed_ms: 40,
                peak_memory: None,
                peak_temp_size: None,
                query_ms: None,
                plan: None,
            },
            RunEvent::OutputWritten(WrittenFile {
                path: PathBuf::from("/out/sales-2026-10.xlsx"),
                rows: 27,
                size: 9_000,
                elapsed_ms: 3,
                warnings: vec!["2 cells of Amount written as text".to_owned()],
            }),
            RunEvent::OutputWritten(WrittenFile {
                path: PathBuf::from("/out/sales-2026-10.csv"),
                rows: 27,
                size: 1_500,
                elapsed_ms: 1,
                warnings: Vec::new(),
            }),
            RunEvent::Finished { elapsed_ms: 420 },
        ];
        let (out, err) = lines(&[("since", "2026-10-01"), ("until", "2026-11-01")], &events);
        assert_eq!(out, "/out/sales-2026-10.xlsx\n/out/sales-2026-10.csv\n");
        assert_eq!(
            err,
            "Sales by category (since = 2026-10-01, until = 2026-11-01)\n\
             Source lines: 1,250 rows, 61.4 KB in 0.1 s, 3 batches\n\
             Warning, source lines: amount has more digits than DuckDB keeps\n\
             Source prices: attached, read in place\n\
             Combine: 27 rows, 2.0 KB in 0.0 s\n\
             Warning, /out/sales-2026-10.xlsx: 2 cells of Amount written as text\n\
             Finished in 0.4 s.\n"
        );
    }

    #[test]
    fn tells_where_to_raise_the_limit_a_run_stopped_at() {
        use dbrover_report::{LimitHit, LimitSetIn};

        let limit = |key: &str, set_in| {
            RunEvent::Failed(RunFailure::at_limit(
                Step::Source {
                    id: "lines".to_owned(),
                },
                "it returned more than 10 rows",
                LimitHit {
                    key: key.to_owned(),
                    value: "10 rows".to_owned(),
                    raise_in: "the [[source]] \"lines\"".to_owned(),
                    set_in,
                },
            ))
        };
        let cases = [
            (
                RunEvent::Failed(RunFailure::new(Step::Report, "since needs a value")),
                "Failed in the report: since needs a value\n",
            ),
            (
                limit("max_rows", None),
                "Failed in source \"lines\": it returned more than 10 rows\n\
                 It stopped at max_rows = 10 rows. To allow more, raise max_rows in the \
                 [[source]] \"lines\" of the report's file, or its default for every report in \
                 Settings › Reports.\n",
            ),
            (
                limit("max_rows", Some(LimitSetIn::File)),
                "Failed in source \"lines\": it returned more than 10 rows\n\
                 It stopped at max_rows = 10 rows, set in the [[source]] \"lines\" of the \
                 report's file. To allow more, raise it there.\n",
            ),
            (
                limit("max_rows", Some(LimitSetIn::Settings)),
                "Failed in source \"lines\": it returned more than 10 rows\n\
                 It stopped at max_rows = 10 rows, the default in Settings › Reports, since the \
                 report's file doesn't set it. To allow more, raise it there for every report, \
                 or set max_rows in the [[source]] \"lines\" of the report's file for this one.\n",
            ),
            (
                limit("continue_on_new_sheet", None),
                "Failed in source \"lines\": it returned more than 10 rows\n\
                 It stopped at 10 rows. To go on in a new sheet, set continue_on_new_sheet = \
                 true in the [[source]] \"lines\" of the report's file.\n",
            ),
            (RunEvent::Cancelled, "Cancelled. No file was written.\n"),
        ];
        for (event, expected) in cases {
            let (out, err) = lines(&[], &[event]);
            assert_eq!(out, "");
            assert_eq!(err, expected);
        }
    }

    #[test]
    fn names_a_report_without_parameters_alone() {
        let started = RunEvent::Started {
            report: "Stock".to_owned(),
            sources: Vec::new(),
            combine: false,
            combine_sql: None,
            version: None,
            combine_limits: None,
            timeout_ms: None,
            machine: None,
        };
        assert_eq!(lines(&[], &[started]).1, "Stock\n");
        assert_eq!(
            super::started("Stock", &TypedValues::new(), Some(&BlobId::of(b"hello\n"))),
            "Stock, version ce01362"
        );
    }
}
