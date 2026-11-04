//! The query history database, owned by one thread from the blocking pool. Requests queue
//! up in order, so a search sent after a run has finished sees that run's statements.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::PathBuf;

use dbrover_core::{DataSourceId, describe_error};
use dbrover_export::ExportSpec;
use dbrover_store::{
    History, HistoryEntry, HistoryError, HistoryFile, HistoryQuery, HistorySettings, ReportName,
    ReportRun,
};
use jiff::Timestamp;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::Event;
use crate::events::EventSink;

/// The most entries a search returns. Plenty to scroll through; narrowing the search finds
/// anything older.
const SEARCH_LIMIT: usize = 500;

enum HistoryRequest {
    Record(HistoryEntry),
    Search(HistoryQuery),
    Prune(HistorySettings),
    Export(HistoryExport),
    Runs {
        report: ReportName,
        change: RunsChange,
        listed: usize,
    },
}

/// What to do to a report's runs before listing them.
enum RunsChange {
    None,
    Record {
        run: Box<ReportRun>,
        kept: NonZeroU32,
    },
    Clear,
    /// The report was renamed from this name: its runs move to the new one.
    RenameFrom(ReportName),
}

/// A search to write to a file, as [`crate::Command::ExportHistory`] asks.
pub(crate) struct HistoryExport {
    pub(crate) query: HistoryQuery,
    pub(crate) path: PathBuf,
    pub(crate) spec: ExportSpec,
    pub(crate) names: HashMap<DataSourceId, String>,
}

/// A handle for queueing history work. Cheap to clone into sessions.
#[derive(Clone)]
pub(crate) struct HistoryLog {
    requests: UnboundedSender<HistoryRequest>,
}

impl HistoryLog {
    /// Starts the history thread. It runs until every handle is dropped; await the returned
    /// task to know that queued entries were written.
    pub(crate) fn start(file: HistoryFile, events: EventSink) -> (Self, JoinHandle<()>) {
        let (requests, receiver) = mpsc::unbounded_channel();
        let task = tokio::task::spawn_blocking(move || serve(&file, receiver, &events));
        (Self { requests }, task)
    }

    pub(crate) fn record(&self, entry: HistoryEntry) {
        self.send(HistoryRequest::Record(entry));
    }

    /// Answered by [`Event::HistoryFound`].
    pub(crate) fn search(&self, query: HistoryQuery) {
        self.send(HistoryRequest::Search(query));
    }

    /// Answered by [`Event::HistoryExported`]. On the history thread like everything else, so
    /// it sees every statement recorded before it, and statements recorded meanwhile wait.
    pub(crate) fn export(&self, export: HistoryExport) {
        self.send(HistoryRequest::Export(export));
    }

    /// Keeps `run`, then answers with its report's newest `listed` runs, by
    /// [`Event::ReportRunsFound`].
    pub(crate) fn record_run(&self, run: ReportRun, kept: NonZeroU32, listed: usize) {
        self.send(HistoryRequest::Runs {
            report: run.report.clone(),
            change: RunsChange::Record {
                run: Box::new(run),
                kept,
            },
            listed,
        });
    }

    /// Answered by [`Event::ReportRunsFound`].
    pub(crate) fn list_runs(&self, report: ReportName, listed: usize) {
        self.send(HistoryRequest::Runs {
            report,
            change: RunsChange::None,
            listed,
        });
    }

    /// Deletes every run of `report`, then answers with none, by [`Event::ReportRunsFound`].
    pub(crate) fn clear_runs(&self, report: ReportName) {
        self.send(HistoryRequest::Runs {
            report,
            change: RunsChange::Clear,
            listed: 0,
        });
    }

    /// Moves `from`'s runs to `to`, a report renamed, then answers with `to`'s newest
    /// `listed`, by [`Event::ReportRunsFound`].
    pub(crate) fn rename_runs(&self, from: ReportName, to: ReportName, listed: usize) {
        self.send(HistoryRequest::Runs {
            report: to,
            change: RunsChange::RenameFrom(from),
            listed,
        });
    }

    /// Deletes what `retention` no longer keeps. Logged, not reported: nobody is waiting.
    pub(crate) fn prune(&self, retention: HistorySettings) {
        if retention != HistorySettings::default() {
            self.send(HistoryRequest::Prune(retention));
        }
    }

    fn send(&self, request: HistoryRequest) {
        // Fails only once the thread has stopped, which happens during shutdown.
        if self.requests.send(request).is_err() {
            tracing::debug!("dropped a history request after the history stopped");
        }
    }
}

fn serve(file: &HistoryFile, mut requests: UnboundedReceiver<HistoryRequest>, events: &EventSink) {
    let history = file.open().map_err(|error| {
        let error = describe_error(&error);
        tracing::error!("query history is unavailable: {error}");
        error
    });
    while let Some(request) = requests.blocking_recv() {
        match request {
            HistoryRequest::Record(entry) => {
                if let Ok(history) = &history {
                    record(history, &entry);
                }
            }
            HistoryRequest::Search(query) => {
                let result = match &history {
                    Ok(history) => history
                        .search(&query, SEARCH_LIMIT)
                        .map_err(|error| describe_error(&error)),
                    Err(error) => Err(error.clone()),
                };
                events.send(Event::HistoryFound { query, result });
            }
            HistoryRequest::Export(export) => {
                let result = match &history {
                    Ok(history) => history
                        .export(&export.query, (&export.path, &export.spec), &export.names)
                        .map_err(|error| describe_error(&error)),
                    Err(error) => Err(error.clone()),
                };
                events.send(Event::HistoryExported {
                    path: export.path,
                    result,
                });
            }
            HistoryRequest::Prune(retention) => {
                if let Ok(history) = &history {
                    prune(history, &retention);
                }
            }
            HistoryRequest::Runs {
                report,
                change,
                listed,
            } => {
                let result = match &history {
                    Ok(history) => runs(history, &report, change, listed)
                        .map_err(|error| describe_error(&error)),
                    Err(error) => Err(error.clone()),
                };
                events.send(Event::ReportRunsFound { report, result });
            }
        }
    }
}

/// Losing one history entry isn't worth interrupting anyone for, so it is only logged.
fn record(history: &History, entry: &HistoryEntry) {
    if let Err(error) = history.record(entry) {
        tracing::warn!(
            "could not record a statement in the history: {}",
            describe_error(&error)
        );
    }
}

/// Makes `change` to `report`'s runs, then reads its newest `listed`.
fn runs(
    history: &History,
    report: &ReportName,
    change: RunsChange,
    listed: usize,
) -> Result<Vec<ReportRun>, HistoryError> {
    match change {
        RunsChange::None => {}
        RunsChange::Record { run, kept } => history.record_run(&run, kept)?,
        RunsChange::Clear => {
            history.clear_runs(report)?;
        }
        RunsChange::RenameFrom(from) => {
            history.rename_runs(&from, report)?;
        }
    }
    history.runs(report, listed)
}

fn prune(history: &History, retention: &HistorySettings) {
    match history.prune(retention, Timestamp::now()) {
        Ok(0) => {}
        Ok(deleted) => tracing::info!("pruned {deleted} statements from the history"),
        Err(error) => tracing::warn!("could not prune the history: {}", describe_error(&error)),
    }
}
