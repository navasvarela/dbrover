//! What `dbrover report …` was asked, read strictly: a cron line or a CI step is typed by hand,
//! and a typo should fail loudly rather than run something else.

use std::path::{MAIN_SEPARATOR, PathBuf};

use dbrover_report::TypedValues;
use dbrover_store::{ReportName, RunId};

use crate::version::parse_config_dir;

pub const USAGE: &str = "\
Usage: dbrover report run <report> [--param <name>=<value>]… [--output-dir <folder>]
                          [--json] [--progress] [--profile <file>] [--config-dir <folder>]
       dbrover report list [--json] [--config-dir <folder>]
       dbrover report runs <report> [--json] [--config-dir <folder>]
       dbrover report compare <report> <run> <run> [--json] [--config-dir <folder>]
       dbrover report explain <report> [--param <name>=<value>]… [--json]
                              [--config-dir <folder>]

list prints each report in the settings folder's reports folder: the name run takes, its
title and parameters, and what keeps it from running, if anything. With --json, an object a
line.

runs prints the runs the history keeps of a report in the reports folder, newest first: each
run's id, start, end, time and file version, and the step that held it longest. With --json,
each run's whole profile, an object a line.

compare prints two of those runs, by their ids, step by step: each step's time, rows, bytes
and peaks in both, the changes past half marked, and the parameters and limits that differ.
With --json, one object.

explain plans a report without running it: each source's statement explained by its engine,
with what its data source's cost guard estimates and whether a run would be refused, and the
combine step planned by DuckDB over empty tables of each source's columns, with the hints
its plan gives. Nothing reads a row. With --json, one object.

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
      --progress               On a terminal, keeps a line on standard error of what each
                               running step has read so far, how fast, and what it waits
                               on, rewritten each second.
      --profile <file>         Writes the run's profile to <file> as JSON once it ends: each
                               step's times, phases and limits, and the hints.
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
  6  the run was stopped: its own time limit, its stall_timeout without progress, more disk
     than its max_disk or the disk allows, Ctrl-C, SIGTERM or SIGHUP";

/// What the command line asks of `dbrover report`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Run(RunArgs),
    List(ListArgs),
    Runs(RunsArgs),
    Compare(CompareArgs),
    Explain(ExplainArgs),
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
    /// Whether to keep a live line of the running steps on a terminal.
    pub progress: bool,
    /// Where to write the run's profile once it ends.
    pub profile: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
}

/// `dbrover report explain`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainArgs {
    pub report: ReportArg,
    /// The parameters' values, as typed, each named once.
    pub values: TypedValues,
    pub style: OutputStyle,
    pub config_dir: Option<PathBuf>,
}

impl ExplainArgs {
    /// `None` when `--help` was asked for.
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = args.peekable();
        let mut report = None;
        let mut values = TypedValues::new();
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
            match arg.as_str() {
                "--help" | "-h" => return Ok(None),
                "--json" => style = OutputStyle::JsonLines,
                option if option.starts_with('-') => {
                    return Err(format!("unknown argument {option:?}"));
                }
                _ if report.is_some() => {
                    return Err(format!("{arg:?}: explain takes one report"));
                }
                _ => report = Some(ReportArg::new(&arg)?),
            }
        }
        let report =
            report.ok_or_else(|| "dbrover report explain needs a report: its name".to_owned())?;
        Ok(Some(Self {
            report,
            values,
            style,
            config_dir,
        }))
    }
}

/// `dbrover report runs`' arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunsArgs {
    pub report: ReportArg,
    pub style: OutputStyle,
    pub config_dir: Option<PathBuf>,
}

/// `dbrover report compare`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareArgs {
    pub report: ReportArg,
    pub before: RunId,
    pub after: RunId,
    pub style: OutputStyle,
    pub config_dir: Option<PathBuf>,
}

/// What `runs` and `compare` read before their own arguments: the report, the style and the
/// settings folder; `None` when `--help` was asked for.
struct HistoryArgs {
    words: Vec<String>,
    style: OutputStyle,
    config_dir: Option<PathBuf>,
}

