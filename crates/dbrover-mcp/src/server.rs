//! The MCP tools, on rmcp's router.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use dbrover_core::{
    CATALOG_PAGE_SIZE, CatalogNode, CatalogPath, ChildrenQuery, Connection, DataSource,
    DriverError, Identifier, NodeKind, PageToken, RelationName, describe_error,
};
use dbrover_engine::read_only::{self as guard, Refusal};
use dbrover_export::{ExportColumn, ExportFormat, ExportSpec, RowSource, render};
use dbrover_language::{NamedSchemas, QueryLanguage, StatementKind};
use dbrover_sql::Dialect;
use dbrover_store::{HistoryOutcome, SchemaScope};
use jiff::Timestamp;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::{NotificationContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit::{Audit, Ran};
use crate::catalog::{self, NodeJson, PathStep};
use crate::connections::Connections;
use crate::execute::ExecuteArgs;
use crate::project::ServedProject;
use crate::scope::Scope;
use crate::transaction::{self, Access, CallSettings, Rows, RunError, Watchdog};

/// Work to run on a connection inside a call's transaction. Boxed because it borrows the
/// connection for as long as it runs.
pub(crate) type Work<'c, T> = Pin<Box<dyn Future<Output = Result<T, RunError>> + Send + 'c>>;

/// Catalog work, which [`DbRoverMcp::on_connection`] runs under one watchdog.
type CatalogWork<'c, T> = Pin<Box<dyn Future<Output = Result<T, DriverError>> + Send + 'c>>;

#[derive(Clone)]
pub(crate) struct DbRoverMcp {
    pub(crate) inner: Arc<Inner>,
    tool_router: ToolRouter<Self>,
}

pub(crate) struct Inner {
    pub(crate) project: ServedProject,
    pub(crate) connections: Arc<Connections>,
    pub(crate) audit: Audit,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ListObjectsArgs {
    /// The data source's name, as list_data_sources gives it (or its id).
    data_source: String,
    /// Which node's children to list: a `path` from an earlier list_objects result. Leave
    /// it out for the top level.
    #[serde(default)]
    path: Vec<PathStep>,
    /// Also list system schemas and tables such as pg_catalog or information_schema.
    #[serde(default)]
    include_system: bool,
    /// Only the objects whose name contains this, ignoring case. On a large level, such as a
    /// Redis database's keys, the database does the search.
    #[serde(default)]
    filter: Option<String>,
    /// Where to go on in a level too large to list at once (a schema of thousands of tables,
    /// a Redis database's keys): the `next` of the previous answer, with the same path and
    /// filter. Leave it out for the first page.
    #[serde(default)]
    after: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct DescribeTableArgs {
    /// The data source's name, as list_data_sources gives it (or its id).
    data_source: String,
    /// The table's name.
    table: String,
    /// The table's schema (on MySQL and MariaDB, its database). Leave it out for the
    /// connection's default schema.
    schema: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct DefinitionArgs {
    /// The data source's name, as list_data_sources gives it (or its id).
    data_source: String,
    /// The object's `path` from list_objects; only nodes with has_definition have one.
    path: Vec<PathStep>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct StatementArgs {
    /// The data source's name, as list_data_sources gives it (or its id).
    data_source: String,
    /// One statement that only reads, in the data source's language: a SELECT in SQL, a
    /// Redis command such as GET or HGETALL, a MongoDB shell command such as
    /// db.orders.find({status: "open"}), or an Elasticsearch or OpenSearch request: a method
    /// and path, then a JSON body on the next lines, such as
    /// GET /orders/_search followed by {"query": {"match": {"status": "open"}}}.
    sql: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct QueryArgs {
    /// The data source's name, as list_data_sources gives it (or its id).
    data_source: String,
    /// One statement that only reads, in the data source's language: a SELECT in SQL, a
    /// Redis command such as GET or HGETALL, a MongoDB shell command such as
    /// db.orders.find({status: "open"}), or an Elasticsearch or OpenSearch request: a method
    /// and path, then a JSON body on the next lines, such as
    /// GET /orders/_search followed by {"query": {"match": {"status": "open"}}}.
    sql: String,
    /// How the rows come back: json (the default), as columns and rows of text; or csv,
    /// markdown or jsonl, as that text in `text`, a header line first for csv and markdown.
    #[serde(default)]
    format: QueryFormat,
}

/// The formats the query tool answers in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
enum QueryFormat {
    /// Columns with their types, and rows as arrays of text or null.
    #[default]
    Json,
    /// RFC 4180: comma-separated, a header line, quoted where needed, NULL as an empty field.
    Csv,
    /// A GitHub-flavoured table, NULL written out.
    Markdown,
    /// One object a line, keyed by column name.
    Jsonl,
}

impl QueryFormat {
    /// The export format that writes the rows as text; `None` for this tool's own JSON.
    fn spec(self) -> Option<ExportSpec> {
        let format = match self {
            Self::Json => return None,
            Self::Csv => ExportFormat::Csv,
            Self::Markdown => ExportFormat::Markdown,
            Self::Jsonl => ExportFormat::JsonLines,
        };
        // None of these formats needs to know where the rows came from.
        format
            .default_spec(&RowSource::untabled(Dialect::Sqlite))
            .ok()
    }
}

/// A failed call, as the client sees it: enough to fix the call and try again.
#[derive(Debug)]
pub(crate) struct ToolError {
    kind: &'static str,
    pub(crate) message: String,
    sqlstate: Option<String>,
    /// Byte offset into the SQL the client sent.
    pub(crate) position: Option<usize>,
}

impl ToolError {
    pub(crate) fn refused(message: impl Into<String>) -> Self {
        Self::plain("refused", message)
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::plain("invalid_arguments", message)
    }

    fn plain(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            sqlstate: None,
            position: None,
        }
    }

    fn from_run(error: RunError, prefix_len: usize) -> Self {
        match error {
            RunError::Driver(DriverError::Statement {
                source,
                position,
                sqlstate,
            }) => Self {
                kind: "database",
                message: describe_error(source.as_ref()),
                sqlstate,
                position: position.and_then(|position| position.checked_sub(prefix_len)),
            },
            RunError::Driver(
                error @ (DriverError::Connect(_)
                | DriverError::Connection(_)
                | DriverError::Lost(_)),
            ) => Self::plain("connection", describe_error(&error)),
            RunError::Driver(error @ DriverError::InvalidPath(_)) => {
                Self::invalid(describe_error(&error))
            }
            error @ RunError::TimedOut(_) => Self::plain("timeout", describe_error(&error)),
            error @ RunError::NoTransaction(_) => Self::refused(error.to_string()),
        }
    }

    fn into_result(self) -> CallToolResult {
        CallToolResult::structured_error(json!({
            "error": {
                "kind": self.kind,
                "message": self.message,
                "sqlstate": self.sqlstate,
                "position": self.position,
            }
        }))
    }
}

type ToolResult = Result<CallToolResult, ErrorData>;

/// Turns a tool body's outcome into what rmcp sends: domain failures are tool results with
/// `isError`, so the model reads them, rather than protocol errors.
fn reply(outcome: Result<Value, ToolError>) -> ToolResult {
    Ok(match outcome {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => error.into_result(),
    })
}

#[tool_router]
impl DbRoverMcp {
    pub(crate) fn new(project: ServedProject, connections: Arc<Connections>, audit: Audit) -> Self {
        let mut tool_router = Self::tool_router();
        // A project that only reads has nothing for execute to run, so clients don't see it.
        if !project.changes.any() {
            tool_router.remove_route("execute");
        }
        Self {
            inner: Arc::new(Inner {
                project,
                connections,
                audit,
            }),
            tool_router,
        }
    }

    /// Lists the project's data sources: their names (which the other tools take), engines
    /// and server versions once connected, and the limits every call runs under.
    #[tool(annotations(read_only_hint = true, open_world_hint = false))]
    async fn list_data_sources(&self) -> ToolResult {
        self.inner.audit.call("list_data_sources", None, None);
        let project = &self.inner.project;
        let data_sources: Vec<Value> = project
            .data_sources
            .iter()
            .map(|data_source| {
                json!({
                    "name": data_source.name,
                    "id": data_source.id.to_string(),
                    "engine": data_source.config.driver().display_name(),
                    "color": data_source.color.map(|color| color.display_name()),
                    "server": self
                        .inner
                        .connections
                        .server_info(data_source.id)
                        .map(|server| server.to_string()),
                })
            })
            .collect();
        reply(Ok(json!({
            "project": project.project.name,
            "data_sources": data_sources,
            "limits": {
                "max_rows": project.limits.max_rows,
                "statement_timeout_ms": project.limits.statement_timeout.as_millis(),
                "read_only": !project.changes.any(),
                "allows": project.changes.names(),
                "schemas": match &project.schemas {
                    SchemaScope::All => json!("all"),
                    SchemaScope::Only(schemas) => json!(schemas),
                },
            },
        })))
    }

    /// Lists one level of a data source's catalog, as DbRover's explorer shows it: databases,
    /// schemas, folders of tables and views, then tables, then their columns, indexes and
    /// constraints; on Redis, databases, then keys grouped by their prefixes; on MongoDB,
    /// databases, then collections, then their indexes; on Elasticsearch and OpenSearch,
    /// folders of indices, aliases, data streams and index templates, then an index's fields.
    /// Pass a node's
    /// `path` to list its children. A large level comes 1,000 objects at a time: when the
    /// answer has `next`, pass it as `after` for the rest.
    #[tool(annotations(read_only_hint = true, open_world_hint = false))]
    async fn list_objects(&self, Parameters(args): Parameters<ListObjectsArgs>) -> ToolResult {
        reply(self.list_objects_json(args).await)
    }

    /// Describes a table: its columns with their types, its primary key, its indexes and its
    /// constraints.
    #[tool(annotations(read_only_hint = true, open_world_hint = false))]
    async fn describe_table(&self, Parameters(args): Parameters<DescribeTableArgs>) -> ToolResult {
        reply(self.describe_table_json(args).await)
    }

    /// The source of a view, function, trigger, index or other object that has one
    /// (`has_definition` in list_objects).
    #[tool(annotations(read_only_hint = true, open_world_hint = false))]
    async fn get_definition(&self, Parameters(args): Parameters<DefinitionArgs>) -> ToolResult {
        reply(self.definition_json(args).await)
    }

    /// Runs one statement that only reads, such as a SELECT, in a read-only transaction that
    /// is then rolled back; or one Redis command that only reads, such as HGETALL; or one MongoDB shell command that only reads, such as db.orders.find({status: "open"}); or one Elasticsearch or OpenSearch request that only reads, such as a search, whose hits come as documents, or a _sql query, whose answer comes as rows. Returns at most the project's row limit, with `truncated` set
    /// when there were more rows; cells are text, or null. With `format` csv, markdown or
    /// jsonl, the rows come back as that text instead, ready to save or paste.
    #[tool(annotations(read_only_hint = true, open_world_hint = false))]
    async fn query(&self, Parameters(args): Parameters<QueryArgs>) -> ToolResult {
        let QueryArgs {
            data_source,
            sql,
            format,
        } = args;
        let statement = StatementArgs { data_source, sql };
        reply(
            self.run_statement("query", statement, false)
                .await
                .map(|ran| ran.json(format)),
        )
    }

    /// Shows how the database would run one read statement, without running it.
    #[tool(annotations(read_only_hint = true, open_world_hint = false))]
    async fn explain(&self, Parameters(args): Parameters<StatementArgs>) -> ToolResult {
        reply(
            self.run_statement("explain", args, true)
                .await
                .map(|ran| ran.json(QueryFormat::Json)),
        )
    }

    /// Runs statements that change data or the schema, in order, in one transaction that is
    /// committed only if they all succeed (on MySQL and MariaDB, DDL commits on its own).
    /// Each statement must be one the project's permissions allow: write for INSERT, UPDATE,
    /// DELETE, MERGE and TRUNCATE; schema for CREATE, ALTER, DROP and other DDL; admin for
    /// grants, settings and anything the parser can't read. Returns rows affected per
    /// statement, and the rows of any that return some (up to the row limit).
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = true,
        idempotent_hint = false,
        open_world_hint = false
    ))]
    async fn execute(&self, Parameters(args): Parameters<ExecuteArgs>) -> ToolResult {
        reply(self.execute_json(args).await)
    }
}

impl DbRoverMcp {
    /// The project's allow-list as `data_source` applies it; `None` when all are allowed.
    pub(crate) fn scope(&self, data_source: &DataSource) -> Option<Scope> {
        Scope::new(&self.inner.project.schemas, &data_source.config)
    }

    /// Refuses a call before it reaches the database, and logs why.
    pub(crate) fn refuse(&self, tool: &str, data_source: &DataSource, reason: String) -> ToolError {
        self.inner.audit.refused(tool, &data_source.name, &reason);
        ToolError::refused(reason)
    }

    fn check_path(
        &self,
        tool: &str,
        data_source: &DataSource,
        path: &CatalogPath,
    ) -> Result<(), ToolError> {
        match self.scope(data_source).map(|scope| scope.check_path(path)) {
            Some(Err(reason)) => Err(self.refuse(tool, data_source, reason)),
            _ => Ok(()),
        }
    }

    pub(crate) fn data_source(&self, tool: &str, name: &str) -> Result<&DataSource, ToolError> {
        self.inner.project.data_source(name).ok_or_else(|| {
            let known: Vec<&str> = self
                .inner
                .project
                .data_sources
                .iter()
                .map(|data_source| data_source.name.as_str())
                .collect();
            let message = format!(
                "the project has no data source {name:?}; it has: {}",
                known.join(", ")
            );
            self.inner.audit.refused(tool, name, &message);
            ToolError::invalid(message)
        })
    }

    async fn list_objects_json(&self, args: ListObjectsArgs) -> Result<Value, ToolError> {
        let tool = "list_objects";
        let data_source = self.data_source(tool, &args.data_source)?;
        self.inner.audit.call(tool, Some(&data_source.name), None);
        let path =
            catalog::path(&args.path).map_err(|error| ToolError::invalid(error.to_string()))?;
        self.check_path(tool, data_source, &path)?;
        let query = ChildrenQuery {
            filter: args.filter.filter(|filter| !filter.is_empty()),
            after: args.after.map(PageToken::new),
            limit: Some(CATALOG_PAGE_SIZE),
        };
        let page = self
            .on_connection(tool, data_source, move |connection| {
                Box::pin(async move { connection.children_page(&path, &query).await })
            })
            .await?;
        let nodes = page.nodes;
        let scope = self.scope(data_source);
        let objects: Vec<NodeJson> = nodes
            .iter()
            .filter(|node| args.include_system || !node.system)
            .filter(|node| match (&scope, node.kind()) {
                (Some(scope), Some(NodeKind::Schema)) => scope.allows(&node.label),
                _ => true,
            })
            .map(NodeJson::from)
            .collect();
        Ok(match page.next {
            Some(next) => json!({ "objects": objects, "next": next.as_str() }),
            None => json!({ "objects": objects }),
        })
    }

    async fn describe_table_json(&self, args: DescribeTableArgs) -> Result<Value, ToolError> {
        let tool = "describe_table";
        let data_source = self.data_source(tool, &args.data_source)?;
        self.inner.audit.call(tool, Some(&data_source.name), None);
        let scope = self.scope(data_source);
        if let (Some(scope), Some(schema)) = (&scope, &args.schema)
            && !scope.allows(schema)
        {
            let reason = format!("this project doesn't allow the schema {schema:?}");
            return Err(self.refuse(tool, data_source, reason));
        }
        let database = data_source.config.database().map(str::to_owned);
        let described = self
            .on_connection(tool, data_source, move |connection| {
                Box::pin(async move {
                    describe(
                        connection,
                        database.as_deref(),
                        args.schema.as_deref(),
                        &args.table,
                    )
                    .await
                })
            })
            .await?;
        // Without a schema, the table is looked up where the connection resolves names,
        // which can be outside the list on engines that don't pin it.
        let described = described.filter(|described| match (&scope, &described["schema"]) {
            (Some(scope), Value::String(schema)) => scope.allows(schema),
            _ => true,
        });
        described.ok_or_else(|| {
            ToolError::invalid(
                "there is no such table (views have no description; try get_definition)",
            )
        })
    }

    async fn definition_json(&self, args: DefinitionArgs) -> Result<Value, ToolError> {
        let tool = "get_definition";
        let data_source = self.data_source(tool, &args.data_source)?;
        self.inner.audit.call(tool, Some(&data_source.name), None);
        let path =
            catalog::path(&args.path).map_err(|error| ToolError::invalid(error.to_string()))?;
        self.check_path(tool, data_source, &path)?;
        let definition = self
            .on_connection(tool, data_source, move |connection| {
                Box::pin(async move { connection.definition(&path).await })
            })
            .await?;
        Ok(json!({ "definition": definition }))
    }

    /// `query` and `explain`: classify, refuse anything but reading, run, record.
    async fn run_statement(
        &self,
        tool: &'static str,
        args: StatementArgs,
        explain: bool,
    ) -> Result<QueryResult, ToolError> {
        let data_source = self.data_source(tool, &args.data_source)?;
        let language = QueryLanguage::from(data_source.config.driver());
        let sql = args.sql.trim().trim_end_matches(';').trim_end();
        // The same check as the consoles of read-only data sources make.
        let refusal = guard::check(sql, language).err();
        let kind = match &refusal {
            None => Some(StatementKind::Read),
            Some(Refusal::Changes(kind)) => Some(*kind),
            Some(Refusal::Unknown(_)) => None,
        };
        if kind.is_some() {
            self.inner.audit.call(tool, Some(&data_source.name), kind);
        }
        if let Some(refusal) = refusal {
            let reason = match refusal {
                Refusal::Changes(_) if self.inner.project.changes.any() => format!(
                    "{}, and {tool} only runs statements that read; use execute",
                    refusal.reason()
                ),
                Refusal::Changes(_) => {
                    format!("{}, and this project only allows reading", refusal.reason())
                }
                Refusal::Unknown(_) => refusal.reason(),
            };
            return Err(self.refuse(tool, data_source, reason));
        }
        // The statement parsed above, so only a parser bug could fail here; it is refused
        // like any statement whose names can't be read.
        if let Some(scope) = self.scope(data_source) {
            let schemas = language
                .analyze(sql)
                .map_or(NamedSchemas::Unknown, |analysis| analysis.schemas);
            if let Err(reason) = scope.check(&schemas) {
                return Err(self.refuse(tool, data_source, reason));
            }
        }

        let dialect = language.dialect();
        let prefix = match (explain, dialect) {
            (false, _) => "",
            (true, Some(dialect)) => {
                let server = self.inner.connections.server_info(data_source.id);
                dialect.explain_prefix(server.as_ref())
            }
            (true, None) => {
                let reason = format!(
                    "{tool} explains SQL statements, and this data source speaks {}",
                    language.display_name()
                );
                return Err(self.refuse(tool, data_source, reason));
            }
        };
        let statement = format!("{prefix}{sql}");
        let max_rows = self.inner.project.limits.max_rows;
        let started = Timestamp::now();
        let clock = Instant::now();
        let to_run = statement.clone();
        let outcome = self
            .run_call(data_source, Access::Read, move |connection, watchdog| {
                Box::pin(async move {
                    watchdog
                        .watch(transaction::fetch(connection, &to_run, max_rows))
                        .await
                })
            })
            .await;
        let elapsed = clock.elapsed();
        self.inner
            .audit
            .record(Ran {
                data_source,
                sql: &statement,
                started,
                elapsed,
                rows: outcome.as_ref().ok().map(|rows| rows.rows.len() as u64),
                outcome: match &outcome {
                    Ok(_) => HistoryOutcome::Ok,
                    Err(RunError::TimedOut(_)) => HistoryOutcome::Cancelled,
                    Err(_) => HistoryOutcome::Error,
                },
            })
            .await;
        let rows = outcome.map_err(|error| self.failure(tool, data_source, error, prefix.len()))?;
        Ok(QueryResult {
            rows,
            elapsed,
            dialect,
        })
    }

    /// Runs catalog work in a read-only call, under the time limit as a whole.
    async fn on_connection<T: Send>(
        &self,
        tool: &str,
        data_source: &DataSource,
        work: impl for<'c> FnOnce(&'c mut dyn Connection) -> CatalogWork<'c, T> + Send + 'static,
    ) -> Result<T, ToolError> {
        self.run_call(data_source, Access::Read, move |connection, watchdog| {
            Box::pin(async move { watchdog.watch(work(connection)).await })
        })
        .await
        .map_err(|error| self.failure(tool, data_source, error, 0))
    }

    pub(crate) fn failure(
        &self,
        tool: &str,
        data_source: &DataSource,
        error: RunError,
        prefix_len: usize,
    ) -> ToolError {
        let error = ToolError::from_run(error, prefix_len);
        self.inner.audit.failed(
            tool,
            &data_source.name,
            &error.message,
            error.sqlstate.as_deref(),
        );
        error
    }

    /// Runs `work` on the data source's connection inside a transaction for `access`,
    /// committed or rolled back as [`transaction::end`] decides. A connection the server
    /// closed while it sat idle fails on `BEGIN`; that is retried once on a new connection,
    /// before `work` has run.
    pub(crate) async fn run_call<T: Send>(
        &self,
        data_source: &DataSource,
        access: Access,
        work: impl for<'c> FnOnce(&'c mut dyn Connection, Watchdog) -> Work<'c, T>,
    ) -> Result<T, RunError> {
        let writable = self.inner.project.changes.any();
        let driver = data_source.config.driver();
        // `ServedProject` leaves out data sources that can't be read safely, so only changes
        // on an engine without a read-write mode, which `execute` refuses first, end here.
        let Some(mode) = transaction::mode(driver, access, writable) else {
            return Err(RunError::NoTransaction(driver.display_name()));
        };
        let timeout = self.inner.project.limits.statement_timeout;
        let scope = self.scope(data_source);
        let settings = CallSettings {
            timeout,
            search_path: scope.as_ref().and_then(Scope::search_path),
        };
        let mut retried = false;
        let mut connection = loop {
            let mut connection = self
                .inner
                .connections
                .checkout(data_source.id)
                .await
                .map_err(|error| RunError::Driver(DriverError::Connect(Box::new(error))))?;
            match transaction::begin(&mut *connection, mode, &settings).await {
                Ok(()) => break connection,
                // A connection idle long enough for the server to drop it fails its first
                // statement; a new one is the answer.
                Err(DriverError::Connection(_) | DriverError::Lost(_)) if !retried => {
                    tracing::info!(data_source = %data_source.name, "reconnecting");
                    connection.discard();
                    retried = true;
                }
                Err(error) => {
                    let outcome = transaction::end(&mut *connection, mode, Err(error.into())).await;
                    if outcome.as_ref().is_err_and(RunError::breaks_connection) {
                        connection.discard();
                    }
                    return outcome;
                }
            }
        };
        let watchdog = Watchdog::new(connection.cancel_handle(), timeout);
        let outcome = work(&mut *connection, watchdog).await;
        let outcome = transaction::end(&mut *connection, mode, outcome).await;
        if outcome.as_ref().is_err_and(RunError::breaks_connection) {
            connection.discard();
        }
        outcome
    }
}

