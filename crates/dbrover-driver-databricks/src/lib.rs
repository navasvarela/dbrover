//! Databricks SQL driver for DbRover, over the Statement Execution API (`docs/cloud-data.md`,
//! C6).
//!
//! A statement is submitted to the data source's SQL warehouse with `wait_timeout` `0s`, polled
//! by its id until it is done, and its result read a chunk at a time through the external
//! links Databricks signs for it. Those four calls are C0's
//! [`JobService`](dbrover_core::JobService) ([`statements`]), so the cursor, cancel and progress
//! handles are [`Jobs`]'s. The API keeps no session, so a connection is an API client and the
//! catalog and schema ([`context`]) it sends with each statement, signed in with C6c's token,
//! OAuth or CLI.

mod api;
mod catalog;
mod context;
mod errors;
mod results;
mod statements;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use async_trait::async_trait;
use dbrover_cloud::databricks::{
    self as sign_in, CliSignIn, EntraDatabricksSignIn, ServicePrincipalSignIn,
};
use dbrover_cloud::entra::{EntraSignIn, EntraTool};
use dbrover_core::{
    CancelHandle, CatalogMatches, CatalogNode, CatalogPath, CatalogSearch, ChildrenPage,
    ChildrenQuery, ColumnInfo, Connection, DatabricksConfig, DatabricksSignIn, DriverError, Jobs,
    Password, ProgressHandle, Rejection, RelationName, ResultCursor, Row, ServerInfo,
    TableConstraints, TableDescription, TableName, TransactionStatus,
};
use serde::Deserialize;

use crate::api::Api;
use crate::catalog::Lister;
use crate::context::{Context, NeedsSession, Use};
use crate::statements::DatabricksJobs;

/// What DbRover calls the service in messages and on Test connection.
const PRODUCT: &str = "Databricks";

/// The SQL warehouses' API, below the workspace's address. Reading a warehouse doesn't start
/// it.
const WAREHOUSES: &str = "/api/2.0/sql/warehouses";

/// What reading a warehouse answers that DbRover uses.
#[derive(Debug, Deserialize)]
struct Warehouse {
    name: String,
}

/// Signs in as `config` says, with `secret` (a personal access token or a service principal's
/// client secret), and reads the data source's warehouse, which proves the workspace answers,
/// takes the sign-in and has the warehouse, without starting it.
///
/// # Errors
///
/// [`DriverError::Connect`] when the data source names no host or warehouse, a secret is
/// missing, Databricks can't be reached, or it refuses the sign-in or doesn't know the
/// warehouse.
pub async fn connect(
    config: &DatabricksConfig,
    secret: Option<&Password>,
) -> Result<DatabricksConnection, DriverError> {
    for (value, what) in [
        (&config.host, "host: the workspace's address"),
        (
            &config.warehouse,
            "SQL warehouse: Databricks runs every statement on one",
        ),
    ] {
        if value.trim().is_empty() {
            return Err(DriverError::Connect(
                format!("Set the data source's {what}").into(),
            ));
        }
    }
    let address = config.address();
    let api = Api::new(&address, signing_in(config, &address, secret)?).map_err(errors::connect)?;
    let warehouse_id = config.warehouse.trim().to_owned();
    let warehouse: Warehouse = api
        .get(&format!("{WAREHOUSES}/{warehouse_id}"))
        .await
        .and_then(|body| {
            serde_json::from_value(body)
                .map_err(|error| api::ApiError::Unreadable(error.to_string()))
        })
        .map_err(errors::connect)?;
    let context = Context {
        catalog: config.catalog.trim().to_owned(),
        schema: config.schema.trim().to_owned(),
    };
    let statements = Arc::new(DatabricksJobs::new(api, warehouse_id, context));
    let jobs = Jobs::new(Arc::clone(&statements) as Arc<dyn dbrover_core::JobService>);
    Ok(DatabricksConnection {
        server_info: ServerInfo::new(
            PRODUCT,
            format!("{}, warehouse {}", config.host.trim(), warehouse.name),
        ),
        warehouse: config.warehouse.trim().to_owned(),
        statements,
        jobs,
    })
}

