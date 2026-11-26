//! `search_data`: the rows that hold a value across a data source's tables, for any project
//! that may read (W16, `docs/data-search.md`).
//!
//! It runs the statements the Search data tab runs, each table in a read-only transaction of
//! its own under the project's statement time limit, so one table that runs out of time leaves
//! the others to be searched. The project's schemas narrow it, and its row limit caps the rows
//! over every table. Refused on the engines that bill each read, since there is no one to ask.

use std::time::Instant;

use dbrover_core::{
    CaseMatch, CatalogPath, DataSearch, DataSource, MatchKind, Relations, RowsPerTable,
    SearchScope, SearchableTable, TablesToSearch,
};
use dbrover_language::QueryLanguage;
use dbrover_sql::{SearchSupport, search_statement};
use dbrover_store::HistoryOutcome;
use jiff::Timestamp;
use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit::Ran;
use crate::server::{DbRoverMcp, ToolError};
use crate::transaction::{self, Access, RunError};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(crate) struct SearchArgs {
    /// The data source's name, as list_data_sources gives it (or its id).
    data_source: String,
    /// The value to find: text, a number, a UUID or a date (YYYY-MM-DD). `%` and `_` are
    /// matched as themselves.
    value: String,
    /// "contains" (the default) finds the value anywhere in a text column; "whole" finds a
    /// text column holding exactly it. Numbers, UUIDs and dates are always whole values.
    #[serde(default)]
    r#match: MatchChoice,
    /// Match case in text columns; ignored by default.
    #[serde(default)]
    case_sensitive: bool,
    /// Only these schemas, by name; every schema the project allows when left out.
    #[serde(default)]
    schemas: Vec<String>,
    /// Only these tables, as `schema.table`; every table of the schemas when left out.
    #[serde(default)]
    tables: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MatchChoice {
    #[default]
    Contains,
    Whole,
}

/// How one table's search ended, for the reply.
enum Found {
    Rows {
        selected: Vec<String>,
        rows: Vec<dbrover_core::Row>,
        more: bool,
    },
    Nothing,
    TimedOut,
    Failed(String),
}

impl DbRoverMcp {
    pub(crate) async fn search_json(&self, args: SearchArgs) -> Result<Value, ToolError> {
        let tool = "search_data";
        let data_source = self.data_source(tool, &args.data_source)?;
        self.inner.audit.call(tool, Some(&data_source.name), None);
        let search = DataSearch::new(
            &args.value,
            match args.r#match {
                MatchChoice::Contains => MatchKind::Contains,
                MatchChoice::Whole => MatchKind::WholeValue,
            },
            if args.case_sensitive {
                CaseMatch::Match
            } else {
                CaseMatch::Ignore
            },
        )
        .map_err(|error| ToolError::invalid(error.to_string()))?;
        let Some(dialect) = QueryLanguage::from(data_source.config.driver()).dialect() else {
            let reason = "search_data searches SQL databases; use query instead".to_owned();
            return Err(self.refuse(tool, data_source, reason));
        };
        let service = self.service_of(tool, data_source).await?;
        match dialect.data_search(service) {
            SearchSupport::Searchable => {}
            SearchSupport::Unsupported => {
                let reason = "this database's data can't be searched yet".to_owned();
                return Err(self.refuse(tool, data_source, reason));
            }
            SearchSupport::AsksFirst => {
                let reason = format!(
                    "{} bills each read, or reads a lake's tables whole, and a search reads \
                     many tables, which DbRover asks a person about first; run a table's \
                     statement with query instead, under the data source's cost guard",
                    data_source.config.driver().display_name()
                );
                return Err(self.refuse(tool, data_source, reason));
            }
        }
        let tables = self.tables_to_search(tool, data_source, &args).await?;

        let max_rows = self.inner.project.limits.max_rows;
        let per_table = std::num::NonZeroU32::new(u32::try_from(max_rows).unwrap_or(u32::MAX))
            .map_or(RowsPerTable::DEFAULT, RowsPerTable::new);
        let started = Timestamp::now();
        let clock = Instant::now();
        let mut left = max_rows;
        let mut found: Vec<(&SearchableTable, Found)> = Vec::new();
        let mut statements: Vec<String> = Vec::new();
        let mut no_columns: Vec<String> = Vec::new();
        for table in &tables {
            let Some(statement) = search_statement(dialect, table, &search, per_table) else {
                no_columns.push(label(table));
                continue;
            };
            if left == 0 {
                break;
            }
            let sql = statement.sql.clone();
            let fetched = self
                .run_call(data_source, Access::Read, move |connection, watchdog| {
                    Box::pin(async move {
                        watchdog
                            .watch(transaction::fetch(connection, &sql, left))
                            .await
                    })
                })
                .await;
            let outcome = match fetched {
                Ok(rows) if rows.rows.is_empty() => Found::Nothing,
                Ok(rows) => {
                    left = left.saturating_sub(rows.rows.len());
                    statements.push(format!("{};", statement.sql));
                    Found::Rows {
                        selected: statement.selected.clone(),
                        more: rows.truncated,
                        rows: rows.rows,
                    }
                }
                Err(RunError::TimedOut(_)) => Found::TimedOut,
                Err(error) => Found::Failed(error.to_string()),
            };
            found.push((table, outcome));
        }
        let rows: u64 = found
            .iter()
            .map(|(_, outcome)| match outcome {
                Found::Rows { rows, .. } => u64::try_from(rows.len()).unwrap_or(u64::MAX),
                _ => 0,
            })
            .sum();
        let sql = if statements.is_empty() {
            format!(
                "-- search_data found no row holding \u{201c}{}\u{201d}",
                args.value
            )
        } else {
            statements.join("\n\n")
        };
        self.inner
            .audit
            .record(Ran {
                data_source,
                sql: &sql,
                started,
                elapsed: clock.elapsed(),
                rows: Some(rows),
                outcome: HistoryOutcome::Ok,
            })
            .await;
        Ok(reply(&found, &no_columns, left == 0))
    }

    /// The tables the search reads: the project's schemas, narrowed by the call's.
    async fn tables_to_search(
        &self,
        tool: &str,
        data_source: &DataSource,
        args: &SearchArgs,
    ) -> Result<Vec<SearchableTable>, ToolError> {
        let request = TablesToSearch {
            scope: SearchScope::Under(CatalogPath::root()),
            views: Relations::TablesOnly,
        };
        let mut tables = self
            .on_bare_connection(tool, data_source, |connection| {
                Box::pin(async move { connection.searchable_tables(&request).await })
            })
            .await?;
        let scope = self.scope(data_source);
        tables.retain(|table| {
            let schema = &table.name.schema;
            scope.as_ref().is_none_or(|scope| scope.allows(schema))
                && (args.schemas.is_empty() || args.schemas.contains(schema))
                && (args.tables.is_empty() || args.tables.contains(&label(table)))
        });
        tables.sort_by(|a, b| {
            (a.row_estimate.is_none(), a.row_estimate, label(a)).cmp(&(
                b.row_estimate.is_none(),
                b.row_estimate,
                label(b),
            ))
        });
        Ok(tables)
    }

    /// The managed service the data source's server said it is, which decides whether it can
    /// be searched (Redshift can't yet).
    async fn service_of(
        &self,
        tool: &str,
        data_source: &DataSource,
    ) -> Result<Option<dbrover_core::ManagedService>, ToolError> {
        self.on_bare_connection(tool, data_source, |connection| {
            Box::pin(async move { Ok(connection.server_info().service) })
        })
        .await
    }
}

/// `schema.table`, as the reply and the `tables` argument name a table.
fn label(table: &SearchableTable) -> String {
    format!("{}.{}", table.name.schema, table.name.name)
}

fn reply(
    found: &[(&SearchableTable, Found)],
    no_columns: &[String],
    row_limit_reached: bool,
) -> Value {
    let mut matched = Vec::new();
    let mut no_match = Vec::new();
    let mut timed_out = Vec::new();
    let mut failed = Vec::new();
    for (table, outcome) in found {
        match outcome {
            Found::Rows {
                selected,
                rows,
                more,
            } => matched.push(json!({
                "table": label(table),
                "key": table.key,
                "columns": selected,
                "rows": rows,
                "more": more,
            })),
            Found::Nothing => no_match.push(label(table)),
            Found::TimedOut => timed_out.push(label(table)),
            Found::Failed(message) => failed.push(json!({
                "table": label(table),
                "error": message,
            })),
        }
    }
    json!({
        "matched": matched,
        "no_match": no_match,
        "timed_out": timed_out,
        "failed": failed,
        "no_column_can_hold_the_value": no_columns,
        "row_limit_reached": row_limit_reached,
    })
}