/// A statement's rows, and what answering with them needs.
struct QueryResult {
    rows: Rows,
    elapsed: std::time::Duration,
    /// The data source's SQL dialect, which types the columns; `None` for another language.
    dialect: Option<Dialect>,
}

impl QueryResult {
    /// The rows as `format` asks: this tool's JSON, or the text of an export format with
    /// the same counts.
    fn json(&self, format: QueryFormat) -> Value {
        let Some(spec) = format.spec() else {
            return rows_json(&self.rows, self.elapsed);
        };
        let columns: Vec<ExportColumn> = self
            .rows
            .columns
            .iter()
            .map(|column| ExportColumn::typed_by(column, self.dialect))
            .collect();
        json!({
            "format": spec.format().id(),
            "text": render(&spec, &columns, &self.rows.rows).unwrap_or_default(),
            "row_count": self.rows.rows.len(),
            "truncated": self.rows.truncated,
            "elapsed_ms": self.elapsed.as_millis(),
        })
    }
}

fn rows_json(rows: &Rows, elapsed: std::time::Duration) -> Value {
    let columns: Vec<Value> = rows
        .columns
        .iter()
        .map(|column| json!({ "name": column.name, "type": column.type_name }))
        .collect();
    json!({
        "columns": columns,
        "rows": rows.rows,
        "row_count": rows.rows.len(),
        "truncated": rows.truncated,
        "elapsed_ms": elapsed.as_millis(),
    })
}

