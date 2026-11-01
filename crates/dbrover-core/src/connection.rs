use std::fmt;

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    CancelHandle, CatalogNode, CatalogPath, Cell, ChildrenPage, ChildrenQuery, DriverError,
    RelationName, ResultCursor, RoleDescription, TableConstraints, TableDescription, TableName,
    TransactionStatus,
};

/// What a server reported about itself when the connection was opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// e.g. "PostgreSQL" or "SQLite".
    pub product: String,
    pub version: String,
}

impl fmt::Display for ServerInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.product, self.version)
    }
}

/// Why the database would refuse a statement, found by preparing it without running it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The database's message.
    pub message: String,
    /// Byte offset into the statement where the problem was found, when the database says.
    pub position: Option<usize>,
    pub kind: RejectionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionKind {
    /// The statement can't be read.
    Syntax,
    /// It names a table, column, function or schema that doesn't exist (now).
    UnknownName,
    /// Anything else: a type mismatch, an ambiguous column, …
    Other,
}

/// A statement that would read every item of a table or index, which an engine that bills each
/// read charges for whatever its filter keeps: what a data source's
/// [`CostGuard`](crate::CostGuard) asks about first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullScan {
    pub table: String,
    /// The secondary index read instead of the table, when the statement names one.
    pub index: Option<String>,
    /// The key whose value, given at the top of the statement's condition, would have read
    /// only its partitions.
    pub partition_key: String,
}

impl FullScan {
    /// Why the statement reads everything, for the question before it runs and the refusals.
    pub fn describe(&self) -> String {
        let read = match &self.index {
            Some(index) => format!("every item of {}'s index {index}", self.table),
            None => format!("every item of {}", self.table),
        };
        format!(
            "This reads {read}, billed for each item read whatever the WHERE keeps: it has no = \
             or IN on the partition key {} at the top of its WHERE.",
            self.partition_key
        )
    }
}

/// A live connection to a database, implemented once per driver.
///
/// Used as `Box<dyn Connection>`, hence `async-trait`: native `async fn` in traits is not
/// dyn-compatible yet. See `docs/adr/0002-native-drivers-behind-a-trait.md`.
#[async_trait]
pub trait Connection: Send {
    fn server_info(&self) -> &ServerInfo;

    /// Round-trips a trivial statement to prove the connection can run queries, not just
    /// that the handshake succeeded.
    async fn ping(&mut self) -> Result<(), DriverError>;

    /// The children of the catalog node at `path`, in display order. The root path lists the
    /// top level (databases for PostgreSQL, schemas for SQLite).
    ///
    /// # Errors
    ///
    /// [`DriverError::InvalidPath`] for a path this driver never returns, or
    /// [`DriverError::Connection`] if the catalog query fails.
    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError>;

    /// Part of the children of the node at `path`, as `query` asks: those whose label contains
    /// its filter, after its token, up to its limit. A driver pages the levels that can grow
    /// too large to load whole, and applies the filter in the server; by default the whole
    /// level is loaded with [`Connection::children`] and filtered here.
    ///
    /// # Errors
    ///
    /// As for [`Connection::children`].
    async fn children_page(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        Ok(ChildrenPage::whole(self.children(path).await?, query))
    }

    /// The source of the object at `path` (a `CREATE VIEW`, function body, index definition,
    /// …), for nodes whose [`CatalogNode::has_definition`] is set.
    ///
    /// # Errors
    ///
    /// As for [`Connection::children`]. A node without a definition is an `InvalidPath`.
    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError>;

    /// Starts running one statement and returns a cursor over its result. Rows are read
    /// lazily, so a huge result costs nothing until it is paged through.
    ///
    /// # Errors
    ///
    /// [`DriverError::Statement`] if the statement is rejected or fails before producing a
    /// result, with the error's position when known.
    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError>;

    /// [`Connection::execute`], with `values` bound to the statement's placeholders in order,
    /// each as text or `NULL`, which the database converts as it would a string. The
    /// placeholders are written as `dbrover_sql::Dialect::parameter_binding` says for the
    /// driver's dialect (`?1`, `$1`, `@P1` or `?`).
    ///
    /// Drivers whose dialect takes values as literals instead don't implement it: the
    /// default runs a statement without values, and refuses values.
    ///
    /// # Errors
    ///
    /// As [`Connection::execute`]; also [`DriverError::Statement`] if the values don't fit
    /// the placeholders, or the driver can't bind any.
    async fn execute_bound(
        &mut self,
        sql: &str,
        values: &[Cell],
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        if !values.is_empty() {
            return Err(DriverError::Statement {
                source: "this database's driver doesn't bind parameters".into(),
                position: None,
                sqlstate: None,
            });
        }
        self.execute(sql).await
    }

    /// Resolves a table the way a statement on this connection would (search path, case
    /// folding) and describes it, for editing its rows. `None` if no such table exists, or it
    /// isn't a plain table (a view, say).
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] if the catalog query fails.
    async fn describe_table(
        &mut self,
        table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError>;

    /// The constraints of `table`, a table [`Connection::describe_table`] found, by the
    /// names the catalog gives them, for changing the table.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] if the catalog query fails.
    async fn describe_constraints(
        &mut self,
        table: &TableName,
    ) -> Result<TableConstraints, DriverError>;

    /// Describes the role at `path`, a node the roles folder lists, on engines whose users
    /// are roles (see `dbrover_sql::Capabilities::users`).
    ///
    /// # Errors
    ///
    /// [`DriverError::InvalidPath`] where there are no roles, for a path that isn't a role,
    /// or for a role that no longer exists; [`DriverError::Connection`] if the catalog query
    /// fails.
    async fn describe_role(&mut self, path: &CatalogPath) -> Result<RoleDescription, DriverError> {
        Err(DriverError::InvalidPath(path.clone()))
    }

    /// Asks the database whether it would accept `sql`, one data query or change (`SELECT`,
    /// `WITH`, `VALUES`, `INSERT`, `UPDATE`, `DELETE`), without running it: PostgreSQL parses
    /// and analyses it as an unnamed prepared statement, SQLite compiles it. Names resolve as
    /// they would for a run on this connection (search path, temporary tables). `None` if it
    /// would be accepted.
    ///
    /// Callers must not ask while a result cursor is open, nor inside an open transaction on
    /// PostgreSQL, where a rejected statement aborts the transaction. Other statements may run
    /// something while being prepared (some SQLite pragmas do), so they must not be passed.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] if the database can't be asked.
    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError>;

    /// Whether `statement` would read a whole table or index, on an engine that bills each read
    /// ([`FullScan`]). Asked before the statement runs when the data source's cost guard is
    /// on; the statement isn't run. `None`, the default, for a statement that reads by key, for
    /// one the driver can't read (the database will say why when it runs), and on every engine
    /// that doesn't bill reads.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] if what the answer depends on can't be read, such as a
    /// table's keys.
    async fn full_scan(&mut self, _statement: &str) -> Result<Option<FullScan>, DriverError> {
        Ok(None)
    }

    /// Whether a transaction is open on this connection, as the database sees it, so a
    /// transaction the user started with their own `BEGIN` counts too.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] if the database can't be asked.
    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError>;

    /// A handle for cancelling this connection's running statement from another task. Valid
    /// for the connection's whole lifetime.
    fn cancel_handle(&self) -> Arc<dyn CancelHandle>;

    async fn close(self: Box<Self>) -> Result<(), DriverError>;
}
