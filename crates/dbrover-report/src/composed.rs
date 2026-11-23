//! The reports a run loads into its one DuckDB: the report being run, and each report it uses
//! as a source (`docs/reports-at-scale.md`, RS9c). Both running a report and explaining one
//! walk them the same way, so the walk is here.

use dbrover_driver_duckdb::WorkspaceTable;

use crate::{Combined, Logic, Plan, RunFailure, RunValues, Step};

/// A report whose sources load into the run's one DuckDB: the report being run, whose tables
/// are in `main`, and each report it uses as a source, whose tables are in a schema of its own
/// (`docs/reports-at-scale.md`, RS9c).
pub(crate) struct Composed<'a> {
    /// The source ids from the report being run down to this one, which name both its schema
    /// and its steps; empty for the report being run.
    pub(crate) path: Vec<String>,
    /// Its own plan, whose parameters and limits its sources are read under.
    pub(crate) plan: &'a Plan,
    pub(crate) combined: &'a Combined,
    /// Its own parameters: the run's for the report being run, and each report source's
    /// bound values below it.
    pub(crate) values: RunValues,
    /// The table its combine writes, in the schema of the report that reads it; `None` for
    /// the report being run, whose rows go to the outputs.
    pub(crate) into: Option<WorkspaceTable>,
}

impl Composed<'_> {
    /// The schema its tables are in: the path joined with `-`, which no source id can hold,
    /// or `main` for the report being run. Not a dot: DuckDB reads a dot in `search_path` as
    /// the catalog a schema is in, so a schema named `sales.sales` could never be named there.
    pub(crate) fn schema(&self) -> Option<String> {
        (!self.path.is_empty()).then(|| self.path.join("-"))
    }

    /// Where the table of its source `id` is.
    pub(crate) fn table(&self, id: &str) -> WorkspaceTable {
        match self.schema() {
            Some(schema) => WorkspaceTable::in_schema(schema, id),
            None => WorkspaceTable::in_main(id),
        }
    }

    /// What the run calls the step of its source `id`: `valued › tx` for a source of a report
    /// used as a source, and `tx` for one of the report being run.
    pub(crate) fn step_id(&self, id: &str) -> String {
        if self.path.is_empty() {
            return id.to_owned();
        }
        format!("{} › {id}", self.path.join(" › "))
    }
}

/// The reports a run loads, the report being run first and each report it uses after the
/// report that names it, so reversing the list runs the combines innermost first (RS9c).
///
/// # Errors
///
/// [`RunFailure`] when a report source's parameters can't be bound, which planning the outer
/// report already refused: a run reaching one is a bug.
pub(crate) fn composed<'a>(
    plan: &'a Plan,
    values: &RunValues,
    combined: &'a Combined,
) -> Result<Vec<Composed<'a>>, RunFailure> {
    let mut reports = vec![Composed {
        path: Vec::new(),
        plan,
        combined,
        values: values.clone(),
        into: None,
    }];
    let mut at = 0;
    while at < reports.len() {
        let mut below = Vec::new();
        {
            let outer = &reports[at];
            for source in &outer.combined.sources {
                let Some(source) = source.report() else {
                    continue;
                };
                let Logic::Combine(combined) = &source.plan.logic else {
                    // A report of one source without a combine: its rows are its result, and
                    // its source loads straight into the table this one reads
                    // ([`Combining::load_report`]).
                    continue;
                };
                let values = source.values(&outer.values).map_err(|problems| {
                    RunFailure::new(
                        Step::Source {
                            id: outer.step_id(&source.id),
                        },
                        problems.join("\n"),
                    )
                })?;
                let mut path = outer.path.clone();
                path.push(source.id.clone());
                below.push(Composed {
                    path,
                    plan: &source.plan,
                    combined,
                    values,
                    into: Some(outer.table(&source.id)),
                });
            }
        }
        reports.extend(below);
        at += 1;
    }
    Ok(reports)
}
