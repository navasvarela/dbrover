//! Explaining a report without running it (`docs/reports-at-scale.md`, "Each source's plan and
//! estimated cost", and RS2d):
//!
//! 1. each source's statement, with the run's parameters, is explained by its engine through a
//!    [`SourceExplainer`], with what the data source's cost guard would say of it; a source
//!    with a lookup is explained with a list of one `NULL` in place of its values, as a run
//!    whose lookup finds none sends;
//! 2. the combine query is planned by DuckDB over empty tables of each source's columns: the
//!    ones the file declares, else those the last run loaded ([`KnownTable`]); attached DuckDB
//!    files are attached as a run attaches them.
//!
//! Nothing here reads a row: an engine's `EXPLAIN` plans, and the combine's tables are empty.
//! The combine's plan says what it reads of each table (its shape), not what it would cost: an
//! empty table has no rows to estimate from.

use std::path::Path;
use std::time::Instant;

use dbrover_core::describe_error;
use dbrover_driver_duckdb::{DuckDbType, Workspace, WorkspaceLimits, WorkspaceStore};
use dbrover_language::Literal;
use dbrover_sql::Dialect;
use serde::{Deserialize, Serialize};

use crate::events::LoadedColumn;
use crate::statement::Statements;
use crate::typing::declared_column_type;
use crate::{Combined, Logic, Plan, PlannedSource, QuerySource, RunValues, SourceStatement};

/// What explaining a report found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Explanation {
    /// As the file lists the sources with a query; attached files have nothing to explain.
    pub sources: Vec<SourceExplanation>,
    /// `None` for a report without a combine step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combine: Option<CombineExplanation>,
}

/// One source's statement, as its engine would run it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceExplanation {
    pub id: String,
    /// Its data source's name.
    pub data_source: String,
    #[serde(flatten)]
    pub explained: SourceExplained,
    /// Anything the reader should know about how it was explained, a sentence each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// What a [`SourceExplainer`] says of one statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceExplained {
    /// The statement explained, with its parameters' values written in.
    pub statement: String,
    pub plan: ExplainedPlan,
    /// What the data source's cost guard estimates of it, where its engine estimates costs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<Estimate>,
}

/// A statement's plan, as its engine wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExplainedPlan {
    /// A plan DbRover draws as a tree: its format, by `dbrover_plan::PlanFormat::as_str`, and
    /// the text the engine wrote.
    Drawn { format: String, text: String },
    /// A plan as the engine writes it, as text, without costs to weigh.
    Text { text: String },
    /// Not explained, and why: the engine has no `EXPLAIN`, or explaining would run the
    /// statement.
    Unexplained { reason: String },
    /// The engine refused to explain the statement, or couldn't be reached.
    Failed { message: String },
}

/// What a statement would cost on an engine that bills each read, and what the data source's
/// guard would do with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Estimate {
    /// What the estimate says, as the guard's question says it.
    pub says: String,
    /// The bytes it would scan, when the engine estimates bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Whether it may scan more than `bytes`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub at_least: bool,
    /// The data source's limit, above which it asks first (`ask_above_bytes`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_bytes: Option<u64>,
    /// Whether a run would refuse the statement: a run has nobody to ask.
    pub refused: bool,
}

/// The combine query's plan over empty tables, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CombineExplanation {
    /// DuckDB's plan, as `EXPLAIN (FORMAT JSON)` writes it.
    Planned { text: String },
    /// A source's columns aren't known: the file doesn't declare them, and no run has loaded
    /// it yet.
    Unknown { reason: String },
    /// DuckDB couldn't plan the query, as when it names a column no source has.
    Failed { message: String },
}

/// A source's table as the last finished run loaded it, which the app's history keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownTable {
    pub id: String,
    pub columns: Vec<LoadedColumn>,
}

