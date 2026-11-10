//! A report's source explained on its real data source, without running it
//! (`docs/reports-at-scale.md`, RS2d):
//!
//! 1. its statement checked to only read, with every value written in;
//! 2. a connection opened as a run opens one, read-only where the engine has such a mode;
//! 3. the statement explained the way its engine explains without running anything: a plan
//!    DbRover draws where it reads the engine's format, the engine's text otherwise, nothing
//!    where explaining would run the statement;
//! 4. the data source's cost guard asked what the statement would cost, and whether a run
//!    would refuse it.

use dbrover_core::{
    Connection, CostEstimate, CostGuard, DriverKind, ReadPace, Row, describe_error,
};
use dbrover_plan::PlanFormat;
use dbrover_report::{Estimate, ExplainedPlan, SourceExplained, SourceExplainer, SourceStatement};
use dbrover_sql::{Dialect, ExplainDepth, ExplainSetting};

use crate::open_data_source;
use crate::read_only::{self, Guard, Storage};
use crate::report_source::{SourceAccess, password_hint};

/// Rows of an `EXPLAIN`'s answer read at most: a text plan of a few hundred lines.
const PLAN_ROWS: usize = 1_000;

/// How an engine explains a statement without running it.
enum Explaining {
    /// A prefix whose answer is a plan DbRover reads, in the row's cell `column`, with a
    /// setting switched on around it where the engine needs one.
    Drawn {
        prefix: &'static str,
        format: PlanFormat,
        column: usize,
        setting: Option<ExplainSetting>,
    },
    /// A prefix whose answer is rows of text.
    Text { prefix: &'static str },
    /// Not explained, and why.
    Unexplained(&'static str),
}

/// DuckDB's `EXPLAIN` answers a row of `explain_key` and `explain_value`.
const DUCKDB_PLAN_COLUMN: usize = 1;

fn explaining(
    driver: DriverKind,
    dialect: Option<Dialect>,
    server: &dbrover_core::ServerInfo,
) -> Explaining {
    match driver {
        DriverKind::DuckDb => Explaining::Drawn {
            prefix: "EXPLAIN (FORMAT JSON) ",
            format: PlanFormat::DuckDbJson,
            column: DUCKDB_PLAN_COLUMN,
            setting: None,
        },
        DriverKind::ClickHouse => Explaining::Drawn {
            prefix: "EXPLAIN json = 1, indexes = 1 ",
            format: PlanFormat::ClickHouseJson,
            column: 0,
            setting: None,
        },
        DriverKind::Athena => Explaining::Unexplained(
            "Athena runs EXPLAIN as a query of its own, which it bills, so it isn't explained.",
        ),
        DriverKind::BigQuery => {
            Explaining::Unexplained("BigQuery has no EXPLAIN; its dry run's estimate is below.")
        }
        DriverKind::Postgres
        | DriverKind::Mysql
        | DriverKind::SqlServer
        | DriverKind::Sqlite
        | DriverKind::Trino => {
            let Some(dialect) = dialect else {
                return Explaining::Unexplained("Its language has no EXPLAIN.");
            };
            match dialect.plan_request(ExplainDepth::Estimated, Some(server)) {
                Some(request) => Explaining::Drawn {
                    prefix: request.prefix,
                    format: request.format,
                    column: 0,
                    setting: dialect.explain_setting(ExplainDepth::Estimated, Some(server)),
                },
                None => Explaining::Text {
                    prefix: dialect.explain_prefix(Some(server)),
                },
            }
        }
        DriverKind::Redis
        | DriverKind::Mongo
        | DriverKind::Elasticsearch
        | DriverKind::Cassandra
        | DriverKind::DynamoDb => {
            Explaining::Unexplained("Its engine has no plan DbRover asks for before a run.")
        }
    }
}

impl SourceExplainer for SourceAccess<'_> {
    async fn explain(&self, statement: &SourceStatement<'_>) -> SourceExplained {
        let source = statement.source;
        let failed = |statement: String, message: String| SourceExplained {
            statement,
            plan: ExplainedPlan::Failed { message },
            estimate: None,
        };
        let checked = match source.language.dialect() {
            Some(dialect) => {
                match dbrover_sql::inline(&statement.query, dialect, &statement.parameters) {
                    Ok(checked) => checked.sql,
                    Err(missing) => return failed(statement.query.clone(), missing.to_string()),
                }
            }
            None => statement.query.clone(),
        };
        if let Err(refusal) = read_only::check(&checked, source.language) {
            return failed(
                checked,
                format!(
                    "{}; a report's source only reads, so it isn't sent",
                    refusal.reason()
                ),
            );
        }
        let data_source = read_only::connection_config(source.data_source.clone());
        let config = &data_source.config;
        let connected =
            open_data_source(&data_source, self.secrets, self.tunnels, self.commands).await;
        let mut connection = match connected {
            Ok(connection) => connection,
            Err(error) => {
                let message = format!(
                    "could not connect to {:?}: {}{}",
                    data_source.name,
                    describe_error(&error),
                    password_hint(config.uses_password())
                );
                return failed(checked, message);
            }
        };
        let mode = match read_only::guard(config.driver(), Storage::of(config)) {
            Guard::Session(mode) => Some(mode),
            Guard::Login | Guard::CheckOnly => None,
        };
        let explained = async {
            if let Some(mode) = mode {
                read_only::begin(connection.as_mut(), mode, Some(source.limits.timeout))
                    .await
                    .map_err(|error| describe_error(&error))?;
            }
            let how = explaining(
                config.driver(),
                source.language.dialect(),
                connection.server_info(),
            );
            let plan = plan(connection.as_mut(), how, &checked).await;
            let estimate = estimate(connection.as_mut(), config.cost_guard(), &checked).await;
            Ok::<_, String>((plan, estimate))
        }
        .await;
        if let Some(mode) = mode
            && let Err(error) = read_only::rollback(connection.as_mut(), mode).await
        {
            tracing::debug!(
                "could not roll back the explain: {}",
                describe_error(&error)
            );
        }
        if let Err(error) = connection.close().await {
            tracing::debug!(
                "could not close the explain's connection: {}",
                describe_error(&error)
            );
        }
        match explained {
            Ok((plan, estimate)) => SourceExplained {
                statement: checked,
                plan,
                estimate,
            },
            Err(message) => failed(checked, message),
        }
    }
}

/// The statement's plan, as `how` asks the engine for it.
async fn plan(connection: &mut dyn Connection, how: Explaining, sql: &str) -> ExplainedPlan {
    let failed = |error: dbrover_core::DriverError| ExplainedPlan::Failed {
        message: describe_error(&error),
    };
    match how {
        Explaining::Unexplained(reason) => ExplainedPlan::Unexplained {
            reason: reason.to_owned(),
        },
        Explaining::Text { prefix } => match rows(connection, &format!("{prefix}{sql}")).await {
            Ok((_, rows)) => ExplainedPlan::Text {
                text: plan_text(&rows),
            },
            Err(error) => failed(error),
        },
        Explaining::Drawn {
            prefix,
            format,
            column,
            setting,
        } => {
            if let Some(setting) = &setting
                && let Err(error) = run(connection, setting.on).await
            {
                return failed(error);
            }
            let answered = rows(connection, &format!("{prefix}{sql}")).await;
            if let Some(setting) = &setting
                && let Err(error) = run(connection, setting.off).await
            {
                tracing::debug!(
                    "could not switch the explain off: {}",
                    describe_error(&error)
                );
            }
            match answered {
                Ok((_, rows)) => match rows
                    .into_iter()
                    .find_map(|mut row| row.get_mut(column)?.take())
                {
                    Some(text) => ExplainedPlan::Drawn {
                        format: format.as_str().to_owned(),
                        text,
                    },
                    None => ExplainedPlan::Failed {
                        message: "the database returned no plan".to_owned(),
                    },
                },
                Err(error) => failed(error),
            }
        }
    }
}

/// What the data source's cost guard estimates of `sql`, where its engine estimates costs.
async fn estimate(
    connection: &mut dyn Connection,
    guard: Option<CostGuard>,
    sql: &str,
) -> Option<Estimate> {
    let estimated = match connection.estimate_cost(sql).await {
        Ok(estimated) => estimated?,
        Err(error) => {
            tracing::debug!(
                "could not estimate the source's cost: {}",
                describe_error(&error)
            );
            return None;
        }
    };
    let guard = guard.unwrap_or(CostGuard::OFF);
    let (bytes, at_least) = match &estimated {
        CostEstimate::Bytes(scan) => (Some(scan.bytes.0), scan.at_least),
        CostEstimate::WholeTable(_) => (None, false),
    };
    Some(Estimate {
        says: estimated.describe(),
        bytes,
        at_least,
        limit_bytes: guard.ask_above_bytes.map(|limit| limit.0),
        refused: guard.holds(&estimated, ReadPace::Whole),
    })
}

/// A plan answered as rows, as text: each row's values a line, apart by two spaces, as
/// SQLite's `EXPLAIN QUERY PLAN` and Trino's one value read.
fn plan_text(rows: &[Row]) -> String {
    rows.iter()
        .map(|row| {
            row.iter()
                .flatten()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs a statement whose answer doesn't matter, such as a setting.
async fn run(connection: &mut dyn Connection, sql: &str) -> Result<(), dbrover_core::DriverError> {
    let cursor = connection.execute(sql).await?;
    cursor.finish().await.map(drop)
}

/// Every row `sql` answers, up to [`PLAN_ROWS`], with its columns' names.
async fn rows(
    connection: &mut dyn Connection,
    sql: &str,
) -> Result<(Vec<String>, Vec<Row>), dbrover_core::DriverError> {
    let mut cursor = connection.execute(sql).await?;
    let columns = cursor
        .columns()
        .iter()
        .map(|column| column.name.clone())
        .collect();
    let mut rows = Vec::new();
    while !cursor.is_exhausted() && rows.len() < PLAN_ROWS {
        rows.extend(cursor.next_page(PLAN_ROWS - rows.len()).await?);
    }
    if let Err(error) = cursor.finish().await {
        tracing::debug!("the explain ended with: {}", describe_error(&error));
    }
    Ok((columns, rows))
}
