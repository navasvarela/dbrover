//! What the engine is told and tells about a data search (W16, `docs/data-search.md`).
//!
//! A search is planned, then run: [`crate::Command::PlanDataSearch`] lists the scope's tables
//! and writes each one's statement, answering with a [`SearchPlan`] that says whether to ask
//! first; [`crate::Command::RunDataSearch`] then runs the plan a table at a time.

use std::time::Duration;

use dbrover_core::{DataSearch, Row, RowsPerTable, SearchableTable, TablesToSearch, TimeLimit};
use dbrover_sql::SearchStatement;

/// What to search for, where, and under which limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    pub search: DataSearch,
    pub tables: TablesToSearch,
    pub limits: SearchLimits,
}

/// How long one table may take, and how many of its rows are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchLimits {
    /// `None` for no limit, as **Retry** asks for once the user has said yes.
    pub per_table: Option<TimeLimit>,
    pub rows: RowsPerTable,
}

/// 15 s: long enough for an indexed or small table on a busy server, short enough that a
/// table read in full from disk is left for a **Retry** rather than holding up the rest.
/// Settings › Data search changes it.
pub const TABLE_TIME_LIMIT: TimeLimit = match TimeLimit::from_seconds(15) {
    Some(limit) => limit,
    None => panic!("the default time limit must not be zero"),
};

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            per_table: Some(TABLE_TIME_LIMIT),
            rows: RowsPerTable::DEFAULT,
        }
    }
}

/// The tables a search will read, in the order it reads them, each with its statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPlan {
    pub tables: Vec<PlannedTable>,
    /// What to ask before running it, if anything.
    pub question: Option<SearchQuestion>,
}

impl SearchPlan {
    /// The tables with a statement: those with a column that can hold the value.
    pub fn searched(&self) -> usize {
        self.tables
            .iter()
            .filter(|table| table.statement.is_some())
            .count()
    }
}

/// A table of the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedTable {
    pub table: SearchableTable,
    /// `None` when none of its columns can hold the value: it isn't read.
    pub statement: Option<SearchStatement>,
}

/// Why a search asks before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchQuestion {
    /// The data source is careful, and the search reads more than one table.
    Careful { tables: usize },
}

/// How one table's statement ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableOutcome {
    /// Rows that hold the value, in the statement's select list's order
    /// ([`SearchStatement::selected`]); `more` when there were more than the rows per table.
    Matched {
        rows: Vec<Row>,
        more: bool,
    },
    NoMatch,
    /// Stopped at the time limit.
    TimedOut(TimeLimit),
    /// The server refused or failed it, with its message.
    Failed(String),
}

/// A table's outcome, and how long it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSearched {
    /// Its place in [`SearchPlan::tables`].
    pub index: usize,
    pub outcome: TableOutcome,
    pub took: Duration,
}

/// How a search ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchEnd {
    /// Every table was read, or given up on at its time limit.
    Finished,
    /// Cancel was pressed; the tables not yet read weren't.
    Cancelled,
    /// The search couldn't go on: the connection was lost, or there was no plan to run.
    Failed(String),
}
