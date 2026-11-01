//! Amazon DynamoDB driver for DbRover, on AWS's own SDK (`docs/nosql.md`, N5).
//!
//! A console's statements are PartiQL, read by `dbrover_language::partiql` and sent as typed
//! through `ExecuteStatement`, every read DbRover makes included. Items are documents (ADR
//! 0006), read a page at a time with a bound on what each page reads ([`cursor`]). DynamoDB
//! keeps no session and the console runs no transactions, so a connection is a client, signed
//! in through `dbrover-cloud`'s AWS sign-in.

mod catalog;
mod cursor;
mod errors;
mod items;
mod keys;
mod running;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use async_trait::async_trait;
use aws_sdk_dynamodb::Client;
use dbrover_cloud::aws::{AwsCredentials, AwsSignIn};
use dbrover_core::{
    CancelHandle, CatalogNode, CatalogPath, Connection, DriverError, DynamoDbConfig,
    DynamoDbSignIn, Rejection, RejectionKind, RelationName, ResultCursor, ServerInfo,
    TableConstraints, TableDescription, TableName, TransactionStatus,
};
use dbrover_language::partiql::{self, Verb};

use crate::cursor::{ItemCursor, PageBound};
use crate::errors::Refused;
use crate::keys::KeyCache;
use crate::running::{DynamoCancel, Running};

/// What DbRover calls the service in messages and on Test connection.
const PRODUCT: &str = "DynamoDB";

/// Signs in as `config` says and lists one table, which proves the credentials are good and
/// the endpoint answers.
///
/// # Errors
///
/// [`DriverError::Connect`] when the sign-in can't be used (a missing profile, an expired
/// IAM Identity Center session, no region), or DynamoDB can't be reached or refuses it.
pub async fn connect(config: &DynamoDbConfig) -> Result<DynamoConnection, DriverError> {
    let sdk_config = sign_in(config)
        .sdk_config()
        .await
        .map_err(errors::connect)?;
    let client = Client::new(&sdk_config);
    client
        .list_tables()
        .limit(1)
        .send()
        .await
        .map_err(|error| errors::connect(Refused::from(error)))?;
    let region = sdk_config
        .region()
        .map_or_else(String::new, ToString::to_string);
    let version = match config.endpoint.as_str() {
        "" => region,
        endpoint => format!("{region} at {endpoint}"),
    };
    Ok(DynamoConnection {
        client,
        server_info: ServerInfo {
            product: PRODUCT.to_owned(),
            version,
        },
        running: Arc::new(Running::default()),
        keys: KeyCache::default(),
        bound: PageBound::DEFAULT,
    })
}

/// The SDK's sign-in for `config`: an empty profile is AWS's default chain.
fn sign_in(config: &DynamoDbConfig) -> AwsSignIn {
    let credentials = match (config.sign_in, config.profile.as_str()) {
        (DynamoDbSignIn::LocalEmulator, _) => AwsCredentials::LocalEmulator,
        (DynamoDbSignIn::Profile, "") => AwsCredentials::DefaultChain,
        (DynamoDbSignIn::Profile, profile) => AwsCredentials::Profile(profile.to_owned()),
    };
    let optional = |setting: &str| (!setting.is_empty()).then(|| setting.to_owned());
    AwsSignIn {
        credentials,
        region: optional(&config.region),
        endpoint: optional(&config.endpoint),
    }
}

/// A console's or the explorer's connection to DynamoDB in one region.
pub struct DynamoConnection {
    client: Client,
    server_info: ServerInfo,
    running: Arc<Running>,
    keys: KeyCache,
    bound: PageBound,
}

#[async_trait]
impl Connection for DynamoConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        self.client
            .list_tables()
            .limit(1)
            .send()
            .await
            .map(drop)
            .map_err(|error| errors::connection(Refused::from(error)))
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        catalog::children(&self.client, path).await
    }

    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        Err(DriverError::InvalidPath(path.clone()))
    }

    async fn execute(&mut self, text: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        let statement = partiql::parse(text).map_err(errors::parse)?;
        // The keys only order a document's attributes; a table DynamoDB can't describe gets
        // its own error from the statement.
        let leading = match self
            .keys
            .of(&self.client, &statement.table, statement.index.as_deref())
            .await
        {
            Ok(keys) => keys.leading(statement.index.as_deref()),
            Err(_) => Vec::new(),
        };
        let cursor = ItemCursor::start(
            self.client.clone(),
            Arc::clone(&self.running),
            self.bound,
            text,
            leading,
            statement.verb == Verb::Select,
        )
        .await?;
        Ok(Box::new(cursor))
    }

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

    /// Reads the statement only: DynamoDB can't check one without running it.
    async fn validate(&mut self, text: &str) -> Result<Option<Rejection>, DriverError> {
        Ok(partiql::parse(text).err().map(|error| Rejection {
            position: Some(error.range.start),
            message: error.message,
            kind: RejectionKind::Syntax,
        }))
    }

    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        Ok(TransactionStatus::Idle)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(DynamoCancel(Arc::clone(&self.running)))
    }

    /// Nothing to close: DynamoDB keeps no session, and the SDK's connections close with it.
    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}
