//! Google BigQuery driver for DbRover, over BigQuery's REST API (`docs/cloud-data.md`, C4).
//!
//! A statement runs as a query job in the data source's project: inserted with DbRover's own
//! job id, polled until it is done, and its result read a page at a time. Those four calls are
//! C0's [`JobService`](dbrover_core::JobService) ([`jobs`]), so the cursor, cancel and progress
//! handles are [`Jobs`]'s. BigQuery keeps no session between jobs and runs no transaction
//! outside a script, so a connection is an API client, signed in with C0b's Google sign-in.

mod api;
mod catalog;
mod dry_run;
mod errors;
mod jobs;
mod preview;
mod results;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use async_trait::async_trait;
use dbrover_core::{
    BigQueryConfig, ByteCount, CancelHandle, CatalogNode, CatalogPath, ChildrenPage, ChildrenQuery,
    Connection, CostEstimate, DriverError, Jobs, ProgressHandle, Rejection, RelationName,
    ResultCursor, ScanEstimate, ServerInfo, TableConstraints, TableDescription, TableName,
    TransactionStatus,
};

use crate::api::{Api, segment};
use crate::catalog::{Lister, NamedProjects};
use crate::dry_run::{DryRun, DryRuns};
use crate::jobs::{BigQueryJobs, JobPlace};
use crate::preview::{Preview, PreviewCursor};
use crate::results::Schema;

/// What DbRover calls the service in messages and on Test connection.
const PRODUCT: &str = "BigQuery";

/// Signs in as `config` says and lists one of its project's datasets, which proves the
/// sign-in works, the endpoint answers and the project can be read.
///
/// # Errors
///
/// [`DriverError::Connect`] when the data source names no project, the sign-in can't be used
/// (no Application Default Credentials, an unreadable credentials file, a refresh token
/// Google refused), BigQuery can't be reached, or it refuses the project.
pub async fn connect(config: &BigQueryConfig) -> Result<BigQueryConnection, DriverError> {
    if config.project.trim().is_empty() {
        return Err(DriverError::Connect(
            "Set the data source's project: the Google Cloud project its queries run in".into(),
        ));
    }
    let api = Api::new(&config.endpoint, &config.sign_in).map_err(errors::connect)?;
    let datasets_path = format!("/projects/{}/datasets", segment(&config.project));
    let probe = Probe {
        api: api.clone(),
        path: datasets_path,
    };
    probe.run().await.map_err(errors::connect)?;
    let place = JobPlace {
        project: config.project.clone(),
        dataset: config.dataset.clone(),
        location: config.location.clone(),
        limit: config.cost_guard.ask_above_bytes,
    };
    let dry_runs = DryRuns::new(api.clone(), place.clone());
    let query_jobs = Arc::new(BigQueryJobs::new(api, place));
    let jobs = Jobs::new(Arc::clone(&query_jobs) as Arc<dyn dbrover_core::JobService>);
    Ok(BigQueryConnection {
        dry_runs,
        query_jobs,
        server_info: ServerInfo::new(PRODUCT, describe_place(config)),
        api: probe.api.clone(),
        project: config.project.clone(),
        other_projects: config.other_projects.clone(),
        probe,
        jobs,
    })
}

/// `project analytics in EU`, which Test connection shows as the version.
fn describe_place(config: &BigQueryConfig) -> String {
    match config.location.as_str() {
        "" => format!("project {}", config.project),
        location => format!("project {} in {location}", config.project),
    }
}

/// The cheapest request that needs the data source's sign-in and project: one dataset of the
/// project, which is free.
struct Probe {
    api: Api,
    path: String,
}

impl Probe {
    async fn run(&self) -> Result<(), api::ApiError> {
        self.api
            .get::<serde_json::Value>(&self.path, &[("maxResults", "1")])
            .await
            .map(drop)
    }
}

/// A console's or the explorer's connection to BigQuery in one project.
pub struct BigQueryConnection {
    server_info: ServerInfo,
    api: Api,
    /// The project jobs run in, listed first in the explorer.
    project: String,
    other_projects: Vec<String>,
    probe: Probe,
    dry_runs: DryRuns,
    /// The jobs' own service, to let a statement past the limit; `jobs` runs them.
    query_jobs: Arc<BigQueryJobs>,
    jobs: Arc<Jobs>,
}

impl BigQueryConnection {
    /// The schema of the table `statement` previews, when it is **Open data**'s statement on a
    /// table, which `tabledata.list` answers for free.
    async fn previewed(&self, statement: &str) -> Result<Option<(Preview, Schema)>, DriverError> {
        let Some(preview) = Preview::recognise(statement) else {
            return Ok(None);
        };
        let table = self
            .lister()
            .table(&preview.project, &preview.dataset, &preview.table)
            .await?;
        Ok(table.previewable().map(|schema| (preview, schema)))
    }

    fn lister(&self) -> Lister<'_> {
        Lister {
            api: &self.api,
            projects: NamedProjects {
                own: &self.project,
                others: &self.other_projects,
            },
        }
    }
}

#[async_trait]
impl Connection for BigQueryConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        self.probe.run().await.map_err(errors::connection)
    }

    /// The level's first thousand nodes: the explorer pages through the rest with
    /// [`Connection::children_page`].
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

    /// From `tables.get`, which is free: no job runs to show a definition.
    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        self.lister().definition(path).await
    }

    /// A query job, but for **Open data**'s statement on a table, which reads its rows with
    /// `tabledata.list` for free.
    async fn execute(&mut self, sql: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        if let Some((preview, schema)) = self.previewed(sql).await? {
            return Ok(Box::new(PreviewCursor::new(
                self.api.clone(),
                &preview,
                schema,
            )));
        }
        self.jobs.execute(sql).await
    }

    /// `None`: BigQuery's tables have no enforced key that tells their rows apart, so their
    /// rows are never edited (`Dialect::read_only_results`).
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

    /// BigQuery's dry run, which is free and leaves no job in the project's history.
    async fn validate(&mut self, sql: &str) -> Result<Option<Rejection>, DriverError> {
        Ok(match self.dry_runs.check(sql).await? {
            DryRun::Accepted(_) => None,
            DryRun::Rejected(rejection) => Some(rejection),
        })
    }

    /// The bytes the dry run says `statement` will scan, which BigQuery bills whatever it
    /// returns; `None` for a statement BigQuery won't run, which says why when it is run.
    async fn estimate_cost(
        &mut self,
        statement: &str,
    ) -> Result<Option<CostEstimate>, DriverError> {
        if self.previewed(statement).await?.is_some() {
            return Ok(Some(CostEstimate::Bytes(ScanEstimate::exactly(ByteCount(
                0,
            )))));
        }
        Ok(match self.dry_runs.check(statement).await? {
            DryRun::Accepted(scan) => Some(CostEstimate::Bytes(scan)),
            DryRun::Rejected(_) => None,
        })
    }

    fn allow_next_past_limit(&mut self) {
        self.query_jobs.allow_next_past_limit();
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

    /// Nothing to close: BigQuery keeps no session.
    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}
