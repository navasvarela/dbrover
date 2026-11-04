//! The reports `dbrover mcp` lists and runs for an MCP client (`docs/reporting.md`, "Finishing
//! RP4"): `dbrover report list`'s listing, and runs as `dbrover report run` makes them, in the
//! worker with every guardrail, kept in the history as MCP's, their files going where the app's
//! go until a report has a folder of its own.

use std::path::PathBuf;

use dbrover_engine::WorkerRun;
use dbrover_mcp::{ReportListing, ReportRunner, RunningReport};
use dbrover_report::{Plan, RunEvent, RunFailure, Step, TypedValues};
use dbrover_store::{AppPaths, ReportName, RunOrigin};

use crate::report_command::history::RunHistory;
use crate::report_command::{self, list};
use crate::reports;

pub struct McpReports {
    pub paths: AppPaths,
    /// Where a run's files go: the user's Downloads folder.
    pub folder: PathBuf,
}

impl ReportRunner for McpReports {
    fn list(&self) -> Result<Vec<ReportListing>, String> {
        let settings =
            report_command::report_settings(&self.paths).map_err(|problems| problems.join("; "))?;
        let defaults = reports::limit_defaults(&settings);
        let listed = list::reports(&self.paths, &defaults).map_err(|error| error.to_string())?;
        Ok(listed
            .into_iter()
            .map(|report| ReportListing {
                name: report.name.to_string(),
                title: report.title,
                description: report.description,
                params: report
                    .params
                    .into_iter()
                    .map(|param| (param.name, param.param_type))
                    .collect(),
                data_sources: report.data_sources,
                problems: report.problems,
            })
            .collect())
    }

    fn start(
        &self,
        report: &str,
        values: TypedValues,
    ) -> Result<Box<dyn RunningReport>, Vec<String>> {
        let name = ReportName::new(report)
            .ok_or_else(|| vec![format!("{report:?} can't be a report's name")])?;
        let path = self.paths.report_files().path(&name);
        let prepared = report_command::prepare(&self.paths, &path, &values, Some(&self.folder))?;
        let history = RunHistory::start(
            &self.paths,
            &path,
            values,
            prepared.runs_kept,
            RunOrigin::Mcp,
        );
        let run = report_command::start_worker(&self.paths, &prepared)
            .map_err(|error| vec![format!("could not start the worker: {error}")])?;
        Ok(Box::new(McpRun {
            run,
            plan: prepared.plan,
            history,
            ended: false,
        }))
    }
}

/// A run for an MCP client, kept in the history once it ends.
struct McpRun {
    run: WorkerRun,
    /// To say who set a limit the run stops at.
    plan: Plan,
    history: Option<RunHistory>,
    ended: bool,
}

impl RunningReport for McpRun {
    fn next(&mut self) -> Option<RunEvent> {
        if self.ended {
            return None;
        }
        let mut event = self.run.wait_next().unwrap_or_else(|| {
            // `WorkerRun` ends every run with an event of its own; this is a bug, not an end.
            RunEvent::Failed(RunFailure::new(
                Step::Worker,
                "the run's events stopped before it ended",
            ))
        });
        if let RunEvent::Failed(failure) = &mut event {
            failure.place_limit(&self.plan);
        }
        if let Some(history) = &mut self.history {
            history.event(&event);
        }
        if event.ends_run() {
            self.ended = true;
            if let Some(history) = self.history.take()
                && let Err(problem) = history.keep()
            {
                tracing::warn!("the run isn't kept in the history: {problem}");
            }
        }
        Some(event)
    }

    fn stopper(&self) -> Box<dyn Fn() + Send + Sync> {
        let stopper = self.run.stopper();
        Box::new(move || stopper.cancel())
    }
}
