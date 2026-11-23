//! Snowflake driver for DbRover, over Snowflake's SQL API (`docs/cloud-data.md`, C5).
//!
//! A statement is submitted with `async=true` and DbRover's own `requestId`, polled by its
//! handle until it is done, and its result read a partition at a time. Those four calls are
//! C0's [`JobService`](dbrover_core::JobService) ([`statements`]), so the cursor, cancel and
//! progress handles are [`Jobs`]'s. The SQL API keeps no session, so a connection is an API
//! client and the context ([`context`]) it sends with each statement, signed in with C5c's key
//! pair or token.

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
use dbrover_cloud::snowflake::{self as sign_in, KeyPair, KeyPairSignIn, TokenType};
use dbrover_core::{
    CancelHandle, CatalogMatches, CatalogNode, CatalogPath, CatalogSearch, ChildrenPage,
    ChildrenQuery, ColumnInfo, Connection, DriverError, Jobs, Password, ProgressHandle, Rejection,
    RelationName, ResultCursor, Row, ServerInfo, SnowflakeConfig, SnowflakeSignIn,
    TableConstraints, TableDescription, TableName, TransactionStatus,
};

use crate::api::Api;
use crate::catalog::Lister;
use crate::context::{Context, Use};
use crate::statements::SnowflakeJobs;

/// What DbRover calls the service in messages and on Test connection.
const PRODUCT: &str = "Snowflake";

/// The cheapest statement that needs the data source's sign-in, account and role, and no
/// warehouse: `SHOW` commands are answered without one, and Snowflake doesn't bill them.
const PROBE: &str = "SHOW DATABASES LIMIT 1";

/// Signs in as `config` says, with `secret` (an encrypted key's passphrase, or the token
/// itself), and runs a `SHOW` that proves the account answers and accepts the sign-in.
///
/// # Errors
///
/// [`DriverError::Connect`] when the data source names no account or user, its key can't be
/// read, a token is missing, Snowflake can't be reached, or it refuses the sign-in.
pub async fn connect(
    config: &SnowflakeConfig,
    secret: Option<&Password>,
) -> Result<SnowflakeConnection, DriverError> {
    for (value, what) in [(&config.account, "account"), (&config.user, "user")] {
        if value.trim().is_empty() {
            return Err(DriverError::Connect(
                format!("Set the data source's {what}: Snowflake signs in to an account as a user")
                    .into(),
            ));
        }
    }
    let api = Api::new(
        &config.account,
        &config.endpoint,
        signing_in(config, secret)?,
    )
    .map_err(errors::connect)?;
    let context = Context {
        warehouse: config.warehouse.clone(),
        role: config.role.clone(),
        database: config.database.clone(),
        schema: config.schema.clone(),
    };
    let statements = Arc::new(SnowflakeJobs::new(api, context));
    let jobs = Jobs::new(Arc::clone(&statements) as Arc<dyn dbrover_core::JobService>);
    let mut connection = SnowflakeConnection {
        server_info: ServerInfo::new(PRODUCT, describe_place(config)),
        statements,
        jobs,
    };
    connection.probe().await.map_err(|error| match error {
        DriverError::Statement { source, .. } | DriverError::Connection(source) => {
            DriverError::Connect(source)
        }
        other => other,
    })?;
    Ok(connection)
}

/// How the data source signs in, once its secret is in hand.
fn signing_in(
    config: &SnowflakeConfig,
    secret: Option<&Password>,
) -> Result<sign_in::SnowflakeSignIn, DriverError> {
    let token = |kind: TokenType, name: &str| match secret {
        Some(secret) => Ok(sign_in::SnowflakeSignIn::Token {
            kind,
            secret: secret.clone(),
        }),
        None => Err(DriverError::Connect(
            format!("Give the data source's {name}: Snowflake signs this data source in with it")
                .into(),
        )),
    };
    match &config.sign_in {
        SnowflakeSignIn::KeyPair { private_key_file } => {
            let key = KeyPair::read(private_key_file, secret)
                .map_err(|error| DriverError::Connect(Box::new(error)))?;
            Ok(sign_in::SnowflakeSignIn::KeyPair(Box::new(
                KeyPairSignIn::new(&config.account, &config.user, key),
            )))
        }
        SnowflakeSignIn::ProgrammaticAccessToken => token(
            TokenType::ProgrammaticAccessToken,
            "programmatic access token",
        ),
        SnowflakeSignIn::OAuth => token(TokenType::OAuth, "OAuth token"),
    }
}

/// `account myorg-myaccount as JANE`, which Test connection shows as the version.
fn describe_place(config: &SnowflakeConfig) -> String {
    format!("account {} as {}", config.account, config.user)
}

/// A console's or the explorer's connection to one Snowflake account.
pub struct SnowflakeConnection {
    server_info: ServerInfo,
    /// The statements' own service, whose context a `USE` changes; `jobs` runs them.
    statements: Arc<SnowflakeJobs>,
    jobs: Arc<Jobs>,
}

impl SnowflakeConnection {
    fn lister(&self) -> Lister<'_> {
        Lister { jobs: &self.jobs }
    }

    async fn probe(&mut self) -> Result<(), DriverError> {
        let cursor = self.jobs.execute(PROBE).await?;
        cursor.finish().await.map(drop)
    }
}

#[async_trait]
impl Connection for SnowflakeConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        self.probe().await
    }

    /// The level's first thousand nodes: the explorer pages through the rest with
    /// [`Connection::children_page`].
    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        let page = self
            .children_page(path, &ChildrenQuery::first_page(None))
            .await?;
        Ok(page.nodes)
    }

    /// From a `SHOW`, which needs no warehouse.
    async fn children_page(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        self.lister().page(path, query).await
    }

    /// `SHOW OBJECTS … IN ACCOUNT`, which needs no warehouse.
    async fn search_catalog(
        &mut self,
        search: &CatalogSearch,
    ) -> Result<Option<CatalogMatches>, DriverError> {
        self.lister().search(search).await.map(Some)
    }

    /// A view's text, or a table's `CREATE TABLE` written from its columns, from `SHOW`
    /// commands: no warehouse runs to show a definition.
    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        self.lister().definition(path).await
    }

    /// A statement, but for a `USE`, which changes what this connection's later statements
    /// are sent with, since the SQL API keeps no session to change.
    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        if let Some(change) = Use::recognise(sql) {
            self.statements.apply(&change);
            return Ok(Box::new(Said::new(change.to_string())));
        }
        self.jobs.execute(sql).await
    }

    /// `None`: Snowflake's keys are informational and not enforced, so nothing tells a table's
    /// rows apart and they are never edited (`Dialect::read_only_results`).
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

    /// Not checked as it is typed: an `EXPLAIN` needs no warehouse but is billed as cloud
    /// services, so the syntax check alone runs (`docs/cloud-data.md`, C5's decisions).
    async fn validate(&mut self, _sql: &str) -> Result<Option<Rejection>, DriverError> {
        Ok(None)
    }

    /// Each statement commits on its own: a transaction lasts only within one request.
    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        Ok(TransactionStatus::Idle)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::clone(&self.jobs) as Arc<dyn CancelHandle>
    }

    fn progress_handle(&self) -> Option<Arc<dyn ProgressHandle>> {
        Some(Arc::clone(&self.jobs) as Arc<dyn ProgressHandle>)
    }

    /// Nothing to close: Snowflake keeps no session for the SQL API.
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
                type_name: Some("VARCHAR".to_owned()),
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