/// How the data source signs in, once its secret is in hand.
fn signing_in(
    config: &DatabricksConfig,
    address: &str,
    secret: Option<&Password>,
) -> Result<sign_in::DatabricksSignIn, DriverError> {
    let secret = |name: &str| {
        secret.cloned().ok_or_else(|| {
            DriverError::Connect(
                format!(
                    "Give the data source's {name}: Databricks signs this data source in with it"
                )
                .into(),
            )
        })
    };
    Ok(match &config.sign_in {
        DatabricksSignIn::PersonalAccessToken => {
            sign_in::DatabricksSignIn::Token(secret("personal access token")?)
        }
        DatabricksSignIn::ServicePrincipal { client_id } => {
            if client_id.trim().is_empty() {
                return Err(DriverError::Connect(
                    "Set the service principal's client ID: Databricks signs it in with its ID \
                     and secret"
                        .into(),
                ));
            }
            let principal =
                ServicePrincipalSignIn::new(address, client_id, secret("client secret")?)
                    .map_err(|error| DriverError::Connect(Box::new(error)))?;
            sign_in::DatabricksSignIn::ServicePrincipal(Box::new(principal))
        }
        DatabricksSignIn::DatabricksCli => {
            sign_in::DatabricksSignIn::DatabricksCli(CliSignIn::new(address))
        }
        DatabricksSignIn::AzureCli { tenant } => {
            sign_in::DatabricksSignIn::Entra(EntraDatabricksSignIn::new(EntraSignIn {
                tool: EntraTool::AzureCli,
                tenant: tenant.clone(),
            }))
        }
    })
}

/// A console's or the explorer's connection to one Databricks workspace and warehouse.
pub struct DatabricksConnection {
    server_info: ServerInfo,
    warehouse: String,
    /// The statements' own service, whose context a `USE` changes; `jobs` runs them.
    statements: Arc<DatabricksJobs>,
    jobs: Arc<Jobs>,
}

impl DatabricksConnection {
    fn lister(&self) -> Lister<'_> {
        Lister {
            api: self.statements.api(),
        }
    }
}

#[async_trait]
impl Connection for DatabricksConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    /// Reads the warehouse again, which starts nothing.
    async fn ping(&mut self) -> Result<(), DriverError> {
        self.statements
            .api()
            .get(&format!("{WAREHOUSES}/{}", self.warehouse))
            .await
            .map(drop)
            .map_err(errors::connection)
    }

    /// The level's first thousand nodes: the explorer pages through the rest with
    /// [`Connection::children_page`].
    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        let page = self
            .children_page(path, &ChildrenQuery::first_page(None))
            .await?;
        Ok(page.nodes)
    }

    /// From Unity Catalog's REST API, which starts no warehouse.
    async fn children_page(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        self.lister().page(path, query).await
    }

    /// Unity Catalog's table summaries, catalog by catalog, which start no warehouse.
    async fn search_catalog(
        &mut self,
        search: &CatalogSearch,
    ) -> Result<Option<CatalogMatches>, DriverError> {
        self.lister().search(search).await.map(Some)
    }

    /// A view's text, or a table's `CREATE TABLE` written from its columns, from Unity
    /// Catalog: no warehouse runs to show a definition.
    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        self.lister().definition(path).await
    }

    /// A statement, but for a `USE`, which changes what this connection's later statements
    /// are sent with, and what would need a session, which isn't sent: the API keeps none.
    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        if let Some(change) = Use::recognise(sql) {
            self.statements.apply(&change);
            return Ok(Box::new(Said::new(change.to_string())));
        }
        if let Some(needs) = NeedsSession::of(sql) {
            return Err(DriverError::Statement {
                source: needs.refusal().into(),
                position: None,
                sqlstate: None,
            });
        }
        self.jobs.execute(sql).await
    }

    /// `None`: Databricks's keys are informational and not enforced, so nothing tells a
    /// table's rows apart and they are never edited (`Dialect::read_only_results`).
    async fn describe_table(
        &mut self,
        _table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        Ok(None)
    }

    async fn describe_constraints(
        &mut self,
        _table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        Ok(TableConstraints::default())
    }

    /// Not checked as it is typed: an `EXPLAIN` is a statement, which would start a stopped
    /// warehouse, so the syntax check alone runs (`docs/cloud-data.md`, C6's decisions).
    async fn validate(&mut self, _sql: &str) -> Result<Option<Rejection>, DriverError> {
        Ok(None)
    }

    /// Each statement commits on its own: the API keeps no transaction between statements.
    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        Ok(TransactionStatus::Idle)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::clone(&self.jobs) as Arc<dyn CancelHandle>
    }

    fn progress_handle(&self) -> Option<Arc<dyn ProgressHandle>> {
        Some(Arc::clone(&self.jobs) as Arc<dyn ProgressHandle>)
    }

    /// Nothing to close: the API keeps no session.
    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

/// What the connection answers for a statement it handles itself: one row saying what it did.
struct Said {
    columns: [ColumnInfo; 1],
    row: Option<Row>,
}

impl Said {
    fn new(message: String) -> Self {
        Self {
            columns: [ColumnInfo {
                name: "status".to_owned(),
                type_name: Some("STRING".to_owned()),
            }],
            row: Some(vec![Some(message)]),
        }
    }
}

#[async_trait]
impl ResultCursor for Said {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        Ok(match max_rows {
            0 => Vec::new(),
            _ => self.row.take().into_iter().collect(),
        })
    }

    fn is_exhausted(&self) -> bool {
        self.row.is_none()
    }

    async fn finish(self: Box<Self>) -> Result<Option<u64>, DriverError> {
        Ok(None)
    }
}
