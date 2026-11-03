//! A report's source on a real data source (`docs/reporting.md`, RP1d and RP3d): its query
//! checked to only read, run in the engine's read-only mode with its parameters bound, refused
//! if the cost guard would ask, and read a page at a time until the source's deadline. A query
//! in a language without SQL arrives with its parameters already written in.
//!
//! Opening a source's statement:
//! 1. bind the parameters, and check the statement as it will run (values as literals);
//! 2. connect as `dbrover mcp` does, with nobody to ask for a password, the connection itself
//!    read-only where the engine has such a mode;
//! 3. open the engine's read-only transaction, with the source's timeout where the engine
//!    takes one;
//! 4. ask the cost guard, then start the statement.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dbrover_core::{
    CancelHandle, Connection, CostGuard, ReadPace, ResultCursor, ResultShape, Row, describe_error,
};
use dbrover_export::ExportColumn;
use dbrover_report::{LimitHit, SourceError, SourceOpener, SourceReader, SourceStatement};
use dbrover_sql::Prepared;
use dbrover_store::SecretStore;

use crate::read_only::{self, Guard, ReadOnlyMode, Storage};
use crate::{PasswordCommands, Tunnels, open_data_source};

/// Rows asked for at a time: a few hundred kilobytes of a typical row, few enough round trips
/// to keep a million-row source fast, and what the caps are counted in steps of.
const PAGE_ROWS: usize = 1_000;

/// How long a cancelled statement gets to stop before the source gives up on it. The worker
/// exits after a failed run, which ends the connection either way.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// What opening a source needs to connect, as the app and `dbrover mcp` connect.
#[derive(Clone, Copy)]
pub struct SourceAccess<'a> {
    pub secrets: &'a Arc<dyn SecretStore>,
    pub tunnels: &'a Tunnels,
    pub commands: &'a PasswordCommands,
}

impl SourceOpener for SourceAccess<'_> {
    type Reader = ReportSource;

    async fn open(&self, statement: &SourceStatement<'_>) -> Result<ReportSource, SourceError> {
        ReportSource::open(statement, *self).await
    }
}

/// A report's source, open and running its statement.
pub struct ReportSource {
    connection: Box<dyn Connection>,
    cursor: Option<Box<dyn ResultCursor>>,
    columns: Vec<ExportColumn>,
    shape: ResultShape,
    mode: Option<ReadOnlyMode>,
    cancel: Arc<dyn CancelHandle>,
    deadline: Instant,
    timeout: Duration,
    id: String,
}

impl ReportSource {
    /// Connects to the statement's data source and starts it.
    ///
    /// # Errors
    ///
    /// [`SourceError`] saying why: a statement that doesn't only read, a connection that
    /// fails, the cost guard, or the database's own error.
    pub async fn open(
        statement: &SourceStatement<'_>,
        access: SourceAccess<'_>,
    ) -> Result<Self, SourceError> {
        let source = statement.source;
        let language = source.language;
        let (checked, prepared) = prepare(statement)?;
        read_only::check(&checked, language).map_err(|refusal| {
            SourceError::new(format!(
                "{}; a report's source only reads, so it isn't sent",
                refusal.reason()
            ))
        })?;

        let data_source = read_only::connection_config(source.data_source.clone());
        let config = &data_source.config;
        let mut connection = open_data_source(
            &data_source,
            access.secrets,
            access.tunnels,
            access.commands,
        )
        .await
        .map_err(|error| {
            SourceError::new(format!(
                "could not connect to {:?}: {}{}",
                data_source.name,
                describe_error(&error),
                password_hint(config.driver().uses_password())
            ))
        })?;
        let mode = match read_only::guard(config.driver(), Storage::of(config)) {
            Guard::Session(mode) => Some(mode),
            Guard::Login | Guard::CheckOnly => None,
        };
        let cancel = connection.cancel_handle();
        let opened = async {
            if let Some(mode) = mode {
                // The database's own limit, where it takes one, comes a grace period after the
                // source's, so the source's watchdog stops the statement first and says which
                // limit it hit; the database's is there if the cancel doesn't get through.
                let backstop = source.limits.timeout + CANCEL_GRACE;
                read_only::begin(connection.as_mut(), mode, Some(backstop))
                    .await
                    .map_err(|error| SourceError::new(describe_error(&error)))?;
            }
            check_cost(connection.as_mut(), config.cost_guard(), &checked).await?;
            if prepared.values.is_empty() {
                connection.execute(&prepared.sql).await
            } else {
                connection
                    .execute_bound(&prepared.sql, &prepared.values)
                    .await
            }
            .map_err(|error| SourceError::new(describe_error(&error)))
        };
        let deadline = statement.deadline;
        let cursor = match watch(opened, deadline, &cancel, &source.id, source.limits.timeout).await
        {
            Ok(cursor) => cursor,
            Err(error) => {
                close(connection, None, mode).await;
                return Err(error);
            }
        };
        let columns = cursor
            .columns()
            .iter()
            .map(|column| ExportColumn::typed_by(column, language.dialect()))
            .collect();
        let shape = cursor.shape();
        Ok(Self {
            connection,
            cursor: Some(cursor),
            columns,
            shape,
            mode,
            cancel,
            deadline,
            timeout: source.limits.timeout,
            id: source.id.clone(),
        })
    }
}

