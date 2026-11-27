//! SQLite and DuckDB dump and restore themselves, inside DbRover (W17g, `docs/dump-restore.md`):
//! no program to find, run or give a password to.
//!
//! - **SQLite:** a dump is `VACUUM INTO`, a consistent copy of the database written while
//!   others read and write it; a restore copies a dump into a new file.
//! - **DuckDB:** a dump is `EXPORT DATABASE` to a folder (the schema, a load script, a Parquet
//!   or CSV file per table); a restore creates a new DuckDB file and runs `IMPORT DATABASE`
//!   on it.
//!
//! Both write to a `.partial` name renamed once they have ended well, as a program's dump
//! does, and are recorded in the history once.

use std::path::{Path, PathBuf};
use std::time::Instant;

use dbrover_core::{
    Connection, DataSource, DuckDbConfig, DuckDbDatabase, DumpEngine, DumpFormat, Quoting,
    RestoreTarget, describe_error,
};
use dbrover_dump::DumpFileKind;
use dbrover_store::{HistoryEntry, HistoryOutcome, HistorySource};
use tokio::sync::oneshot;

use crate::dumps::{self, DumpDone, DumpFailure, DumpRun};
use crate::open::{Connector, TypedSecrets, open};
use crate::passwords::Prompting;
use crate::program_run::{ProgramServices, remove_any, size_of};
use crate::restores::{RestoreDone, RestoreRun};

/// Dumps `run` with the engine's own statement.
pub(crate) async fn dump(
    run: &DumpRun,
    engine: DumpEngine,
    data_source: &DataSource,
    services: &ProgramServices,
    cancelled: oneshot::Receiver<()>,
) -> Result<DumpDone, DumpFailure> {
    let final_path = run.request.file.clone();
    let partial = dumps::partial_path(&final_path);
    dumps::refuse_existing(&final_path, &partial)?;
    let statement = dump_statement(engine, run.request.format, &partial)?;
    let mut connection = connect(data_source, services).await?;
    let started = Instant::now();
    let executed_at = jiff::Timestamp::now();
    tracing::info!(data_source = %data_source.name, "{statement}");
    let ran = run_cancellable(connection.as_mut(), &statement, cancelled).await;
    let result = dumps::finish(ran, &partial, &final_path, started.elapsed());
    services.history.record(dumps::history_entry(
        data_source,
        &run.request,
        statement,
        executed_at,
        &result,
    ));
    result
}

/// `VACUUM INTO '…'` or `EXPORT DATABASE '…' (FORMAT …)`, the path written as a literal.
fn dump_statement(
    engine: DumpEngine,
    format: DumpFormat,
    to: &Path,
) -> Result<String, DumpFailure> {
    let path = Quoting::STANDARD.string_literal(&to.to_string_lossy());
    match (engine, format) {
        (DumpEngine::Sqlite, DumpFormat::DatabaseFile) => Ok(format!("VACUUM INTO {path}")),
        (DumpEngine::DuckDb, DumpFormat::Parquet) => {
            Ok(format!("EXPORT DATABASE {path} (FORMAT parquet)"))
        }
        (DumpEngine::DuckDb, DumpFormat::Csv) => Ok(format!("EXPORT DATABASE {path} (FORMAT csv)")),
        _ => Err(DumpFailure::Arguments(dbrover_dump::ArgumentError::Format(
            engine,
        ))),
    }
}

async fn connect(
    data_source: &DataSource,
    services: &ProgramServices,
) -> Result<Box<dyn Connection>, DumpFailure> {
    let connector = Connector {
        secrets: &services.secrets,
        tunnels: &services.tunnels,
        commands: &services.commands,
        prompting: Prompting::Ask(&services.passwords),
    };
    open(data_source, &TypedSecrets::default(), connector)
        .await
        .map_err(|error| DumpFailure::Connect(describe_error(&error)))
}

/// Runs `statement`, asking the database to stop it when `cancelled` fires.
async fn run_cancellable(
    connection: &mut dyn Connection,
    statement: &str,
    cancelled: oneshot::Receiver<()>,
) -> Result<(), DumpFailure> {
    let cancel = connection.cancel_handle();
    let mut cancel_asked = false;
    let ran = {
        let run = async {
            let cursor = connection.execute(statement).await?;
            cursor.finish().await
        };
        tokio::pin!(run);
        tokio::select! {
            ran = &mut run => ran,
            _ = cancelled => {
                cancel_asked = true;
                if let Err(error) = cancel.cancel().await {
                    tracing::warn!("could not cancel {statement}: {error}");
                }
                run.await
            }
        }
    };
    match ran {
        Ok(_) if cancel_asked => Err(DumpFailure::Cancelled),
        Ok(_) => Ok(()),
        Err(_) if cancel_asked => Err(DumpFailure::Cancelled),
        Err(error) => Err(DumpFailure::Files(describe_error(&error))),
    }
}