impl HistoryArgs {
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = args.peekable();
        let mut words = Vec::new();
        let mut style = OutputStyle::Human;
        let mut config_dir = None;
        while let Some(arg) = args.next() {
            if let Some(folder) = parse_config_dir(&arg, &mut args) {
                config_dir = Some(folder?);
                continue;
            }
            match arg.as_str() {
                "--help" | "-h" => return Ok(None),
                "--json" => style = OutputStyle::JsonLines,
                option if option.starts_with('-') => {
                    return Err(format!("unknown argument {option:?}"));
                }
                _ => words.push(arg),
            }
        }
        Ok(Some(Self {
            words,
            style,
            config_dir,
        }))
    }
}

impl RunsArgs {
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let Some(HistoryArgs {
            words,
            style,
            config_dir,
        }) = HistoryArgs::parse(args)?
        else {
            return Ok(None);
        };
        match words.as_slice() {
            [report] => Ok(Some(Self {
                report: ReportArg::new(report)?,
                style,
                config_dir,
            })),
            [] => Err("dbrover report runs needs a report: its name".to_owned()),
            [_, extra, ..] => Err(format!("{extra:?}: runs takes one report")),
        }
    }
}

impl CompareArgs {
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let Some(HistoryArgs {
            words,
            style,
            config_dir,
        }) = HistoryArgs::parse(args)?
        else {
            return Ok(None);
        };
        let run_id = |word: &String| {
            word.parse::<RunId>()
                .map_err(|_| format!("{word:?} isn't a run's id; dbrover report runs lists them"))
        };
        match words.as_slice() {
            [report, before, after] => Ok(Some(Self {
                report: ReportArg::new(report)?,
                before: run_id(before)?,
                after: run_id(after)?,
                style,
                config_dir,
            })),
            _ => Err(
                "dbrover report compare needs a report and two runs' ids: <report> <run> <run>"
                    .to_owned(),
            ),
        }
    }
}

/// `dbrover report list`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListArgs {
    pub style: OutputStyle,
    pub config_dir: Option<PathBuf>,
}

impl ListArgs {
    /// `None` when `--help` was asked for.
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = args.peekable();
        let mut style = OutputStyle::Human;
        let mut config_dir = None;
        while let Some(arg) = args.next() {
            if let Some(folder) = parse_config_dir(&arg, &mut args) {
                config_dir = Some(folder?);
                continue;
            }
            match arg.as_str() {
                "--help" | "-h" => return Ok(None),
                "--json" => style = OutputStyle::JsonLines,
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(Some(Self { style, config_dir }))
    }
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
            Some("list") => match ListArgs::parse(args) {
                Ok(Some(list)) => Self::List(list),
                Ok(None) => Self::Help,
                Err(problem) => Self::Invalid(problem),
            },
            Some("runs") => match RunsArgs::parse(args) {
                Ok(Some(runs)) => Self::Runs(runs),
                Ok(None) => Self::Help,
                Err(problem) => Self::Invalid(problem),
            },
            Some("compare") => match CompareArgs::parse(args) {
                Ok(Some(compare)) => Self::Compare(compare),
                Ok(None) => Self::Help,
                Err(problem) => Self::Invalid(problem),
            },
            Some("explain") => match ExplainArgs::parse(args) {
                Ok(Some(explain)) => Self::Explain(explain),
                Ok(None) => Self::Help,
                Err(problem) => Self::Invalid(problem),
            },
            Some("--help" | "-h") => Self::Help,
            Some(other) => Self::Invalid(format!("unknown report command {other:?}")),
            None => Self::Invalid(
                "dbrover report needs a command: run, list, runs, compare or explain".to_owned(),
            ),
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
        let mut progress = false;
        let mut profile = None;
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
            if let Some(file) = option_value(&arg, &["--profile"], &mut args) {
                profile = Some(PathBuf::from(file?));
                continue;
            }
            match arg.as_str() {
                "--help" | "-h" => return Ok(None),
                "--json" => style = OutputStyle::JsonLines,
                "--progress" => progress = true,
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
            progress,
            profile,
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

    fn run_args(report: ReportArg) -> RunArgs {
        RunArgs {
            report,
            values: TypedValues::new(),
            output_dir: None,
            style: OutputStyle::Human,
            progress: false,
            profile: None,
            config_dir: None,
        }
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
            progress: false,
            profile: None,
            config_dir: None,
        })
    }

