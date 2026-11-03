//! What `dbrover report …` was asked, read strictly: a cron line or a CI step is typed by hand,
//! and a typo should fail loudly rather than run something else.

use std::path::{MAIN_SEPARATOR, PathBuf};

use dbrover_report::TypedValues;
use dbrover_store::ReportName;

use crate::version::parse_config_dir;

pub const USAGE: &str = "\
Usage: dbrover report run <report> [--param <name>=<value>]… [--output-dir <folder>]
                          [--json] [--config-dir <folder>]

Runs a report without opening a window, for cron, a scheduled task or a CI job, under the
limits a run from the app has, and exits with a code that says how it ended.

<report> is a report's name in the settings folder's reports folder, as the app lists it
(sales-by-category), or the path to a report file (./sales-by-category.toml): a name ending
in .toml or holding a folder separator is a path.

  -p, --param <name>=<value>   A parameter's value, as the report's tab takes it: a date as
                               2026-10-01, a timestamp as \"2026-10-01 12:00\". Once each.
  -o, --output-dir <folder>    Where the files go, made if it doesn't exist. The current
                               folder without it.
      --json                   Writes the run's events to standard output as JSON Lines,
                               one object a line, the last saying how the run ended.
      --config-dir <folder>    The settings folder whose data sources, export formats and
                               report limits the run uses, as for the app.

Without --json, standard error tells the run's steps as they end, and standard output lists
the files written, their full paths a line each.

Exit codes:
  0  the report ran and wrote its files
  1  it failed otherwise: the combine query, an output, or the worker
  2  the command line is wrong
  3  the run couldn't start: the report file, a data source it names, its parameters, or the
     settings folder
  4  a source failed: connecting to its data source, or its query
  5  a guardrail stopped a step, such as max_rows; the message names the key to raise
  6  the run was stopped: its own time limit, Ctrl-C or SIGTERM";

/// What the command line asks of `dbrover report`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Run(RunArgs),
    Help,
    /// Arguments that can't be followed, with what was wrong.
    Invalid(String),
}

/// `dbrover report run`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    pub report: ReportArg,
    /// The parameters' values, as typed, each named once.
    pub values: TypedValues,
    /// `None` for the current folder.
    pub output_dir: Option<PathBuf>,
    pub style: OutputStyle,
    pub config_dir: Option<PathBuf>,
}

/// What the command prints as the run goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputStyle {
    /// Lines for a person, or cron's mail: steps on standard error, files on standard output.
    #[default]
    Human,
    /// The worker's events as JSON Lines on standard output, for a script or a CI step.
    JsonLines,
}

/// Which report to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportArg {
    /// A report in the settings folder's `reports/`, by the name the app lists it by.
    Name(ReportName),
    /// A report file anywhere.
    Path(PathBuf),
}

impl ReportArg {
    /// A path if `arg` ends in `.toml` or holds a folder separator, else a report's name.
    fn new(arg: &str) -> Result<Self, String> {
        let is_path = arg.ends_with(".toml") || arg.contains(['/', MAIN_SEPARATOR]);
        if is_path {
            return Ok(Self::Path(PathBuf::from(arg)));
        }
        ReportName::new(arg)
            .map(Self::Name)
            .ok_or_else(|| format!("{arg:?} is neither a report's name nor a report file's path"))
    }
}

impl Request {
    /// Reads what follows `dbrover report`.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Self {
        let mut args = args.into_iter();
        match args.next().as_deref() {
            Some("run") => match RunArgs::parse(args) {
                Ok(Some(run)) => Self::Run(run),
                Ok(None) => Self::Help,
                Err(problem) => Self::Invalid(problem),
            },
            Some("--help" | "-h") => Self::Help,
            Some(other) => Self::Invalid(format!("unknown report command {other:?}")),
            None => Self::Invalid("dbrover report needs a command: run".to_owned()),
        }
    }
}

impl RunArgs {
    /// `None` when `--help` was asked for.
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = args.peekable();
        let mut report = None;
        let mut values = TypedValues::new();
        let mut output_dir = None;
        let mut style = OutputStyle::Human;
        let mut config_dir = None;
        while let Some(arg) = args.next() {
            if let Some(folder) = parse_config_dir(&arg, &mut args) {
                config_dir = Some(folder?);
                continue;
            }
            if let Some(value) = option_value(&arg, &["--param", "-p"], &mut args) {
                let (name, value) = parameter(&value?)?;
                if values.insert(name.clone(), value).is_some() {
                    return Err(format!("--param {name} is given twice"));
                }
                continue;
            }
            if let Some(folder) = option_value(&arg, &["--output-dir", "-o"], &mut args) {
                output_dir = Some(PathBuf::from(folder?));
                continue;
            }
            match arg.as_str() {
                "--help" | "-h" => return Ok(None),
                "--json" => style = OutputStyle::JsonLines,
                option if option.starts_with('-') => {
                    return Err(format!("unknown argument {option:?}"));
                }
                _ if report.is_some() => {
                    return Err(format!("{arg:?}: a run takes one report"));
                }
                _ => report = Some(ReportArg::new(&arg)?),
            }
        }
        let report = report.ok_or("dbrover report run needs a report: its name or its file")?;
        Ok(Some(Self {
            report,
            values,
            output_dir,
            style,
            config_dir,
        }))
    }
}

