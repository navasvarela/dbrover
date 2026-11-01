//! Noticing that a session's connection is gone, whichever request, cursor or ping found out,
//! and answering what was asked of a connection that is gone.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use dbrover_core::{
    CancelHandle, CatalogNode, CatalogPath, Cell, ChildrenPage, ChildrenQuery, ColumnInfo,
    Connection, DriverError, FullScan, Rejection, RelationName, ResultCursor, ResultShape,
    RoleDescription, Row, ServerInfo, TableConstraints, TableDescription, TableName,
    TransactionStatus, describe_error,
};

use super::{SessionContext, SessionOwner, SessionRequest};
use crate::{
    DdlFailure, DescribedTable, EditFailure, Event, ExportAll, ImportFailure, RunOutcome,
    StatementCheck, StatementFailure, TableExport,
};

/// Why the connection was lost, once a call has reported [`DriverError::Lost`]. Shared with
/// the cursors the connection hands out, since a cursor may be what finds out.
#[derive(Debug, Clone, Default)]
pub(super) struct Loss(Arc<Mutex<Option<String>>>);

impl Loss {
    pub(super) fn reason(&self) -> Option<String> {
        self.lock().clone()
    }

    /// Passes `result` on, noting the first loss it reports. The reason is the driver's own
    /// error, without `DriverError::Lost`'s "the connection was lost", which whoever shows
    /// the reason says in their own words.
    fn watch<T>(&self, result: Result<T, DriverError>) -> Result<T, DriverError> {
        if let Err(DriverError::Lost(source)) = &result {
            self.lock()
                .get_or_insert_with(|| describe_error(source.as_ref()));
        }
        result
    }

    /// A panic while holding the lock can't leave an `Option` half-written.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A session's connection, reporting every loss to its [`Loss`].
pub(super) struct Watched {
    connection: Box<dyn Connection>,
    loss: Loss,
}

impl Watched {
    pub(super) fn new(connection: Box<dyn Connection>, loss: Loss) -> Self {
        Self { connection, loss }
    }
}

#[async_trait]
impl Connection for Watched {
    fn server_info(&self) -> &ServerInfo {
        self.connection.server_info()
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        self.loss.watch(self.connection.ping().await)
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        self.loss.watch(self.connection.children(path).await)
    }

    async fn children_page(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        self.loss
            .watch(self.connection.children_page(path, query).await)
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        self.loss.watch(self.connection.definition(path).await)
    }

    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        let cursor = self.loss.watch(self.connection.execute(sql).await)?;
        Ok(Box::new(WatchedCursor {
            cursor,
            loss: self.loss.clone(),
        }))
    }

    async fn execute_bound(
        &mut self,
        sql: &str,
        values: &[Cell],
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        let cursor = self
            .loss
            .watch(self.connection.execute_bound(sql, values).await)?;
        Ok(Box::new(WatchedCursor {
            cursor,
            loss: self.loss.clone(),
        }))
    }

    async fn describe_table(
        &mut self,
        table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        self.loss.watch(self.connection.describe_table(table).await)
    }

    async fn describe_constraints(
        &mut self,
        table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        self.loss
            .watch(self.connection.describe_constraints(table).await)
    }

    async fn describe_role(&mut self, path: &CatalogPath) -> Result<RoleDescription, DriverError> {
        self.loss.watch(self.connection.describe_role(path).await)
    }

    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError> {
        self.loss.watch(self.connection.validate(sql).await)
    }

    async fn full_scan(&mut self, statement: &str) -> Result<Option<FullScan>, DriverError> {
        self.loss.watch(self.connection.full_scan(statement).await)
    }

    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        self.loss.watch(self.connection.transaction_status().await)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.connection.cancel_handle()
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        self.connection.close().await
    }
}

struct WatchedCursor {
    cursor: Box<dyn ResultCursor>,
    loss: Loss,
}

