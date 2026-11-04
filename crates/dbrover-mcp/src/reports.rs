//! The report tools (`docs/reporting.md`, "Finishing RP4"): `list_reports`, for every project,
//! and `run_report`, for a project that allows running reports. A project sees only the
//! reports whose data sources are all its own, as it sees only its own data sources.
//!
//! The server doesn't run reports itself: the app hands it a [`ReportRunner`], which runs one
//! in `dbrover report-worker` as `dbrover report run` does, under the report's own limits.

use std::collections::BTreeMap;
use std::sync::Arc;

use dbrover_report::{RunEvent, RunFailure, TypedValues};
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
}

/// A run going on.
pub trait RunningReport: Send {
    /// Waits for the run's next event; `None` once the last one has come.
    fn next(&mut self) -> Option<RunEvent>;

    /// What stops the run, from any thread.
    fn stopper(&self) -> Box<dyn Fn() + Send + Sync>;
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
    fn project_reports(&self, tool: &str) -> Result<Vec<ReportListing>, ToolError> {
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
            let names: Vec<&str> = reports.iter().map(|report| report.name.as_str()).collect();
            return Err(ToolError::invalid(format!(
                "the project has no report {:?}; it has: {}",
                args.report,
                names.join(", ")
            )));
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
        let mut run = tokio::task::spawn_blocking(move || runner.start(&name, values))
            .await
            .map_err(|error| ToolError::refused(error.to_string()))?
            .map_err(|problems| ToolError::invalid(problems.join("; ")))?;
        let stop = run.stopper();
        let mut events = tokio::task::spawn_blocking(move || {
            let mut events = Vec::new();
            while let Some(event) = run.next() {
                events.push(event);
            }
            events
        });
        let events = tokio::select! {
            events = &mut events => events,
            () = cancelled => {
                stop();
                events.await
            }
        }
        .map_err(|error| ToolError::refused(error.to_string()))?;
        Ok(outcome_json(&events, self.inner.project.limits.max_rows))
    }
}

/// How a run ended, from its events: what each step measured, the files written, the preview's
/// first `max_rows` rows, or the failure and where to raise the limit it hit.
fn outcome_json(events: &[RunEvent], max_rows: usize) -> Value {
    let mut steps = Vec::new();
    let mut files = Vec::new();
    let mut preview = Value::Null;
    let mut end = json!({"outcome": "failed", "error": "the run's events stopped early"});
    for event in events {
        match event {
            RunEvent::SourceDone {
                id,
                rows,
                bytes,
                elapsed_ms,
                batches,
                ..
            } => steps.push(json!({
                "step": format!("source {id}"),
                "rows": rows,
                "bytes": bytes,
                "elapsed_ms": elapsed_ms,
                "batches": batches,
            })),
            RunEvent::SourceAttached { id, .. } => {
                steps.push(json!({"step": format!("source {id}"), "attached": true}));
            }
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
            RunEvent::Started { .. } | RunEvent::Progress { .. } => {}
        }
    }
    if let Value::Object(end) = &mut end {
        end.insert("steps".to_owned(), Value::Array(steps));
        end.insert("files".to_owned(), Value::Array(files));
        end.insert("preview".to_owned(), preview);
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
            },
            RunEvent::Preview(Preview {
                columns: vec!["n".to_owned()],
                rows: vec![vec![Some("1".to_owned())], vec![Some("2".to_owned())]],
                more: false,
            }),
            RunEvent::OutputWritten(WrittenFile {
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