/// A table's description: the driver's for its name and key, and the explorer's catalog
/// levels for column types, indexes and constraints. A name given as typed is tried first,
/// then as an unquoted name the database folds to its own case.
async fn describe(
    connection: &mut dyn Connection,
    database: Option<&str>,
    schema: Option<&str>,
    table: &str,
) -> Result<Option<Value>, DriverError> {
    let identifier = |name: &str, quoted| Identifier {
        name: name.to_owned(),
        quoted,
    };
    let mut description = None;
    for quoted in [true, false] {
        let name = RelationName {
            schema: schema.map(|schema| identifier(schema, quoted)),
            name: identifier(table, quoted),
        };
        description = connection.describe_table(&name).await?;
        if description.is_some() {
            break;
        }
    }
    let Some(description) = description else {
        return Ok(None);
    };
    let mut table_path = CatalogPath::root();
    if let Some(database) = database {
        table_path = table_path.child(NodeKind::Database, database);
    }
    let table_path = table_path
        .child(NodeKind::Schema, &description.schema)
        .child(
            NodeKind::Folder(dbrover_core::FolderKind::Tables),
            dbrover_core::FolderKind::Tables.label(),
        )
        .child(NodeKind::Table, &description.name);
    let mut parts = serde_json::Map::new();
    for folder in connection.children(&table_path).await? {
        let Some(NodeKind::Folder(kind)) = folder.kind() else {
            continue;
        };
        let children = connection.children(&folder.path).await?;
        parts.insert(
            kind.label().to_owned(),
            Value::from(named_details(&children)),
        );
    }
    let mut value = json!({
        "schema": description.schema,
        "name": description.name,
        "primary_key": description.primary_key,
    });
    if let Value::Object(object) = &mut value {
        object.extend(parts);
    }
    Ok(Some(value))
}

