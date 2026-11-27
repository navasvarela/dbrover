use std::fmt;

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    CancelHandle, CatalogMatches, CatalogNode, CatalogPath, CatalogSearch, Cell, ChildrenPage,
    ChildrenQuery, CostEstimate, DriverError, HandedOver, Handoff, ManagedService,
    PrincipalDescription, ProgressHandle, RelationName, ResultCursor, RoleDescription,
    SchemaDescription, SearchableTable, ServerActivity, SessionId, SessionStop, StopOutcome,
    TableConstraints, TableDescription, TableName, TablesToSearch, TransactionStatus,
    UserManagement,
};

/// What a server reported about itself when the connection was opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// e.g. "PostgreSQL" or "SQLite".
    pub product: String,
    pub version: String,
    /// The managed service the server said it is, where its catalog or DDL differs from its
    /// engine's (Redshift, Synapse, Fabric; C1). `None` for the engine itself, and for
    /// services that behave as it does.
    pub service: Option<ManagedService>,
}

impl ServerInfo {
    pub fn new(product: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            product: product.into(),
            version: version.into(),
            service: None,
        }
    }

    pub fn on_service(self, service: Option<ManagedService>) -> Self {
        Self { service, ..self }
    }
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

    /// Objects anywhere in the connected database whose name contains the search's text,
    /// ignoring case, found on the server without loading the levels they are in (C0g), up
    /// to the search's limit. `None`, the default, where the driver can't search: the explorer
    /// then filters the levels it has loaded.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] if the catalog query fails.
    async fn search_catalog(
        &mut self,
        _search: &CatalogSearch,
    ) -> Result<Option<CatalogMatches>, DriverError> {
        Ok(None)
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

    /// Runs `sql` with `values` bound, as [`Connection::execute_bound`] does, for a caller that
    /// reads its rows through [`ResultCursor::batches`] where the engine has them (ADR 0007).
    /// An engine whose answer's format is chosen when the statement starts, as ClickHouse's
    /// is, starts it in its columnar format here; the default runs the statement as
    /// `execute_bound` does, which suits an engine whose cursor answers both ways, or only in
    /// text.
    ///
    /// # Errors
    ///
    /// As [`Connection::execute_bound`].
    async fn execute_for_batches(
        &mut self,
        sql: &str,
        values: &[Cell],
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        self.execute_bound(sql, values).await
    }

    /// Runs `sql` with `values` bound, asking the engine to write its result as files rather
    /// than to answer with rows, for a report whose `[[source]]` has a `handoff`
    /// (`docs/reports-at-scale.md`, RS7f). The caller then reads the files with DuckDB and
    /// deletes them.
    ///
    /// `None`, the default, for an engine with no handoff: [`HandoffSupport::of`] says which
    /// engines have one, and planning refuses a `handoff` on the others, so a `None` here is a
    /// driver that didn't implement what its support says it has.
    ///
    /// # Errors
    ///
    /// As [`Connection::execute_bound`]; the statement that writes the files is the engine's
    /// own, so its refusals are the engine's.
    async fn execute_as_files(
        &mut self,
        sql: &str,
        values: &[Cell],
        handoff: &Handoff,
    ) -> Result<Option<HandedOver>, DriverError> {
        let _ = (sql, values, handoff);
        Ok(None)
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

    /// Describes every table, index and view of `schema` in the configured database, for
    /// comparing schemas and saving snapshots (see `docs/schema-compare.md`). Names inside
    /// definitions are printed without `schema` and other schemas' names with theirs, so the
    /// same objects described in two schemas read the same. `None` if there is no such schema.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] where the driver can't read a whole schema yet (see
    /// `dbrover_sql::Capabilities::schema_compare`); [`DriverError::Connection`] if a catalog
    /// query fails.
    async fn describe_schema(
        &mut self,
        schema: &str,
    ) -> Result<Option<SchemaDescription>, DriverError> {
        let _ = schema;
        Err(DriverError::Unsupported(
            "This database's schemas can't be compared yet.",
        ))
    }

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

    /// What the server offers for managing its users, and what this login may do there (W18):
    /// read when a users folder first loads, before any of the users dialogs opens.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] on engines whose users DbRover doesn't manage this way;
    /// [`DriverError::Connection`] if the server can't be asked.
    async fn user_management(&mut self) -> Result<UserManagement, DriverError> {
        Err(DriverError::Unsupported(
            "This database's users can't be managed here yet.",
        ))
    }

    /// Describes the account, login, user or role at `path`, a node a users, logins or roles
    /// folder lists, on MySQL, MariaDB and SQL Server (W18). PostgreSQL's roles are described
    /// by [`Connection::describe_role`].
    ///
    /// # Errors
    ///
    /// [`DriverError::InvalidPath`] for a path that isn't one, or one that no longer exists;
    /// [`DriverError::Connection`] if the catalog can't be read.
    async fn describe_principal(
        &mut self,
        path: &CatalogPath,
    ) -> Result<PrincipalDescription, DriverError> {
        Err(DriverError::InvalidPath(path.clone()))
    }

    /// What the server is doing: its sessions as this login may see them, who waits on whose
    /// locks, and the statements that would stop each session (W15). Read on a connection of
    /// its own, never one a console runs on.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] on engines that have no other sessions to show (SQLite,
    /// DuckDB) or whose activity DbRover doesn't read yet; [`DriverError::Connection`] if the
    /// server's views can't be read.
    async fn server_activity(&mut self) -> Result<ServerActivity, DriverError> {
        Err(DriverError::Unsupported(
            "This database's server activity can't be shown yet.",
        ))
    }

    /// Sends one of the statements [`Connection::server_activity`] wrote for a session it
    /// listed. Only such a statement may be passed: the driver reads its answer as the stop it
    /// wrote.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] where activity isn't read; [`DriverError::Statement`] when
    /// the server refuses, such as for a login that may not end another user's session.
    async fn stop_session(&mut self, stop: &SessionStop) -> Result<StopOutcome, DriverError> {
        let _ = stop;
        Err(DriverError::Unsupported(
            "This database's sessions can't be stopped yet.",
        ))
    }

    /// The id the server gives this connection's session, as [`Connection::server_activity`]
    /// lists it, so DbRover can tell its own sessions apart. `None` where the engine has no
    /// lasting session (ClickHouse over HTTP) or activity isn't read.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] if the server can't be asked.
    async fn own_session(&mut self) -> Result<Option<SessionId>, DriverError> {
        Ok(None)
    }

    /// The tables of a search's scope, each with its key, its row estimate and its columns as
    /// the catalog types them (W16, `docs/data-search.md`), read in a few catalog queries
    /// rather than one per table, since a schema may have thousands. System schemas are left
    /// out of a search of the whole data source.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] where the driver doesn't list them yet (see
    /// `dbrover_sql::Capabilities::data_search`); [`DriverError::InvalidPath`] for a scope that
    /// names no database, schema, folder or table; [`DriverError::Connection`] if a catalog
    /// query fails.
    async fn searchable_tables(
        &mut self,
        tables: &TablesToSearch,
    ) -> Result<Vec<SearchableTable>, DriverError> {
        let _ = tables;
        Err(DriverError::Unsupported(
            "This database's data can't be searched yet.",
        ))
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

    /// What `statement` would cost to run, on an engine that bills each read: whether it reads
    /// a whole table or index, or how many bytes it scans ([`CostEstimate`]). Asked before the
    /// statement runs when the data source's [`CostGuard`](crate::CostGuard) is on; the
    /// statement isn't run. `None`, the default, for a statement that costs nothing worth
    /// asking about, for one the driver can't read (the database will say why when it runs),
    /// and on every engine that doesn't bill reads.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] if what the answer depends on can't be read, such as a
    /// table's keys.
    async fn estimate_cost(
        &mut self,
        _statement: &str,
    ) -> Result<Option<CostEstimate>, DriverError> {
        Ok(None)
    }

    /// Lets the next statement this connection runs scan past the data source's byte limit,
    /// because the user saw its estimate ([`Connection::estimate_cost`]) and said yes. Engines
    /// that keep the limit themselves (BigQuery's `maximumBytesBilled`, C4) send that one
    /// statement without it; the default does nothing.
    fn allow_next_past_limit(&mut self) {}

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

    /// A handle for asking what the running statement has done so far, from another task
    /// while it runs (C0f): engines that run statements as jobs, and ClickHouse. `None`, the
    /// default, where the engine can't say. Valid for the connection's whole lifetime.
    fn progress_handle(&self) -> Option<Arc<dyn ProgressHandle>> {
        None
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError>;
}