/// The statement as checked, every value written in, and as sent: in SQL, with its values
/// bound where the driver binds them.
fn prepare(statement: &SourceStatement<'_>) -> Result<(String, Prepared), SourceError> {
    let Some(dialect) = statement.source.language.dialect() else {
        return Ok((
            statement.query.clone(),
            Prepared::unchanged(&statement.query),
        ));
    };
    let values = &statement.parameters;
    let checked = dbrover_sql::inline(&statement.query, dialect, values)
        .map_err(|missing| SourceError::new(missing.to_string()))?;
    let prepared = dbrover_sql::bind(&statement.query, dialect, values)
        .map_err(|missing| SourceError::new(missing.to_string()))?;
    Ok((checked.sql, prepared))
}

impl SourceReader for ReportSource {
    fn columns(&self) -> &[ExportColumn] {
        &self.columns
    }

    fn shape(&self) -> ResultShape {
        self.shape
    }

    async fn next_page(&mut self) -> Result<Option<Vec<Row>>, SourceError> {
        let Some(cursor) = self.cursor.as_mut().filter(|cursor| !cursor.is_exhausted()) else {
            return Ok(None);
        };
        let page = async {
            cursor
                .next_page(PAGE_ROWS)
                .await
                .map_err(|error| SourceError::new(describe_error(&error)))
        };
        let page = watch(page, self.deadline, &self.cancel, &self.id, self.timeout).await?;
        Ok((!page.is_empty()).then_some(page))
    }

    /// Ends the statement, rolls back the read-only transaction and disconnects. What fails
    /// here is only logged: the run is over either way.
    async fn close(self) {
        close(self.connection, self.cursor, self.mode).await;
    }
}

/// Runs `work` until `deadline`, then cancels the statement and fails with the timeout as the
/// limit hit.
async fn watch<T>(
    work: impl Future<Output = Result<T, SourceError>>,
    deadline: Instant,
    cancel: &Arc<dyn CancelHandle>,
    id: &str,
    timeout: Duration,
) -> Result<T, SourceError> {
    let mut work = std::pin::pin!(work);
    let remaining = deadline.saturating_duration_since(Instant::now());
    tokio::select! {
        outcome = &mut work => return outcome,
        () = tokio::time::sleep(remaining) => {}
    }
    if let Err(error) = cancel.cancel().await {
        tracing::warn!(
            "could not cancel the report's statement: {}",
            describe_error(&error)
        );
    }
    // Whatever it returns now, it ended because of the cancel.
    let _ = tokio::time::timeout(CANCEL_GRACE, work).await;
    let value = friendly(timeout);
    Err(SourceError {
        message: format!(
            "the query ran past {value}, the source's timeout, and was cancelled; filter in its \
             query, or raise timeout"
        ),
        limit: Some(Box::new(LimitHit {
            key: "timeout".to_owned(),
            value,
            raise_in: format!("the [[source]] {id:?}"),
        })),
    })
}

/// `10m`, `1h 30m`, as a report file writes a duration.
fn friendly(duration: Duration) -> String {
    match jiff::SignedDuration::try_from(duration) {
        Ok(duration) => format!("{duration:#}"),
        Err(_) => format!("{}s", duration.as_secs()),
    }
}

/// Refuses `sql` when the data source's cost guard holds what it would cost: nobody can be
/// asked during a run.
async fn check_cost(
    connection: &mut dyn Connection,
    guard: Option<CostGuard>,
    sql: &str,
) -> Result<(), SourceError> {
    let Some(guard) = guard.filter(CostGuard::is_on) else {
        return Ok(());
    };
    let estimate = connection
        .estimate_cost(sql)
        .await
        .map_err(|error| SourceError::new(describe_error(&error)))?;
    match estimate {
        Some(estimate) if guard.holds(&estimate, ReadPace::Whole) => {
            Err(SourceError::new(format!(
                "{} The data source asks before that, and a report's run can't be asked: filter on \
             the key in the query, or change the data source's cost guard",
                estimate.describe()
            )))
        }
        _ => Ok(()),
    }
}

/// What a failed connection may be missing, on an engine that signs in with a password.
fn password_hint(uses_password: bool) -> &'static str {
    if uses_password {
        ". A report's run can't ask for a password: save it in the data source's settings, or \
         use a password command or a cloud sign-in"
    } else {
        ""
    }
}

async fn close(
    mut connection: Box<dyn Connection>,
    cursor: Option<Box<dyn ResultCursor>>,
    mode: Option<ReadOnlyMode>,
) {
    if let Some(cursor) = cursor {
        if !cursor.is_exhausted() {
            // Stops the server producing rows nobody will read; the cursor is dropped below.
            if let Err(error) = connection.cancel_handle().cancel().await {
                tracing::warn!(
                    "could not cancel the report's statement: {}",
                    describe_error(&error)
                );
            }
        }
        if let Err(error) = cursor.finish().await {
            tracing::debug!(
                "the report's statement ended with: {}",
                describe_error(&error)
            );
        }
    }
    if let Some(mode) = mode
        && let Err(error) = read_only::rollback(connection.as_mut(), mode).await
    {
        tracing::warn!(
            "could not roll back the report's read: {}",
            describe_error(&error)
        );
    }
    if let Err(error) = connection.close().await {
        tracing::warn!(
            "could not close the report's connection: {}",
            describe_error(&error)
        );
    }
}