fn named_details(nodes: &[CatalogNode]) -> Vec<Value> {
    nodes
        .iter()
        .map(|node| json!({ "name": node.label, "detail": node.detail }))
        .collect()
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DbRoverMcp {
    fn get_info(&self) -> ServerConfig {
        let project = &self.inner.project;
        let mut instructions = format!(
            "Databases of the DbRover project \"{}\". Start with list_data_sources; the other \
             tools take a data source's name. query and explain run one statement that only \
             reads, in a read-only transaction. Every call returns at most {} rows, and a \
             statement is cancelled after {} s.",
            project.project.name,
            project.limits.max_rows,
            project.limits.statement_timeout.as_secs()
        );
        let allowed = project.changes.names();
        if allowed.is_empty() {
            instructions.push_str(" Statements that change anything are refused.");
        } else {
            instructions.push_str(&format!(
                " execute runs statements that change something, in one transaction; this \
                 project allows {} beyond reading, and refuses the rest.",
                allowed.join(", ")
            ));
        }
        if let SchemaScope::Only(schemas) = &project.schemas {
            instructions.push_str(&format!(
                " Only these schemas can be listed or named: {}.",
                schemas.join(", ")
            ));
        }
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("dbrover", env!("CARGO_PKG_VERSION")))
            .with_instructions(instructions)
    }

    /// Learns the client's name for the history, once the handshake is done.
    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        if let Some(info) = context.peer.peer_info() {
            self.inner.audit.set_client(&info.client_info.name);
        }
    }
}