    fn invalid(problem: &str) -> Request {
        Request::Invalid(problem.to_owned())
    }

    #[test]
    fn reads_a_list() {
        let list = |style, config_dir: Option<&str>| {
            Request::List(ListArgs {
                style,
                config_dir: config_dir.map(PathBuf::from),
            })
        };
        let cases: Vec<(&[&str], Request)> = vec![
            (&["list"], list(OutputStyle::Human, None)),
            (
                &["list", "--json", "--config-dir", "/srv/dbrover"],
                list(OutputStyle::JsonLines, Some("/srv/dbrover")),
            ),
            (&["list", "--help"], Request::Help),
            (&["list", "sales"], invalid("unknown argument \"sales\"")),
        ];
        for (args, expected) in cases {
            assert_eq!(parse(args), expected, "{args:?}");
        }
    }

    #[test]
    fn reads_an_explain_with_its_parameters() {
        let cases: Vec<(&[&str], Request)> = vec![
            (
                &["explain", "sales", "-p", "since=2026-10-01", "--json"],
                Request::Explain(ExplainArgs {
                    report: named("sales"),
                    values: [("since".to_owned(), "2026-10-01".to_owned())]
                        .into_iter()
                        .collect(),
                    style: OutputStyle::JsonLines,
                    config_dir: None,
                }),
            ),
            (
                &["explain"],
                invalid("dbrover report explain needs a report: its name"),
            ),
            (
                &["explain", "sales", "other"],
                invalid("\"other\": explain takes one report"),
            ),
            (
                &["explain", "sales", "--output-dir", "out"],
                invalid("unknown argument \"--output-dir\""),
            ),
            (&["explain", "--help"], Request::Help),
        ];
        for (args, expected) in cases {
            assert_eq!(parse(args), expected, "{args:?}");
        }
    }

    #[test]
    fn reads_the_history_commands_and_a_profile_file() {
        let cases: Vec<(&[&str], Request)> = vec![
            (
                &["runs", "sales", "--json"],
                Request::Runs(RunsArgs {
                    report: named("sales"),
                    style: OutputStyle::JsonLines,
                    config_dir: None,
                }),
            ),
            (
                &[
                    "compare",
                    "sales",
                    "11",
                    "12",
                    "--config-dir",
                    "/srv/dbrover",
                ],
                Request::Compare(CompareArgs {
                    report: named("sales"),
                    before: RunId(11),
                    after: RunId(12),
                    style: OutputStyle::Human,
                    config_dir: Some("/srv/dbrover".into()),
                }),
            ),
            (
                &["compare", "sales", "11"],
                invalid(
                    "dbrover report compare needs a report and two runs' ids: <report> <run> \
                     <run>",
                ),
            ),
            (
                &["compare", "sales", "11", "newest"],
                invalid("\"newest\" isn't a run's id; dbrover report runs lists them"),
            ),
            (
                &["runs"],
                invalid("dbrover report runs needs a report: its name"),
            ),
            (&["runs", "sales", "--help"], Request::Help),
            (
                &["run", "sales", "--profile", "out/profile.json"],
                Request::Run(RunArgs {
                    profile: Some("out/profile.json".into()),
                    ..run_args(named("sales"))
                }),
            ),
            (
                &["run", "sales", "--profile"],
                invalid("--profile needs a value"),
            ),
        ];
        for (args, expected) in cases {
            assert_eq!(parse(args), expected, "{args:?}");
        }
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
                    "--progress",
                    "--config-dir=/srv/dbrover",
                ],
                Request::Run(RunArgs {
                    report: named("sales"),
                    values: TypedValues::new(),
                    output_dir: Some("/srv/out".into()),
                    style: OutputStyle::Human,
                    progress: true,
                    profile: None,
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
                    progress: false,
                    profile: None,
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
                    progress: false,
                    profile: None,
                    config_dir: None,
                }),
            ),
            (
                &["run", "sales", "--json=yes"],
                invalid("unknown argument \"--json=yes\""),
            ),
            (&["run", "sales", "--help"], Request::Help),
            (&["--help"], Request::Help),
            (
                &[],
                invalid("dbrover report needs a command: run, list, runs, compare or explain"),
            ),
            (&["lists"], invalid("unknown report command \"lists\"")),
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