#[async_trait]
impl ResultCursor for WatchedCursor {
    fn columns(&self) -> &[ColumnInfo] {
        self.cursor.columns()
    }

    fn shape(&self) -> ResultShape {
        self.cursor.shape()
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        self.loss.watch(self.cursor.next_page(max_rows).await)
    }

    fn is_exhausted(&self) -> bool {
        self.cursor.is_exhausted()
    }

    async fn finish(self: Box<Self>) -> Result<Option<u64>, DriverError> {
        let Self { cursor, loss } = *self;
        loss.watch(cursor.finish().await)
    }
}

/// Answers a request the lost connection can't serve, so the UI isn't left waiting.
pub(super) fn refuse(context: &SessionContext, request: SessionRequest, reason: &str) {
    let message = format!("the connection was lost ({reason}); reconnect to go on");
    let console = match context.owner {
        SessionOwner::Console(console) => Some(console),
        SessionOwner::Explorer(_) => None,
    };
    let id = context.data_source.id;
    let event = match (request, console) {
        (SessionRequest::Execute(script), _) => {
            context.send(Event::StatementFinished {
                run: script.run,
                statement: 0,
                result: Err(StatementFailure {
                    message,
                    position: None,
                }),
            });
            Event::RunFinished {
                run: script.run,
                outcome: RunOutcome::Failed,
                elapsed: std::time::Duration::ZERO,
            }
        }
        (SessionRequest::EndTransaction(_), Some(console)) => Event::TransactionEnded {
            console,
            result: Err(message),
        },
        (SessionRequest::ApplyEdits(_), Some(console)) => Event::EditsApplied {
            console,
            result: Err(EditFailure {
                statement: None,
                message,
            }),
        },
        (SessionRequest::ExportAll(ExportAll { path, .. }), _)
        | (SessionRequest::ExportTable(TableExport { path, .. }), _) => Event::ExportFinished {
            exporter: context.owner.exporter(),
            path,
            result: Err(message),
        },
        (SessionRequest::Validate(statements), Some(console)) => Event::StatementsValidated {
            console,
            results: statements
                .into_iter()
                .map(|sql| (sql, StatementCheck::Skipped))
                .collect(),
        },
        (SessionRequest::Children(path, query), _) => Event::ChildrenLoaded {
            id,
            path,
            query,
            result: Err(message),
        },
        (SessionRequest::Definition(path), _) => Event::DefinitionLoaded {
            id,
            path,
            result: Err(message),
        },
        (SessionRequest::DescribeTable(table), _) => Event::TableDescribed {
            data_source: id,
            table,
            result: Err(message),
        },
        (SessionRequest::DescribeTables(tables), _) => Event::TablesDescribed {
            data_source: id,
            tables: tables
                .into_iter()
                .map(|table| DescribedTable {
                    table,
                    result: Err(message.clone()),
                })
                .collect(),
        },
        (SessionRequest::DescribeRole(path), _) => Event::RoleDescribed {
            data_source: id,
            path,
            result: Err(message),
        },
        (SessionRequest::ApplyDdl(_), _) => Event::DdlApplied {
            data_source: id,
            result: Err(DdlFailure {
                statement: None,
                message,
                applied: 0,
            }),
        },
        (SessionRequest::ImportRows(_), _) => Event::RowsImported {
            data_source: id,
            result: Err(ImportFailure {
                lines: None,
                message,
            }),
        },
        (SessionRequest::Ddl(path), _) => Event::DdlLoaded {
            data_source: id,
            path,
            result: Err(message),
        },
        (SessionRequest::LoadTable(path), _) => Event::TableLoaded {
            data_source: id,
            path,
            result: Err(message),
        },
        // Rows to fetch were the lost connection's; the explorer never edits or ends a
        // transaction.
        (
            SessionRequest::FetchMore(_)
            | SessionRequest::EndTransaction(_)
            | SessionRequest::ApplyEdits(_)
            | SessionRequest::Validate(_),
            _,
        ) => return,
    };
    context.send(event);
}
