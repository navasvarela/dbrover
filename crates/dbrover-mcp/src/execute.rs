//! The `execute` tool: statements that change something, each allowed by the project's
//! permission for its kind, run in order in one transaction.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use dbrover_core::DataSource;
use dbrover_language::{Analysis, NamedSchemas, QueryLanguage, StatementKind};
use dbrover_sql::commits_implicitly;
use dbrover_store::HistoryOutcome;
use jiff::Timestamp;
use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit::Ran;
use crate::project::Permission;
use crate::server::{DbRoverMcp, ToolError};
use crate::transaction::{self, Access, Rows, RunError};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(crate) struct ExecuteArgs {
    /// The data source's name, as list_data_sources gives it (or its id).
    data_source: String,
    /// One or more statements, separated by semicolons, in the data source's dialect. They
    /// run in order, in one transaction, without BEGIN or COMMIT of their own.
    sql: String,
}

/// One statement of the call, as classified before anything runs.
struct Planned<'a> {
    text: &'a str,
    /// Where it is in the text the client sent, for error positions.
    range: Range<usize>,
    /// `Err` with the reason when the parser can't read it.
    analysis: Result<Analysis, String>,
}

impl Planned<'_> {
    /// What the statement needs beyond reading. A statement the parser can't read needs
    /// `admin`, because what it does can't be known.
    fn needs(&self) -> Result<Option<Permission>, &'static str> {
        let Ok(analysis) = &self.analysis else {
            return Ok(Some(Permission::Admin));
        };
        match analysis.kind {
            StatementKind::Read => Ok(None),
            StatementKind::Write => Ok(Some(Permission::Write)),
            StatementKind::Schema => Ok(Some(Permission::Schema)),
            StatementKind::Admin => Ok(Some(Permission::Admin)),
            StatementKind::Transaction => Err(
                "execute runs each call in a transaction of its own, so leave out BEGIN, \
                 COMMIT, ROLLBACK and savepoints",
            ),
        }
    }

    fn describe(&self) -> String {
        match &self.analysis {
            Ok(analysis) => analysis.kind.description().to_owned(),
            Err(reason) => format!("can't be read ({reason}), so what it does is unknown"),
        }
    }

    fn schemas(&self) -> NamedSchemas {
        match &self.analysis {
            Ok(analysis) => analysis.schemas.clone(),
            Err(_) => NamedSchemas::Unknown,
        }
    }
}

impl DbRoverMcp {
    pub(crate) async fn execute_json(&self, args: ExecuteArgs) -> Result<Value, ToolError> {
        let tool = "execute";
        let data_source = self.data_source(tool, &args.data_source)?;
        let language = QueryLanguage::from(data_source.config.driver());
        let planned = plan(&args.sql, language);
        if planned.is_empty() {
            return Err(ToolError::invalid("there is no statement to run"));
        }
        self.inner.audit.call(
            tool,
            Some(&data_source.name),
            planned
                .iter()
                .filter_map(|statement| statement.analysis.as_ref().ok())
                .map(|analysis| analysis.kind)
                .max(),
        );
        self.check_allowed(tool, data_source, &planned)?;
        let driver = data_source.config.driver();
        if transaction::mode(&data_source.config, Access::Write, true).is_none() {
            let reason = format!(
                "DbRover can't run changes on {} for MCP clients yet: this data source is \
                 served for reading only",
                driver.display_name()
            );
            return Err(self.refuse(tool, data_source, reason));
        }

        let texts: Vec<String> = planned
            .iter()
            .map(|statement| statement.text.to_owned())
            .collect();
        let atomic = !texts.iter().any(|text| {
            language
                .dialect()
                .is_some_and(|dialect| commits_implicitly(text, dialect))
        });
        let max_rows = self.inner.project.limits.max_rows;
        let done = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&done);
        let cost_guard = data_source.config.cost_guard();
        let started = Timestamp::now();
        let clock = Instant::now();
        let outcome = self
            .run_call(data_source, Access::Write, move |connection, watchdog| {
                Box::pin(async move {
                    let mut results = Vec::with_capacity(texts.len());
                    for text in &texts {
                        transaction::check_scan(connection, cost_guard, text).await?;
                        let rows = watchdog
                            .watch(transaction::fetch(connection, text, max_rows))
                            .await?;
                        results.push(rows);
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(results)
                })
            })
            .await;
        let elapsed = clock.elapsed();
        self.inner
            .audit
            .record(Ran {
                data_source,
                sql: args.sql.trim(),
                started,
                elapsed,
                rows: outcome.as_ref().ok().map(|results| {
                    results
                        .iter()
                        .map(|rows| rows.row_count.unwrap_or(rows.rows.len() as u64))
                        .sum()
                }),
                outcome: match &outcome {
                    Ok(_) => HistoryOutcome::Ok,
                    Err(RunError::TimedOut(_)) => HistoryOutcome::Cancelled,
                    Err(_) => HistoryOutcome::Error,
                },
            })
            .await;
        match outcome {
            Ok(results) => Ok(results_json(&planned, &results, atomic, elapsed)),
            Err(error) => {
                let failed = done.load(Ordering::Relaxed);
                Err(self.statement_failure(tool, data_source, &planned, failed, atomic, error))
            }
        }
    }

