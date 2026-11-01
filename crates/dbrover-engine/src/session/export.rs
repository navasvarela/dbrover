//! Exporting every row of a statement: running it again on the console's connection and
//! writing each page to the file as it arrives, so the rows are never all in memory.

use std::time::Instant;

use dbrover_core::{CancelHandle, DriverError, ResultCursor, Row, describe_error};
use dbrover_export::{ExportColumn, ExportWriter};
use dbrover_sql::{ParameterValues, RowSql};

use crate::export_file::create_writer;
use dbrover_store::{HistoryEntry, HistoryOutcome, HistorySource};
use jiff::Timestamp;

use super::guard::{self, ReadOnly};
use super::parameters::Sendable;
use super::query::PAGE_SIZE;
use super::{Disconnected, Worker, cancel_running_statement, cost};
use crate::{Event, ExportAll, RunPurpose, TableExport};

/// Why an export stopped short. The partial file is removed either way.
enum Stopped {
    Cancelled,
    Failed(String),
}

impl Stopped {
    fn message(&self) -> String {
        match self {
            Self::Cancelled => "Export cancelled".to_owned(),
            Self::Failed(message) => message.clone(),
        }
    }
}

/// Exports every row of a table: its `SELECT *`, written as [`write_all`] writes a
/// statement's rows.
pub(super) async fn write_table(
    worker: &mut Worker<'_>,
    export: TableExport,
) -> Result<(), Disconnected> {
    let TableExport {
        run,
        table,
        path,
        spec,
    } = export;
    let Some(dialect) = worker.context.dialect() else {
        worker.context.send(Event::ExportFinished {
            exporter: worker.context.owner.exporter(),
            path,
            result: Err(worker
                .context
                .needs_sql("its tables can't be exported here")),
        });
        return Ok(());
    };
    let sql = RowSql::new(dialect, table.as_table()).select_all();
    let statement = ExportAll {
        run,
        sql,
        parameters: ParameterValues::default(),
        path,
        spec,
    };
    write_all(worker, statement).await
}

/// Runs the export's statement, writes its rows, and reports [`Event::ExportFinished`] to
/// whoever the session works for.
pub(super) async fn write_all(
    worker: &mut Worker<'_>,
    export: ExportAll,
) -> Result<(), Disconnected> {
    let started_at = Timestamp::now();
    let started = Instant::now();
    let dialect = worker.context.dialect();
    let sendable = Sendable::new(&export.sql, dialect, &export.parameters, RunPurpose::Run)
        .map_err(|missing| missing.to_string());
    let written = match &sendable {
        Ok(sendable) => match read_only_check(worker, &sendable.checked).await? {
            Ok(()) => match cost::held_scan(worker, &sendable.checked).await? {
                Ok(None) => export_rows(worker, &export, sendable).await?,
                Ok(Some(scan)) => Err(Stopped::Failed(cost::refused(&scan, worker.context.name()))),
                Err(error) => Err(Stopped::Failed(describe_error(&error))),
            },
            Err(refused) => Err(Stopped::Failed(refused)),
        },
        Err(missing) => Err(Stopped::Failed(missing.clone())),
    };
    let (rows, outcome) = match &written {
        Ok(rows) => (Some(*rows as u64), HistoryOutcome::Ok),
        Err(Stopped::Cancelled) => (None, HistoryOutcome::Cancelled),
        Err(Stopped::Failed(_)) => (None, HistoryOutcome::Error),
    };
    worker.context.record(HistoryEntry {
        data_source: worker.context.data_source.id,
        executed_at: started_at,
        sql: sendable.map_or_else(|_| export.sql.clone(), |sendable| sendable.recorded),
        duration: Some(started.elapsed()),
        rows,
        outcome,
        source: HistorySource::Export,
    });
    let result = written.map_err(|stopped| stopped.message());
    match &result {
        Ok(rows) => tracing::info!(
            data_source = %worker.context.name(),
            "exported {rows} rows to {}",
            export.path.display()
        ),
        Err(error) => {
            tracing::warn!(data_source = %worker.context.name(), "export stopped: {error}")
        }
    }
    worker.context.send(Event::ExportFinished {
        exporter: worker.context.owner.exporter(),
        path: export.path,
        result,
    });
    Ok(())
}