/// Restores `run`'s dump into a new file: a copy of a SQLite file, or a new DuckDB database
/// importing an export.
pub(crate) async fn restore(
    run: &RestoreRun,
    engine: DumpEngine,
    kind: DumpFileKind,
    data_source: &DataSource,
    services: &ProgramServices,
) -> Result<RestoreDone, DumpFailure> {
    let RestoreTarget::NewDatabase(name) = &run.request.target else {
        return Err(DumpFailure::Files(format!(
            "{engine} restores into a new file only, never over one"
        )));
    };
    let target = PathBuf::from(name);
    let partial = dumps::partial_path(&target);
    dumps::refuse_existing(&target, &partial)?;
    let started = Instant::now();
    let executed_at = jiff::Timestamp::now();
    let (statement, restored) = match (engine, kind) {
        (DumpEngine::Sqlite, DumpFileKind::SqliteFile) => {
            let from = run.request.file.clone();
            let to = partial.clone();
            let copied = tokio::task::spawn_blocking(move || std::fs::copy(from, to)).await;
            let copied = match copied {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(error)) => Err(DumpFailure::Files(describe_error(&error))),
                Err(error) => Err(DumpFailure::Files(error.to_string())),
            };
            ("copy".to_owned(), copied)
        }
        (DumpEngine::DuckDb, DumpFileKind::DuckDbExport) => {
            let statement = format!(
                "IMPORT DATABASE {}",
                Quoting::STANDARD.string_literal(&run.request.file.to_string_lossy())
            );
            let imported = import(&partial, &statement).await;
            (statement, imported)
        }
        _ => {
            return Err(DumpFailure::Arguments(
                dbrover_dump::ArgumentError::OtherEngine {
                    file: kind.engine().unwrap_or(engine),
                    server: engine,
                },
            ));
        }
    };
    let result = restored
        .and_then(|()| {
            std::fs::rename(&partial, &target)
                .map_err(|error| DumpFailure::Files(describe_error(&error)))
        })
        .map(|()| RestoreDone {
            database: target.display().to_string(),
            created: true,
            elapsed: started.elapsed(),
        });
    if result.is_err()
        && let Err(error) = remove_any(&partial)
    {
        tracing::warn!("could not remove {}: {error}", partial.display());
    }
    let shown = match engine {
        DumpEngine::Sqlite => format!(
            "copy {} to {} ({})",
            run.request.file.display(),
            target.display(),
            dbrover_core::ByteSize(size_of(&run.request.file))
        ),
        _ => statement,
    };
    services.history.record(HistoryEntry {
        data_source: data_source.id,
        executed_at,
        sql: shown,
        duration: result.as_ref().ok().map(|done| done.elapsed),
        rows: None,
        outcome: if result.is_ok() {
            HistoryOutcome::Ok
        } else {
            HistoryOutcome::Error
        },
        source: HistorySource::Restore {
            restored: match &result {
                Ok(_) => format!(
                    "Restored {} into a new file {}",
                    run.request.file.display(),
                    target.display()
                ),
                Err(failure) => format!(
                    "Failed to restore {}: {failure}",
                    run.request.file.display()
                ),
            },
        },
    });
    result
}

/// Creates a DuckDB database at `path` and imports an export into it.
async fn import(path: &Path, statement: &str) -> Result<(), DumpFailure> {
    let files = |error: dbrover_core::DriverError| DumpFailure::Files(describe_error(&error));
    dbrover_driver_duckdb::create_database(path)
        .await
        .map_err(files)?;
    let config = DuckDbConfig::new(DuckDbDatabase::File {
        path: path.to_owned(),
        read_only: false,
    });
    let mut connection = dbrover_driver_duckdb::connect(&config)
        .await
        .map_err(files)?;
    let imported = async {
        let cursor = connection.execute(statement).await?;
        cursor.finish().await
    }
    .await;
    // Closed before the rename, so the file is whole on disk and DuckDB's lock is gone.
    drop(connection);
    imported.map(|_| ()).map_err(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_each_engines_own_statement() {
        let to = Path::new("/d/it's.partial");
        assert_eq!(
            dump_statement(DumpEngine::Sqlite, DumpFormat::DatabaseFile, to).unwrap(),
            "VACUUM INTO '/d/it''s.partial'"
        );
        assert_eq!(
            dump_statement(DumpEngine::DuckDb, DumpFormat::Csv, to).unwrap(),
            "EXPORT DATABASE '/d/it''s.partial' (FORMAT csv)"
        );
        assert!(dump_statement(DumpEngine::Sqlite, DumpFormat::Parquet, to).is_err());
    }
}
