//! Query plans, the same shape whatever the database: a tree of [`Step`]s, each with what it
//! does, the database's estimate and, once the statement has run under `EXPLAIN ANALYZE`, what
//! actually happened.
//!
//! Each engine answers `EXPLAIN` in a format of its own; a [`PlanFormat`] reads one of them into
//! a [`Plan`]. The plan then weighs its steps ([`Plan::outline`]), to show which ones cost the
//! most, and writes itself as text ([`Plan::text`]), so whoever shows a plan only draws it.
//! Nothing here talks to a database.

mod json;
mod mysql;
mod postgres;
mod sqlserver;
mod text;
mod weigh;

pub use text::format_rows;
pub use weigh::{Heat, Line, Measure, Weight};

/// A statement's plan, as one database explained it.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub root: Step,
    /// Facts about the whole plan rather than one step, in the database's words: the time
    /// spent planning and running it, its triggers.
    pub details: Vec<Detail>,
}

/// One step of a plan: an operation that produces rows for the step above it from the rows of
/// the steps below it.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// What the step does, as the database names it: `Hash Join`, `Seq Scan`,
    /// `Clustered Index Seek`.
    pub operation: String,
    /// What it works on, in the words that follow the operation when the database writes its
    /// plan as text: `on orders o`, `using customers_pkey on customers c`.
    pub target: Option<String>,
    /// The name the database gives the subtree this step starts, when it is a part of the
    /// statement planned on its own: `CTE recent`, `SubPlan 2`.
    pub heading: Option<String>,
    pub estimate: Estimate,
    /// What happened when the statement ran, if it was explained by running it.
    pub actual: Option<Actual>,
    /// Everything else the database says about the step, in its words and order: the join
    /// condition, the filter, the sort key.
    pub details: Vec<Detail>,
    pub children: Vec<Step>,
}

/// What the database expected of a step before running it. Costs are in the database's own
/// units, which only compare within one plan.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Estimate {
    /// The cost of producing the step's first row, where the database estimates it.
    pub startup_cost: Option<f64>,
    /// The cost of producing all its rows, its children's included.
    pub total_cost: Option<f64>,
    /// The rows it returns each time it runs.
    pub rows: Option<f64>,
}

/// What a step did when the statement ran.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Actual {
    /// The rows it returned each time it ran, on average: comparable with
    /// [`Estimate::rows`]. `None` where the database times a step without counting its rows
    /// (MariaDB's query blocks).
    pub rows: Option<f64>,
    /// How many times it ran; 0 when the plan never needed it.
    pub loops: f64,
    /// How long each run took on average, when the database timed it.
    pub time: Option<Timing>,
}

/// When a step's first and last rows were ready, in milliseconds from when it started, its
/// children's time included.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    /// `None` where the database only says how long the step took in all (MariaDB).
    pub first_row_ms: Option<f64>,
    pub last_row_ms: f64,
}

impl Actual {
    /// How long the step took over every time it ran, its children's time included, when the
    /// database timed it.
    pub fn total_ms(&self) -> Option<f64> {
        self.time.map(|time| time.last_row_ms * self.loops)
    }
}

/// How far a step's actual rows must be from its estimate for the estimate to be called wrong:
/// ten times more or fewer. The planner chooses joins and scans by row counts, and a factor of
/// ten is where the usual advice says its choice is likely to be off; smaller misses are
/// everyday noise.
pub const MISESTIMATE_FACTOR: f64 = 10.0;

/// A fact about a step or a plan, labelled as the database labels it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub label: String,
    pub value: String,
}

impl Detail {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

impl Plan {
    /// Whether the statement ran to produce this plan, so its steps have actual figures.
    pub fn is_analysed(&self) -> bool {
        self.root.any(&|step| step.actual.is_some())
    }
}

impl Step {
    /// Whether the step, or any step under it, is one `matches` accepts.
    pub fn any(&self, matches: &dyn Fn(&Step) -> bool) -> bool {
        matches(self) || self.children.iter().any(|child| child.any(matches))
    }

    /// The operation and its target together, as one line of the text plan starts.
    pub fn title(&self) -> String {
        match &self.target {
            Some(target) => format!("{} {target}", self.operation),
            None => self.operation.clone(),
        }
    }

    /// Whether the actual rows were [`MISESTIMATE_FACTOR`] times or more off the estimate.
    pub fn is_misestimated(&self) -> bool {
        self.misestimate()
            .is_some_and(|factor| factor >= MISESTIMATE_FACTOR)
    }

    /// How far the actual rows were from the estimate, as a factor of at least 1, when the step
    /// ran and both are known: 10 means ten times more, or ten times fewer, rows than expected.
    /// A step that returned no rows is compared as if it returned one, as is an estimate of none.
    pub fn misestimate(&self) -> Option<f64> {
        let actual = self.actual.filter(|actual| actual.loops > 0.0)?;
        let estimated = self.estimate.rows?.max(1.0);
        let returned = actual.rows?.max(1.0);
        Some(if returned > estimated {
            returned / estimated
        } else {
            estimated / returned
        })
    }
}

/// A plan format some database answers `EXPLAIN` in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlanFormat {
    /// PostgreSQL's `EXPLAIN (FORMAT JSON)`, with or without `ANALYZE`.
    PostgresJson,
    /// MySQL's `EXPLAIN FORMAT=JSON`, in either of its formats, with or without `ANALYZE`, and
    /// MariaDB's `EXPLAIN FORMAT=JSON` and `ANALYZE FORMAT=JSON`.
    MysqlJson,
    /// SQL Server's XML showplan, estimated (`SET SHOWPLAN_XML`) or actual
    /// (`SET STATISTICS XML`).
    SqlServerXml,
}

impl PlanFormat {
    /// Reads a plan the database wrote in this format.
    ///
    /// # Errors
    ///
    /// [`PlanError`] if `text` isn't in this format, or holds no plan.
    pub fn parse(self, text: &str) -> Result<Plan, PlanError> {
        match self {
            Self::PostgresJson => postgres::parse(text),
            Self::MysqlJson => mysql::parse(text),
            Self::SqlServerXml => sqlserver::parse(text),
        }
    }

    /// The name of the notation the plan is written in, for what copying it copies.
    pub fn notation(self) -> &'static str {
        match self {
            Self::PostgresJson | Self::MysqlJson => "JSON",
            Self::SqlServerXml => "XML",
        }
    }
}

/// Why a plan couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("the plan isn't JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the plan isn't XML: {0}")]
    Xml(#[from] roxmltree::Error),
    /// The text is in the right notation, but not shaped as the format's plans are.
    #[error("the plan isn't shaped as expected: {0}")]
    Shape(String),
}
