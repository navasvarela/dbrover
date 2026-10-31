//! Query plans, the same shape whatever the database: a tree of [`Step`]s, each with what it
//! does, the database's estimate and, once the statement has run under `EXPLAIN ANALYZE`, what
//! actually happened.
//!
//! Each engine answers `EXPLAIN` in a format of its own; a [`PlanFormat`] reads one of them into
//! a [`Plan`]. The plan then weighs its steps ([`Plan::outline`]), to show which ones cost the
//! most, and writes itself as text ([`Plan::text`]), so whoever shows a plan only draws it.
//! Nothing here talks to a database.

mod postgres;
mod text;
mod weigh;

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
    /// [`Estimate::rows`].
    pub rows: f64,
    /// How many times it ran; 0 when the plan never needed it.
    pub loops: f64,
    /// How long each run took on average, when the database timed it.
    pub time: Option<Timing>,
}

/// When a step's first and last rows were ready, in milliseconds from when it started, its
/// children's time included.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    pub first_row_ms: f64,
    pub last_row_ms: f64,
}

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
        self.root.actual.is_some()
    }
}

impl Step {
    /// The operation and its target together, as one line of the text plan starts.
    pub fn title(&self) -> String {
        match &self.target {
            Some(target) => format!("{} {target}", self.operation),
            None => self.operation.clone(),
        }
    }

    /// How far the actual rows were from the estimate, as a factor of at least 1, when the step
    /// ran and both are known: 10 means ten times more, or ten times fewer, rows than expected.
    /// A step that returned no rows is compared as if it returned one, as is an estimate of none.
    pub fn misestimate(&self) -> Option<f64> {
        let actual = self.actual.filter(|actual| actual.loops > 0.0)?;
        let estimated = self.estimate.rows?.max(1.0);
        let returned = actual.rows.max(1.0);
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
        }
    }

    /// The name of the notation the plan is written in, for what copying it copies.
    pub fn notation(self) -> &'static str {
        match self {
            Self::PostgresJson => "JSON",
        }
    }
}

/// Why a plan couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("the plan isn't JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The text is in the right notation, but not shaped as the format's plans are.
    #[error("the plan isn't shaped as expected: {0}")]
    Shape(String),
}
