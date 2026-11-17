//! The report tools (`docs/reporting.md`, "Finishing RP4"): `list_reports`, for every project,
//! and `run_report`, for a project that allows running reports. A project sees only the
//! reports whose data sources are all its own, as it sees only its own data sources.
//!
//! The server doesn't run reports itself: the app hands it a [`ReportRunner`], which runs one
//! in `dbrover report-worker` as `dbrover report run` does, under the report's own limits.

use std::collections::BTreeMap;
use std::sync::Arc;

use dbrover_insight::{explanation, json};
use dbrover_report::{HintId, RunEvent, RunFailure, TypedValues};
use dbrover_store::ReportRun;
use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::server::{DbRoverMcp, ToolError};

/// Lists and runs the reports of the settings folder the server reads.
pub trait ReportRunner: Send + Sync {
    /// Every report in the reports folder, each read and planned.
    ///
    /// # Errors
    ///
    /// Why the folder can't be read.
    fn list(&self) -> Result<Vec<ReportListing>, String>;

    /// Starts `report`, by its name in the folder, with `values`.
    ///
    /// # Errors
    ///
    /// What keeps it from starting: the report, its values, the worker.
    fn start(
        &self,
        report: &str,
        values: TypedValues,
    ) -> Result<Box<dyn RunningReport>, Vec<String>>;

    /// Starts an explanation of `report` with `values` (RS8e): the worker plans it, answers
    /// one [`RunEvent::Explained`], and writes no file. The app builds the request, since
    /// what explaining the combine step needs — the columns the last finished run loaded —
    /// comes from the history the app reads for it.
    ///
    /// # Errors
    ///
    /// What keeps it from starting, as [`ReportRunner::start`] does.
    fn explain(
        &self,
        report: &str,
        values: TypedValues,
    ) -> Result<Box<dyn RunningReport>, Vec<String>>;
}

/// A run going on.
pub trait RunningReport: Send {
    /// Waits for the run's next event; `None` once the last one has come.
    fn next(&mut self) -> Option<RunEvent>;

    /// What stops the run, from any thread.
    fn stopper(&self) -> Box<dyn Fn() + Send + Sync>;
}

/// A report this project isn't shown, in the one wording every report tool refuses with. It
/// names the reports the project *is* shown and nothing else, so the answer never says whether
/// the asked-for report exists elsewhere.
pub(crate) fn no_such_report(name: &str, reports: &[ReportListing]) -> ToolError {
    if reports.is_empty() {
        return ToolError::invalid(format!(
            "the project has no report {name:?}, and no reports at all: a report is listed for a \
             project only when every data source it reads is one of the project's"
        ));
    }
    let names: Vec<&str> = reports.iter().map(|report| report.name.as_str()).collect();
    ToolError::invalid(format!(
        "the project has no report {name:?}; it has: {}",
        names.join(", ")
    ))
}