/// On a read-only data source, refuses a statement that isn't a read, and opens the read-only
/// transaction the statement runs in; the session rolls it back once the export is done.
async fn read_only_check(
    worker: &mut Worker<'_>,
    sql: &str,
) -> Result<Result<(), String>, Disconnected> {
    let Some(read_only) = worker.context.read_only else {
        return Ok(Ok(()));
    };
    if let Err(refusal) = ReadOnly::check(sql, worker.context.language(), worker.context.name()) {
        return Ok(Err(refusal));
    }
    Ok(guard::open(worker, read_only)
        .await?
        .map_err(|error| describe_error(&error)))
}

/// Pages through the statement's rows into the file. Returns how many were written.
async fn export_rows(
    worker: &mut Worker<'_>,
    export: &ExportAll,
    sendable: &Sendable,
) -> Result<Result<usize, Stopped>, Disconnected> {
    let run = Some(export.run);
    let execute = sendable.execute(worker.connection);
    let mut cursor = match worker
        .signals
        .guard(execute, run, worker.cancel_handle)
        .await?
    {
        Ok(cursor) => cursor,
        Err(error) => return Ok(Err(stopped(worker, export, &error))),
    };
    let dialect = worker.context.dialect();
    let columns: Vec<ExportColumn> = cursor
        .columns()
        .iter()
        .map(|column| ExportColumn::typed_by(column, dialect))
        .collect();
    let (path, spec) = (export.path.clone(), export.spec.clone());
    let created =
        blocking(move || create_writer(&path, &spec, &columns).map_err(|e| describe_error(&e)))
            .await;
    let mut writer = match created {
        Ok(writer) => writer,
        Err(error) => {
            discard(worker.cancel_handle, cursor).await;
            return Ok(Err(Stopped::Failed(error)));
        }
    };
    loop {
        let page = cursor.next_page(PAGE_SIZE);
        let rows = match worker
            .signals
            .guard(page, run, worker.cancel_handle)
            .await?
        {
            Ok(rows) => rows,
            Err(error) => {
                // The statement already failed; whatever finishing reports adds nothing.
                let _ = cursor.finish().await;
                writer.abandon();
                return Ok(Err(stopped(worker, export, &error)));
            }
        };
        writer = match write_page(writer, rows).await {
            Ok(writer) => writer,
            Err(error) => {
                discard(worker.cancel_handle, cursor).await;
                return Ok(Err(Stopped::Failed(error)));
            }
        };
        worker.context.send(Event::ExportProgress {
            exporter: worker.context.owner.exporter(),
            rows: writer.rows(),
        });
        if cursor.is_exhausted() {
            break;
        }
    }
    let finished = worker
        .signals
        .guard(cursor.finish(), run, worker.cancel_handle)
        .await?;
    if let Err(error) = finished {
        writer.abandon();
        return Ok(Err(stopped(worker, export, &error)));
    }
    let rows = writer.rows();
    match blocking(move || writer.finish().map_err(|e| describe_error(&e))).await {
        Ok(()) => Ok(Ok(rows)),
        Err(error) => Ok(Err(Stopped::Failed(error))),
    }
}

/// Writes a page on the blocking pool, giving the writer back, or abandoning it on failure.
async fn write_page(mut writer: ExportWriter, rows: Vec<Row>) -> Result<ExportWriter, String> {
    blocking(move || match writer.write(&rows) {
        Ok(()) => Ok(writer),
        Err(error) => {
            writer.abandon();
            Err(describe_error(&error))
        }
    })
    .await
}

/// Runs file work on the blocking pool, so a slow disk never holds up the session's task.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| Err(describe_error(&error)))
}

/// Stops the statement and drops its unread rows.
async fn discard(cancel_handle: &dyn CancelHandle, cursor: Box<dyn ResultCursor>) {
    cancel_running_statement(cancel_handle).await;
    // An error here is the cancellation just requested.
    let _ = cursor.finish().await;
}

/// Why the statement stopped: cancelled by the user, or its error.
fn stopped(worker: &Worker<'_>, export: &ExportAll, error: &DriverError) -> Stopped {
    if worker.signals.is_cancelled(export.run) {
        return Stopped::Cancelled;
    }
    Stopped::Failed(match error {
        DriverError::Statement { source, .. } => describe_error(source.as_ref()),
        other => describe_error(other),
    })
}