    /// Refuses the call, before anything runs, if any statement needs a permission the
    /// project lacks or names a schema outside its list.
    fn check_allowed(
        &self,
        tool: &str,
        data_source: &DataSource,
        planned: &[Planned<'_>],
    ) -> Result<(), ToolError> {
        let changes = self.inner.project.changes;
        let several = planned.len() > 1;
        for (index, statement) in planned.iter().enumerate() {
            let which = if several {
                format!("statement {} ", index + 1)
            } else {
                "this statement ".to_owned()
            };
            match statement.needs() {
                Err(reason) => return Err(self.refuse(tool, data_source, reason.to_owned())),
                Ok(Some(permission)) if !changes.allows(permission) => {
                    let reason = format!(
                        "{which}{}, which needs the project's {} permission",
                        statement.describe(),
                        permission.name()
                    );
                    return Err(self.refuse_for_permission(tool, data_source, permission, reason));
                }
                Ok(_) => {}
            }
            if let Some(Err(reason)) = self
                .scope(data_source)
                .map(|scope| scope.check(&statement.schemas()))
            {
                return Err(self.refuse(tool, data_source, format!("{which}is refused: {reason}")));
            }
        }
        Ok(())
    }

    fn refuse_for_permission(
        &self,
        tool: &str,
        data_source: &DataSource,
        permission: Permission,
        reason: String,
    ) -> ToolError {
        self.inner.audit.refused_for_permission(
            tool,
            &data_source.name,
            permission.name(),
            &reason,
        );
        ToolError::refused(reason)
    }

    /// The error for statement `failed` (counting from zero), with its position made
    /// relative to the text the client sent.
    fn statement_failure(
        &self,
        tool: &str,
        data_source: &DataSource,
        planned: &[Planned<'_>],
        failed: usize,
        atomic: bool,
        error: RunError,
    ) -> ToolError {
        let mut error = self.failure(tool, data_source, error, 0);
        let Some(statement) = planned.get(failed) else {
            return error;
        };
        error.position = error
            .position
            .map(|position| position + statement.range.start);
        let undone = if atomic {
            "nothing was committed"
        } else {
            "the statements before it that the database commits on its own (such as MySQL's \
             DDL) stay committed; the rest was rolled back"
        };
        error.message = if planned.len() > 1 {
            format!(
                "statement {} of {} failed, and {undone}: {}",
                failed + 1,
                planned.len(),
                error.message
            )
        } else {
            format!("{undone}: {}", error.message)
        };
        error
    }
}

/// Splits and classifies the statements, in order.
fn plan(sql: &str, language: QueryLanguage) -> Vec<Planned<'_>> {
    language
        .split(sql)
        .into_iter()
        .map(|range| {
            let text = &sql[range.clone()];
            Planned {
                text,
                range,
                analysis: language.analyze(text).map_err(|error| error.to_string()),
            }
        })
        .collect()
}

fn results_json(
    planned: &[Planned<'_>],
    results: &[Rows],
    atomic: bool,
    elapsed: std::time::Duration,
) -> Value {
    let statements: Vec<Value> = planned
        .iter()
        .zip(results)
        .map(|(statement, rows)| {
            let kind = statement
                .analysis
                .as_ref()
                .map_or("unknown", |analysis| analysis.kind.description());
            let mut value = json!({
                "kind": kind,
                "rows_affected": rows.row_count,
            });
            if !rows.columns.is_empty()
                && let Value::Object(object) = &mut value
            {
                let columns: Vec<Value> = rows
                    .columns
                    .iter()
                    .map(|column| json!({ "name": column.name, "type": column.type_name }))
                    .collect();
                object.insert("columns".to_owned(), Value::from(columns));
                object.insert("rows".to_owned(), json!(rows.rows));
                object.insert("truncated".to_owned(), Value::from(rows.truncated));
            }
            value
        })
        .collect();
    json!({
        "committed": true,
        "atomic": atomic,
        "statements": statements,
        "elapsed_ms": elapsed.as_millis(),
    })
}

#[cfg(test)]
mod tests {
    use dbrover_sql::Dialect;

    use super::*;

    #[test]
    fn plans_each_statement_with_its_place_in_the_text() {
        let sql = "INSERT INTO t VALUES (1);\n  CREATE TABLE u (id int);  SELEC 1";

        let planned = plan(sql, QueryLanguage::Sql(Dialect::Postgres));

        let texts: Vec<&str> = planned.iter().map(|statement| statement.text).collect();
        assert_eq!(
            texts,
            [
                "INSERT INTO t VALUES (1)",
                "CREATE TABLE u (id int)",
                "SELEC 1"
            ]
        );
        assert_eq!(&sql[planned[1].range.clone()], "CREATE TABLE u (id int)");
        assert_eq!(planned[0].needs(), Ok(Some(Permission::Write)));
        assert_eq!(planned[1].needs(), Ok(Some(Permission::Schema)));
        assert_eq!(
            planned[2].needs(),
            Ok(Some(Permission::Admin)),
            "what can't be read needs admin"
        );
        assert_eq!(planned[2].schemas(), NamedSchemas::Unknown);
    }

    #[test]
    fn never_runs_transaction_control() {
        for sql in ["BEGIN", "COMMIT", "ROLLBACK", "SAVEPOINT a"] {
            let planned = plan(sql, QueryLanguage::Sql(Dialect::Postgres));
            assert!(planned[0].needs().is_err(), "{sql}");
        }
        assert_eq!(
            plan("SELECT 1", QueryLanguage::Sql(Dialect::Mysql))[0].needs(),
            Ok(None)
        );
        assert!(plan("  ;  -- nothing\n", QueryLanguage::Sql(Dialect::Sqlite)).is_empty());
    }

    /// What an MCP client's commands in a Redis or MongoDB data source will go through.
    #[test]
    fn plans_each_language_by_its_own_classification() {
        let script = "get a\nset b 1\n# a note\nflush\nshutdown now";
        let needs: Vec<_> = plan(script, QueryLanguage::Test)
            .iter()
            .map(|planned| (planned.text, planned.needs()))
            .collect();
        assert_eq!(
            needs,
            [
                ("get a", Ok(None)),
                ("set b 1", Ok(Some(Permission::Write))),
                ("flush", Ok(Some(Permission::Admin))),
                ("shutdown now", Ok(Some(Permission::Admin))),
            ]
        );
        assert!(
            plan("begin", QueryLanguage::Test)[0].needs().is_err(),
            "transaction control is refused"
        );
    }
}
