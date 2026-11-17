//! The report tools that read a report's run history (`docs/reports-at-scale.md`, "RS8
//! against today's code"): `report_runs`, `report_profile` and `compare_report_runs`.
//!
//! The server reads the history itself, through [`dbrover_store::History`], rather than
//! asking the app for it: a kept run is a row of `history.sqlite`, which the server already
//! opens for its audit log, so these tools answer with no app running. What only the app can
//! do — read the reports folder, start the worker — stays behind
//! [`ReportRunner`](crate::ReportRunner).
//!
//! Every tool here resolves its report through `project_reports` first, so a project reads the
//! runs only of the reports it is shown, and a report it may not see is refused in the words
//! `run_report` uses rather than answered with an empty history, which would itself say
//! whether the report exists.

use dbrover_insight::insights::{self, Dominant, millis, origin, outcome};
use dbrover_insight::{compare, funnel, json};
use dbrover_report::HintId;
use dbrover_store::{ReportName, ReportRun, RunId, RunOutcome};
use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json as json_value};

use crate::reports::ReportListing;
use crate::server::{DbRoverMcp, ToolError};

/// How many of the newest runs `report_runs` answers when the call doesn't say, and the most
/// it will. A whole history would fill a client's context: a profile is tens of kilobytes to a
/// megabyte and `runs_kept` can be large (`docs/reports-at-scale.md`, "Risks"). What a run
/// gets here is a summary; `report_profile` answers one run in full.
const RUNS_DEFAULT: usize = 10;
const RUNS_MOST: usize = 100;

/// Runs read from the history, so a call can name a run that isn't among the newest and a
/// step can be judged against its median: more than Settings › Reports lets a report keep, as
/// `dbrover report runs` reads.
const RUNS_READ: usize = 10_000;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(crate) struct ReportRunsArgs {
    /// The report's name, as list_reports gives it.
    report: String,
    /// How many of the newest runs to answer; 10 by default, 100 at most.
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(crate) struct ReportProfileArgs {
    /// The report's name, as list_reports gives it.
    report: String,
    /// The run's id, as report_runs gives it; the newest kept run when left out.
    #[serde(default)]
    run: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(crate) struct CompareReportRunsArgs {
    /// The report's name, as list_reports gives it.
    report: String,
    /// The earlier run's id, as report_runs gives it.
    before: i64,
    /// The later run's id, as report_runs gives it.
    after: i64,
}

impl DbRoverMcp {
    /// The report `name` names among those this project sees, with its kept runs, newest
    /// first. Refuses a name the project isn't shown, listing the ones it is.
    pub(crate) fn kept_runs(
        &self,
        tool: &str,
        name: &str,
    ) -> Result<(ReportListing, Vec<ReportRun>), ToolError> {
        let reports = self.project_reports(tool)?;
        let Some(report) = reports.iter().find(|report| report.name == name) else {
            let names: Vec<&str> = reports.iter().map(|report| report.name.as_str()).collect();
            return Err(ToolError::invalid(format!(
                "the project has no report {name:?}; it has: {}",
                names.join(", ")
            )));
        };
        // The folder gave the name, so this holds; a name it can't hold reaches no file.
        let Some(report_name) = ReportName::new(&report.name) else {
            return Err(ToolError::refused(format!(
                "{:?} can't be a report's name",
                report.name
            )));
        };
        let runs = self
            .inner
            .history
            .open()
            .and_then(|history| history.runs(&report_name, RUNS_READ))
            .map_err(|error| {
                let reason = format!("the run history can't be read: {error}");
                self.inner.audit.refused(tool, "-", &reason);
                ToolError::refused(reason)
            })?;
        Ok((report.clone(), runs))
    }

    /// The run `id` names among `runs`, or the newest of them when it names none. The reason
    /// says how many there are, so a client that asked for a run the history has dropped
    /// knows to list them again.
    pub(crate) fn one_run<'r>(
        &self,
        report: &ReportListing,
        runs: &'r [ReportRun],
        id: Option<i64>,
    ) -> Result<&'r ReportRun, ToolError> {
        let found = match id {
            Some(id) => runs.iter().find(|run| run.id == Some(RunId(id))),
            None => runs.first(),
        };
        found.ok_or_else(|| {
            ToolError::invalid(match id {
                Some(id) => format!(
                    "{:?} has no kept run {id}; report_runs lists the {} it has",
                    report.name,
                    runs.len()
                ),
                None => format!("{:?} has kept no run yet", report.name),
            })
        })
    }

    pub(crate) fn report_runs_json(&self, args: ReportRunsArgs) -> Result<Value, ToolError> {
        let tool = "report_runs";
        self.inner.audit.call(tool, None, None);
        let (report, runs) = self.kept_runs(tool, &args.report)?;
        let slower = compare::slower_than_usual(&runs);
        let wanted = args.limit.unwrap_or(RUNS_DEFAULT).clamp(1, RUNS_MOST);
        let listed: Vec<Value> = runs
            .iter()
            .enumerate()
            .take(wanted)
            .map(|(place, run)| summary(run, &slower, place))
            .collect();
        Ok(json_value!({
            "report": report.name,
            "title": report.title,
            "kept": runs.len(),
            "runs": listed,
            "dominant_throughout": insights::dominant_throughout(&runs).map(dominant),
            "quiet_hints": report.quiet_hints.iter().copied().map(HintId::as_str).collect::<Vec<_>>(),
        }))
    }

    pub(crate) fn report_profile_json(&self, args: ReportProfileArgs) -> Result<Value, ToolError> {
        let tool = "report_profile";
        self.inner.audit.call(tool, None, None);
        let (report, runs) = self.kept_runs(tool, &args.report)?;
        let run = self.one_run(&report, &runs, args.run)?;
        let mut profile = json::profile(run, &report.quiet_hints);
        // The funnel is MCP's addition, not part of the document `--profile` writes: that
        // document's shape is RS1's, and a reader of it computes the funnel itself. An
        // assistant can't, so it comes with the profile here.
        if let Value::Object(fields) = &mut profile {
            fields.insert("funnel".to_owned(), data_funnel(run));
        }
        Ok(profile)
    }

    pub(crate) fn compare_report_runs_json(
        &self,
        args: CompareReportRunsArgs,
    ) -> Result<Value, ToolError> {
        let tool = "compare_report_runs";
        self.inner.audit.call(tool, None, None);
        let (report, runs) = self.kept_runs(tool, &args.report)?;
        let before = self.one_run(&report, &runs, Some(args.before))?;
        let after = self.one_run(&report, &runs, Some(args.after))?;
        Ok(json::comparison(
            before,
            after,
            &compare::compare(before, after),
        ))
    }
}