/// The value of an option named one of `names`, if `arg` is it: the next argument for
/// `--name value`, or what follows the first `=` for `--name=value`.
fn option_value(
    arg: &str,
    names: &[&str],
    rest: &mut impl Iterator<Item = String>,
) -> Option<Result<String, String>> {
    let long = names.first()?;
    if names.contains(&arg) {
        return Some(
            rest.next()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("{long} needs a value")),
        );
    }
    let value = arg.strip_prefix(long)?.strip_prefix('=')?;
    Some(if value.is_empty() {
        Err(format!("{long} needs a value"))
    } else {
        Ok(value.to_owned())
    })
}

/// `since=2026-10-01` as its name and value; the value is everything after the first `=`.
fn parameter(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((name, value)) if !name.trim().is_empty() => {
            Ok((name.trim().to_owned(), value.to_owned()))
        }
        _ => Err(format!("--param {text:?} needs the form <name>=<value>")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Request {
        Request::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    fn named(name: &str) -> ReportArg {
        ReportArg::Name(ReportName::new(name).unwrap())
    }

    fn run(report: ReportArg, values: &[(&str, &str)]) -> Request {
        Request::Run(RunArgs {
            report,
            values: values
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            output_dir: None,
            style: OutputStyle::Human,
            config_dir: None,
        })
    }

    fn invalid(problem: &str) -> Request {
        Request::Invalid(problem.to_owned())
    }

    #[test]
    fn reads_a_run_and_refuses_what_it_does_not_know() {
        let cases: Vec<(&[&str], Request)> = vec![
            (
                &[
                    "run",
                    "sales-by-category",
                    "--param",
                    "since=2026-10-01",
                    "-p",
                    "until=2026-11-01",
                ],
                run(
                    named("sales-by-category"),
                    &[("since", "2026-10-01"), ("until", "2026-11-01")],
                ),
            ),
            (
                &["run", "--param=at=2026-10-01 12:00", "sales"],
                run(named("sales"), &[("at", "2026-10-01 12:00")]),
            ),
            // Everything after the first `=` is the value, and an empty text is a value.
            (
                &["run", "sales", "-p", "filter=a=b", "-p", "note="],
                run(named("sales"), &[("filter", "a=b"), ("note", "")]),
            ),
            (
                &["run", "examples/reports/sales-by-category.toml"],
                run(
                    ReportArg::Path("examples/reports/sales-by-category.toml".into()),
                    &[],
                ),
            ),
            (
                &["run", "sales.toml"],
                run(ReportArg::Path("sales.toml".into()), &[]),
            ),
            (
                &[
                    "run",
                    "sales",
                    "-o",
                    "/srv/out",
                    "--config-dir=/srv/dbrover",
                ],
                Request::Run(RunArgs {
                    report: named("sales"),
                    values: TypedValues::new(),
                    output_dir: Some("/srv/out".into()),
                    style: OutputStyle::Human,
                    config_dir: Some("/srv/dbrover".into()),
                }),
            ),
            (
                &["run", "--output-dir=out", "sales"],
                Request::Run(RunArgs {
                    report: named("sales"),
                    values: TypedValues::new(),
                    output_dir: Some("out".into()),
                    style: OutputStyle::Human,
                    config_dir: None,
                }),
            ),
            (
                &["run", "sales", "--json"],
                Request::Run(RunArgs {
                    report: named("sales"),
                    values: TypedValues::new(),
                    output_dir: None,
                    style: OutputStyle::JsonLines,
                    config_dir: None,
                }),
            ),
            (
                &["run", "sales", "--json=yes"],
                invalid("unknown argument \"--json=yes\""),
            ),
            (&["run", "sales", "--help"], Request::Help),
            (&["--help"], Request::Help),
            (&[], invalid("dbrover report needs a command: run")),
            (&["list"], invalid("unknown report command \"list\"")),
            (
                &["run"],
                invalid("dbrover report run needs a report: its name or its file"),
            ),
            (
                &["run", "sales", "other"],
                invalid("\"other\": a run takes one report"),
            ),
            (
                &["run", "sales", "--parm", "since=2026-10-01"],
                invalid("unknown argument \"--parm\""),
            ),
            (
                &["run", "sales", "--param", "since"],
                invalid("--param \"since\" needs the form <name>=<value>"),
            ),
            (
                &["run", "sales", "--param", "=2026-10-01"],
                invalid("--param \"=2026-10-01\" needs the form <name>=<value>"),
            ),
            (
                &["run", "sales", "--param"],
                invalid("--param needs a value"),
            ),
            (
                &["run", "sales", "-p", "since=1", "-p", "since=2"],
                invalid("--param since is given twice"),
            ),
            (
                &["run", "sales", "--output-dir="],
                invalid("--output-dir needs a value"),
            ),
            (
                &["run", "sales", "--config-dir"],
                invalid("--config-dir needs a folder"),
            ),
            (
                &["run", ".hidden"],
                invalid("\".hidden\" is neither a report's name nor a report file's path"),
            ),
        ];
        for (args, expected) in cases {
            assert_eq!(parse(args), expected, "{args:?}");
        }
    }
}