/// A report as the reports folder holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportListing {
    /// What `run_report` takes: the file's name without `.toml`.
    pub name: String,
    pub title: String,
    pub description: Option<String>,
    /// Each parameter's name and type, in the order the report asks for them.
    pub params: Vec<(String, String)>,
    /// The data sources its sources name, by name or id as the file writes them; `None` when
    /// the file can't be read, which shows it to no project.
    pub data_sources: Option<Vec<String>>,
    /// What keeps it from running; empty when it can.
    pub problems: Vec<String>,
    /// The hints its `[report] quiet_hints` silences, so the tools that answer hints can mark
    /// them without reading the file themselves (RS8c).
    pub quiet_hints: Vec<HintId>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(crate) struct ExplainReportArgs {
    /// The report's name, as list_reports gives it.
    report: String,
    /// Each parameter's value, as text, by name, as run_report takes them: the plans are
    /// explained with the values written in, so they are the plans those values would get.
    #[serde(default)]
    params: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(crate) struct RunReportArgs {
    /// The report's name, as list_reports gives it.
    report: String,
    /// Each parameter's value, as text, by name: a date as 2026-10-01, a timestamp as
    /// "2026-10-01 12:00".
    #[serde(default)]
    params: BTreeMap<String, String>,
}

impl DbRoverMcp {
    /// The reports this project sees: those whose every data source is one of its own.
    pub(crate) fn project_reports(&self, tool: &str) -> Result<Vec<ReportListing>, ToolError> {
        let Some(runner) = &self.inner.reports else {
            return Ok(Vec::new());
        };
        let listed = runner.list().map_err(|error| {
            self.inner.audit.refused(tool, "-", &error);
            ToolError::refused(format!("the reports can't be listed: {error}"))
        })?;
        let project = &self.inner.project;
        Ok(listed
            .into_iter()
            .filter(|report| {
                report.data_sources.as_ref().is_some_and(|names| {
                    names.iter().all(|name| project.data_source(name).is_some())
                })
            })
            .collect())
    }

    pub(crate) fn list_reports_json(&self) -> Result<Value, ToolError> {
        let tool = "list_reports";
        self.inner.audit.call(tool, None, None);
        let reports: Vec<Value> = self
            .project_reports(tool)?
            .iter()
            .map(|report| {
                json!({
                    "name": report.name,
                    "title": report.title,
                    "description": report.description,
                    "params": report.params.iter().map(|(name, param_type)| {
                        json!({"name": name, "type": param_type})
                    }).collect::<Vec<_>>(),
                    "data_sources": report.data_sources,
                    "can_run": report.problems.is_empty(),
                    "problems": report.problems,
                })
            })
            .collect();
        Ok(json!({
            "reports": reports,
            "may_run": self.inner.project.reports,
        }))
    }

    /// Explains the report, stopping the worker if the client cancels the call.
    ///
    /// Not behind **Run reports**: explaining runs no statement that reads data, so it is a
    /// read like `explain` is (`docs/reports-at-scale.md`, "RS8 against today's code").
    pub(crate) async fn explain_report_json(
        &self,
        args: ExplainReportArgs,
        cancelled: impl Future<Output = ()>,
    ) -> Result<Value, ToolError> {
        let tool = "explain_report";
        self.inner.audit.call(tool, None, None);
        let Some(runner) = &self.inner.reports else {
            let reason = "this settings folder has no reports";
            self.inner.audit.refused(tool, "-", reason);
            return Err(ToolError::refused(reason));
        };
        let (report, runs) = self.kept_runs(tool, &args.report)?;
        let runner = Arc::clone(runner);
        let name = report.name.clone();
        let values = args.params;
        let run = tokio::task::spawn_blocking(move || runner.explain(&name, values))
            .await
            .map_err(|error| ToolError::refused(error.to_string()))?
            .map_err(|problems| ToolError::invalid(problems.join("; ")))?;
        let events = collect(run, cancelled).await?;
        explanation_json(&events, &runs, &report.quiet_hints)
    }

    /// Runs the report, stopping it if the client cancels the call.
    pub(crate) async fn run_report_json(
        &self,
        args: RunReportArgs,
        cancelled: impl Future<Output = ()>,
    ) -> Result<Value, ToolError> {
        let tool = "run_report";
        self.inner.audit.call(tool, None, None);
        let (Some(runner), true) = (&self.inner.reports, self.inner.project.reports) else {
            let reason = "this project doesn't allow running reports; switch on Run reports in \
                          its MCP settings in DbRover";
            self.inner.audit.refused(tool, "-", reason);
            return Err(ToolError::refused(reason));
        };
        let reports = self.project_reports(tool)?;
        let Some(report) = reports.iter().find(|report| report.name == args.report) else {
            return Err(no_such_report(&args.report, &reports));
        };
        if !report.problems.is_empty() {
            return Err(ToolError::invalid(format!(
                "the report can't run: {}",
                report.problems.join("; ")
            )));
        }
        let runner = Arc::clone(runner);
        let name = report.name.clone();
        let values = args.params;
        let run = tokio::task::spawn_blocking(move || runner.start(&name, values))
            .await
            .map_err(|error| ToolError::refused(error.to_string()))?
            .map_err(|problems| ToolError::invalid(problems.join("; ")))?;
        let events = collect(run, cancelled).await?;
        Ok(outcome_json(&events, self.inner.project.limits.max_rows))
    }
}

/// Every event of a run or an explanation, read on a blocking thread; cancelling the call
/// stops the worker and takes the events it had already sent, which is how a cancelled run
/// still says what it managed.
async fn collect(
    mut run: Box<dyn RunningReport>,
    cancelled: impl Future<Output = ()>,
) -> Result<Vec<RunEvent>, ToolError> {
    let stop = run.stopper();
    let mut events = tokio::task::spawn_blocking(move || {
        let mut events = Vec::new();
        while let Some(event) = run.next() {
            events.push(event);
        }
        events
    });
    tokio::select! {
        events = &mut events => events,
        () = cancelled => {
            stop();
            events.await
        }
    }
    .map_err(|error| ToolError::refused(error.to_string()))
}

/// The explanation among `events`, with the hints its plans give: the same document
/// `dbrover report explain --json` writes, with a silenced hint marked rather than left out.
/// A worker that answered no explanation failed, and says why.
fn explanation_json(
    events: &[RunEvent],
    runs: &[ReportRun],
    quiet: &[HintId],
) -> Result<Value, ToolError> {
    let explained = events.iter().find_map(|event| match event {
        RunEvent::Explained(explanation) => Some(explanation.as_ref()),
        _ => None,
    });
    let Some(explanation) = explained else {
        let why = events
            .iter()
            .find_map(|event| match event {
                RunEvent::Failed(failure) => Some(failure.to_string()),
                RunEvent::Cancelled => Some("the explanation was cancelled".to_owned()),
                _ => None,
            })
            .unwrap_or_else(|| "the worker sent no explanation".to_owned());
        return Err(ToolError::refused(why));
    };
    let hints = explanation::hints(explanation, runs);
    Ok(json::explanation(explanation, &hints, quiet))
}

/// How a run ended, from its events: what each step measured, the files written, the preview's
/// first `max_rows` rows, or the failure and where to raise the limit it hit.
fn outcome_json(events: &[RunEvent], max_rows: usize) -> Value {
    let mut steps = Vec::new();
    let mut files = Vec::new();
    let mut preview = Value::Null;
    let mut kept = Value::Null;
    let mut end = json!({"outcome": "failed", "error": "the run's events stopped early"});
    for event in events {
        match event {
            RunEvent::SourceDone {
                id,
                rows,
                bytes,
                elapsed_ms,
                batches,
                parts,
                ..
            } => steps.push(json!({
                "step": format!("source {id}"),
                "rows": rows,
                "bytes": bytes,
                "elapsed_ms": elapsed_ms,
                "batches": batches,
                "ranges": parts,
            })),
            RunEvent::SourceAttached { id, .. } => {
                steps.push(json!({"step": format!("source {id}"), "attached": true}));
            }
            RunEvent::LakeRead { id, url, format } => steps.push(json!({
                "step": format!("source {id}"),
                "lake": url,
                "format": format,
            })),
            RunEvent::LakeScanned {
                id,
                requests,
                bytes,
            } => steps.push(json!({
                "step": format!("source {id}"),
                "bucket_requests": requests,
                "bucket_bytes": bytes,
            })),
            RunEvent::CombineDone {
                rows,
                bytes,
                elapsed_ms,
                peak_memory,
                peak_temp_size,
                ..
            } => steps.push(json!({
                "step": "combine",
                "rows": rows,
                "bytes": bytes,
                "elapsed_ms": elapsed_ms,
                "peak_memory": peak_memory,
                "peak_temp_size": peak_temp_size,
            })),
            RunEvent::Preview(shown) => {
                preview = json!({
                    "columns": shown.columns,
                    "rows": shown.rows.iter().take(max_rows).collect::<Vec<_>>(),
                    "more": shown.more || shown.rows.len() > max_rows,
                });
            }
            RunEvent::OutputWritten(file) => files.push(json!({
                "path": file.path,
                "rows": file.rows,
                "size": file.size,
                "warnings": file.warnings,
            })),
            RunEvent::Finished { elapsed_ms } => {
                end = json!({"outcome": "finished", "elapsed_ms": elapsed_ms});
            }
            RunEvent::Failed(failure) => end = failed_json(failure),
            RunEvent::Cancelled => end = json!({"outcome": "cancelled"}),
            RunEvent::Resumed(resumed) => {
                for file in &resumed.files {
                    files.push(json!({
                        "path": file.path,
                        "rows": file.rows,
                        "size": file.size,
                        "warnings": file.warnings,
                    }));
                }
            }
            RunEvent::WorkKept(folder) => {
                kept = json!({"folder": folder.folder, "ranges": folder.ranges});
            }
            // The worker's memory, the steps' phases and each range's numbers are RS8's, with
            // the run profile.
            RunEvent::Started { .. }
            | RunEvent::Progress { .. }
            | RunEvent::PartDone(_)
            | RunEvent::WorkerUsage { .. }
            | RunEvent::Explained(_) => {}
        }
    }
    if let Value::Object(end) = &mut end {
        end.insert("steps".to_owned(), Value::Array(steps));
        end.insert("files".to_owned(), Value::Array(files));
        end.insert("preview".to_owned(), preview);
        if !kept.is_null() {
            end.insert("kept".to_owned(), kept);
        }
    }
    end
}

fn failed_json(failure: &RunFailure) -> Value {
    json!({
        "outcome": "failed",
        "step": failure.step.to_string(),
        "error": failure.message,
        "limit": failure.limit.as_ref().map(|limit| json!({
            "key": limit.key,
            "value": limit.value,
            "set_in": limit.set_in,
            "advice": limit.advice(),
        })),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use dbrover_report::{LimitHit, LimitSetIn, Preview, Step, WrittenFile};

    use super::*;

    #[test]
    fn says_how_a_run_ended_with_what_it_wrote() {
        let finished = [
            RunEvent::CombineDone {
                rows: 3,
                bytes: 30,
                first_row_ms: Some(1),
                elapsed_ms: 5,
                peak_memory: Some(1 << 20),
                peak_temp_size: Some(0),
                query_ms: None,
                plan: None,
            },
            RunEvent::Preview(Preview {
                columns: vec!["n".to_owned()],
                rows: vec![vec![Some("1".to_owned())], vec![Some("2".to_owned())]],
                more: false,
            }),
            RunEvent::OutputWritten(WrittenFile {
                output: None,
                path: PathBuf::from("/out/n.csv"),
                rows: 3,
                size: 8,
                elapsed_ms: 1,
                warnings: Vec::new(),
            }),
            RunEvent::Finished { elapsed_ms: 40 },
        ];
        let json = outcome_json(&finished, 1);
        assert_eq!(json["outcome"], "finished");
        assert_eq!(json["files"][0]["path"], "/out/n.csv");
        assert_eq!(json["steps"][0]["step"], "combine");
        assert_eq!(json["preview"]["rows"].as_array().unwrap().len(), 1);
        assert_eq!(json["preview"]["more"], true);

        let failed = [RunEvent::Failed(RunFailure::at_limit(
            Step::Source {
                id: "lines".to_owned(),
            },
            "too many rows",
            LimitHit {
                key: "max_rows".to_owned(),
                value: "10 rows".to_owned(),
                raise_in: "the [[source]] \"lines\"".to_owned(),
                set_in: Some(LimitSetIn::File),
            },
        ))];
        let json = outcome_json(&failed, 1);
        assert_eq!(json["outcome"], "failed");
        assert_eq!(json["step"], "source \"lines\"");
        assert_eq!(json["limit"]["set_in"], "file");
        assert!(json["files"].as_array().unwrap().is_empty());
    }
}