/// A run a line: how it ended and what held it, without its steps.
fn summary(run: &ReportRun, slower: &[compare::Slower], place: usize) -> Value {
    let failed_at = match &run.outcome {
        RunOutcome::Failed(error) => Some(error.step.clone()),
        RunOutcome::Finished | RunOutcome::Cancelled => None,
    };
    let slower_steps: Vec<Value> = slower
        .iter()
        .filter(|found| found.run == place)
        .map(|found| json_value!({"step": found.step.to_string(), "times_its_median": found.times}))
        .collect();
    json_value!({
        "id": run.id,
        "started_at": run.started_at.to_string(),
        "elapsed_ms": millis(run),
        "outcome": outcome(run),
        "failed_at": failed_at,
        "origin": origin(run),
        "params": run.params,
        "version": run.version.map(|version| version.to_string()),
        "dominant": insights::dominant(run).map(dominant),
        "slower_than_usual": slower_steps,
        // Named `kept_work` rather than `can_resume`: the run kept a work folder, which the
        // app may since have discarded. Only the app knows whether it is still there.
        "kept_work": run.profile.kept,
    })
}

fn dominant(dominant: Dominant) -> Value {
    json_value!({"step": dominant.step.to_string(), "share": dominant.share})
}

/// The data funnel's numbers, a source a line: what its engine scanned, what it returned to
/// DbRover, what the combine used of it, and what the run wrote. The widest drop is where the
/// source's query should filter, which is what an assistant optimising a report looks for.
fn data_funnel(run: &ReportRun) -> Value {
    let Some(funnel) = funnel::funnel(run) else {
        return Value::Null;
    };
    json_value!({
        "written_rows": funnel.written_rows,
        "sources": funnel.lines.iter().map(|line| json_value!({
            "source": line.source,
            "scanned_bytes": line.scanned_bytes,
            "returned_rows": line.returned_rows,
            "returned_bytes": line.returned_bytes,
            "used_rows": line.used_rows,
        })).collect::<Vec<_>>(),
    })
}
