//! Amazon Athena driver for DbRover, on AWS's own SDK (`docs/cloud-data.md`, C3).
//!
//! A statement runs as an Athena query execution: started in the data source's workgroup,
//! catalog and database, polled until it ends, and its result read a page at a time. Those four
//! calls are C0's [`JobService`](dbrover_core::JobService) ([`jobs`]), so the cursor, cancel and
//! progress handles are [`Jobs`]'s. Athena keeps no session and runs no transactions, so a
//! connection is a client, signed in through `dbrover-cloud`'s AWS sign-in.

mod catalog;
mod errors;
mod jobs;
mod results;
#[cfg(test)]
mod tests;
mod workgroup;

use std::sync::Arc;

use async_trait::async_trait;
use aws_sdk_athena::Client;
use dbrover_cloud::aws::AwsSignIn;
use dbrover_core::{
    AthenaConfig, CancelHandle, CatalogNode, CatalogPath, ChildrenPage, ChildrenQuery, Connection,
    DriverError, Jobs, ProgressHandle, Rejection, RelationName, ResultCursor, ServerInfo,
    TableConstraints, TableDescription, TableName, TransactionStatus,
};

use crate::catalog::Lister;

use crate::jobs::{AthenaJobs, QueryPlace};
use crate::workgroup::Workgroup;

/// What DbRover calls the service in messages and on Test connection.
const PRODUCT: &str = "Athena";

/// Signs in as `config` says and reads its workgroup, which proves the credentials are good,
/// the endpoint answers and the workgroup exists, and says where its results go.
///
/// # Errors
///
/// [`DriverError::Connect`] when the sign-in can't be used (a missing profile, an expired
/// IAM Identity Center session, no region), Athena can't be reached or refuses it, the
/// workgroup doesn't exist, or neither the workgroup nor the data source says where results
/// go.
pub async fn connect(config: &AthenaConfig) -> Result<AthenaConnection, DriverError> {
    let sdk_config = sign_in(config)
        .sdk_config()
        .await
        .map_err(errors::connect)?;
    let client = Client::new(&sdk_config);
    let workgroup = Workgroup::read(&client, &config.workgroup)
        .await
        .map_err(errors::connect)?;
    let output_location = workgroup
        .output_location_for(&config.output_location)
        .map_err(errors::connect)?;
    let region = sdk_config
        .region()
        .map_or_else(String::new, ToString::to_string);
    let server_info = ServerInfo::new(PRODUCT, workgroup.describe_version(&region));
    let place = QueryPlace {
        workgroup: config.workgroup.clone(),
        catalog: config.catalog.clone(),
        database: config.database.clone(),
        output_location,
    };
    let limit = workgroup.limit_for(config.cost_guard.ask_above_bytes);
    let jobs = Jobs::new(Arc::new(AthenaJobs::new(client.clone(), place, limit)));
    Ok(AthenaConnection {
        client,
        catalog: config.catalog.clone(),
        workgroup_name: config.workgroup.clone(),
        server_info,
        jobs,
    })
}

fn sign_in(config: &AthenaConfig) -> AwsSignIn {
    AwsSignIn::from_settings(
        config.sign_in,
        &config.profile,
        &config.region,
        &config.endpoint,
    )
}

/// How many lines of a definition to read a page: as many as `GetQueryResults` returns.
const DEFINITION_PAGE_ROWS: usize = 1000;

/// A console's or the explorer's connection to Athena in one region and workgroup.
pub struct AthenaConnection {
    client: Client,
    /// The catalog the data source's queries run in.
    catalog: String,
    workgroup_name: String,
    server_info: ServerInfo,
    jobs: Arc<Jobs>,
}

impl AthenaConnection {
    fn lister(&self) -> Lister<'_> {
        Lister {
            client: &self.client,
            own_catalog: &self.catalog,
        }
    }
}

#[async_trait]
impl Connection for AthenaConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    /// Reads the workgroup again: free, and it needs the same sign-in a query does.
    async fn ping(&mut self) -> Result<(), DriverError> {
        Workgroup::read(&self.client, &self.workgroup_name)
            .await
            .map(drop)
            .map_err(errors::connection)
    }

    /// The level's first thousand nodes, which is twenty of Athena's requests: the explorer
    /// pages through the rest with [`Connection::children_page`].
    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        let page = self.children_page(path, &ChildrenQuery::default()).await?;
        Ok(page.nodes)
    }

    async fn children_page(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        self.lister().page(path, query).await
    }

    /// `SHOW CREATE TABLE` or `SHOW CREATE VIEW`, run as a query, whose rows are the lines of
    /// the definition.
    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        let statement = catalog::definition_statement(path, &self.catalog)?;
        let mut cursor = self.jobs.execute(&statement).await?;
        let mut lines = Vec::new();
        while !cursor.is_exhausted() {
            let rows = cursor.next_page(DEFINITION_PAGE_ROWS).await?;
            lines.extend(
                rows.into_iter()
                    .filter_map(|row| row.into_iter().next().flatten()),
            );
        }
        cursor.finish().await?;
        Ok(lines.join("\n"))
    }

    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        self.jobs.execute(sql).await
    }

    /// `None`: Athena's tables have no key that tells their rows apart, so their rows are never
    /// edited (`Dialect::read_only_results`).
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

    /// `None`, always: Athena can check a statement only by running a query (`EXPLAIN`), which
    /// would add one to the workgroup's history each time the console pauses, with a second or
    /// more of polling. Athena's own errors show when the statement runs.
    async fn validate(&mut self, _sql: &str) -> Result<Option<Rejection>, DriverError> {
        Ok(None)
    }

    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        Ok(TransactionStatus::Idle)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::clone(&self.jobs) as Arc<dyn CancelHandle>
    }

    fn progress_handle(&self) -> Option<Arc<dyn ProgressHandle>> {
        Some(Arc::clone(&self.jobs) as Arc<dyn ProgressHandle>)
    }

    /// Nothing to close: Athena keeps no session.
    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}