/// Explains a report's sources: the engine on real data sources, tests in memory.
pub trait SourceExplainer {
    /// Explains the statement on its data source, without running it.
    fn explain(&self, statement: &SourceStatement<'_>) -> impl Future<Output = SourceExplained>;
}

/// Explains `plan` with `values`: see the module. `known` holds the tables the last run
/// loaded, and `spill` is a folder DuckDB may use, which the caller removes.
pub async fn explain(
    plan: &Plan,
    values: &RunValues,
    explainer: &impl SourceExplainer,
    known: &[KnownTable],
    spill: &Path,
) -> Explanation {
    match &plan.logic {
        Logic::Direct(source) => Explanation {
            sources: vec![explain_source(plan, values, explainer, source).await],
            combine: None,
        },
        Logic::Combine(combined) => {
            let mut sources = Vec::new();
            for source in &combined.sources {
                if let PlannedSource::Query(source) = source {
                    sources.push(explain_source(plan, values, explainer, source).await);
                }
            }
            let combine = explain_combine(combined, values, known, spill).await;
            Explanation {
                sources,
                combine: Some(combine),
            }
        }
    }
}

async fn explain_source(
    plan: &Plan,
    values: &RunValues,
    explainer: &impl SourceExplainer,
    source: &QuerySource,
) -> SourceExplanation {
    let statements = Statements::new(plan, values, source);
    let now = Instant::now();
    let (statement, notes) = match &source.lookup {
        None => (statements.whole(now), Vec::new()),
        Some(lookup) => (
            statements.batch(&[Literal::Null], now + source.limits.timeout),
            vec![format!(
                "Its lookup's values come from {:?} as the report runs, so it was explained \
                 with a list of one NULL in their place.",
                lookup.from
            )],
        ),
    };
    SourceExplanation {
        id: source.id.clone(),
        data_source: source.data_source.name.clone(),
        explained: explainer.explain(&statement).await,
        notes,
    }
}

/// The combine query planned over empty tables of every source's columns.
async fn explain_combine(
    combined: &Combined,
    values: &RunValues,
    known: &[KnownTable],
    spill: &Path,
) -> CombineExplanation {
    let mut tables = Vec::new();
    for source in &combined.sources {
        let PlannedSource::Query(source) = source else {
            continue;
        };
        match table_of(source, known) {
            Some(columns) => tables.push((source.id.as_str(), columns)),
            None => {
                return CombineExplanation::Unknown {
                    reason: format!(
                        "Source {} declares no columns and hasn't been loaded by a finished run \
                         yet: run the report once, or declare its columns, to explain the \
                         combine step.",
                        source.id
                    ),
                };
            }
        }
    }
    let limits = &combined.combine.limits;
    // Its tables are empty, so the database stays in memory.
    let opened = Workspace::open(
        &WorkspaceStore::Memory,
        &WorkspaceLimits {
            memory_limit: limits.memory_limit.0,
            threads: limits.threads(),
            temp_directory: spill.to_path_buf(),
            max_temp_size: limits.max_temp_size.0,
        },
    )
    .await;
    let mut workspace = match opened {
        Ok(workspace) => workspace,
        Err(error) => {
            return CombineExplanation::Failed {
                message: describe_error(&error),
            };
        }
    };
    let planned = plan_combine(&mut workspace, combined, values, &tables).await;
    workspace.close().await;
    match planned {
        Ok(text) => CombineExplanation::Planned { text },
        Err(message) => CombineExplanation::Failed { message },
    }
}

/// Makes the empty tables, attaches the files, locks the workspace as a run does, and plans
/// the query.
async fn plan_combine(
    workspace: &mut Workspace,
    combined: &Combined,
    values: &RunValues,
    tables: &[(&str, Vec<(String, DuckDbType)>)],
) -> Result<String, String> {
    for source in &combined.sources {
        if let PlannedSource::Attached(attached) = source {
            workspace
                .attach(&attached.id, &attached.path)
                .await
                .map_err(|error| describe_error(&error))?;
        }
    }
    for (id, columns) in tables {
        let names: Vec<String> = columns.iter().map(|(name, _)| name.clone()).collect();
        let types: Vec<DuckDbType> = columns
            .iter()
            .map(|(_, column_type)| *column_type)
            .collect();
        let loader = workspace
            .loader(id, &names)
            .await
            .map_err(|error| describe_error(&error))?;
        loader
            .finish(&types)
            .await
            .map_err(|error| describe_error(&error))?;
    }
    workspace
        .plan_without_statistics()
        .await
        .map_err(|error| describe_error(&error))?;
    workspace
        .lock()
        .await
        .map_err(|error| describe_error(&error))?;
    let prepared = dbrover_sql::bind(
        combined.combine.query.sql(),
        Dialect::DuckDb,
        &values.parameters,
    )
    .map_err(|missing| missing.to_string())?;
    workspace
        .explain(&prepared.sql, &prepared.values)
        .await
        .map_err(|error| describe_error(&error))
}

/// The columns of `source`'s table: declared in the file, else as the last run loaded them;
/// `None` when neither says.
fn table_of(source: &QuerySource, known: &[KnownTable]) -> Option<Vec<(String, DuckDbType)>> {
    if let Some(declared) = &source.columns {
        return Some(
            declared
                .iter()
                .map(|column| {
                    (
                        column.name.clone(),
                        declared_column_type(column.column_type),
                    )
                })
                .collect(),
        );
    }
    let table = known
        .iter()
        .find(|table| table.id.eq_ignore_ascii_case(&source.id))?;
    table
        .columns
        .iter()
        .map(|column| {
            let column_type = column.type_name.parse().ok()?;
            Some((column.name.clone(), column_type))
        })
        .collect()
}
