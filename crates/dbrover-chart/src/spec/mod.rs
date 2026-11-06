//! A chart written down: a few TOML keys under `[chart]`, as a block above a statement holds
//! them (`docs/charts-as-code.md`, CH1).
//!
//! A spec is checked twice. [`ChartSpec::read`] checks its keys and their values, with each
//! problem a sentence placed at its line; [`ChartSpec::resolve`] checks the columns it names
//! against a result's, once they are known. [`write_back`] writes the toolbar's choice into a
//! spec's text, keeping everything else in it.

mod arrange;
mod read;
mod resolve;
mod write;

use std::fmt;
use std::ops::Range;

pub use arrange::arrange;
pub use resolve::{ColumnProblem, Resolved, SortBy};
pub use write::{SavedChoice, WriteError, write_back};

use crate::ChartKind;

/// The table a spec's keys are under.
pub const SPEC_TABLE: &str = "chart";

/// A chart as its spec writes it. Every key is optional: one left out takes the value the chart
/// tab would start with, given the keys that are present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChartSpec {
    pub kind: Option<ChartKind>,
    pub x: Option<String>,
    /// The Y columns' names; empty when the spec leaves `y` out.
    pub y: Vec<String>,
    pub title: Option<String>,
    pub x_title: Option<String>,
    pub y_title: Option<String>,
    /// Start the Y axis at zero for lines and scatters; bars always do.
    pub y_zero: bool,
    pub sort: Option<Sort>,
    /// How many rows to chart, after `sort`.
    pub limit: Option<usize>,
}

/// A column to sort the rows by before charting them, and which way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sort {
    pub column: String,
    pub descending: bool,
}

/// A problem with a spec's text: TOML that doesn't read, a key that doesn't exist, or a value
/// of the wrong kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecProblem {
    /// The bytes of the spec's text the problem is about, to underline.
    pub span: Range<usize>,
    /// The line of the spec's text it is on, counted from 1.
    pub line: usize,
    /// What is wrong, as a sentence without the chart and line it is at.
    pub detail: String,
}

impl SpecProblem {
    fn new(text: &str, span: Range<usize>, detail: impl Into<String>) -> Self {
        let start = span.start.min(text.len());
        Self {
            line: text[..start].matches('\n').count() + 1,
            span,
            detail: detail.into(),
        }
    }

    /// The problem as a spec starting on line `first_line` of a longer text says it: a block
    /// in a console says the console's line.
    pub fn moved_to(mut self, first_line: usize) -> Self {
        self.line += first_line.saturating_sub(1);
        self
    }
}

impl fmt::Display for SpecProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chart, line {}: {}", self.line, self.detail)
    }
}

/// The keys a spec reads, in the order a new spec writes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Kind,
    X,
    Y,
    Title,
    XTitle,
    YTitle,
    YZero,
    Sort,
    Limit,
}

impl Key {
    const ALL: [Self; 9] = [
        Self::Kind,
        Self::X,
        Self::Y,
        Self::Title,
        Self::XTitle,
        Self::YTitle,
        Self::YZero,
        Self::Sort,
        Self::Limit,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Kind => "kind",
            Self::X => "x",
            Self::Y => "y",
            Self::Title => "title",
            Self::XTitle => "x_title",
            Self::YTitle => "y_title",
            Self::YZero => "y_zero",
            Self::Sort => "sort",
            Self::Limit => "limit",
        }
    }

    fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.name() == name)
    }
}
