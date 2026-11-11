//! The statements a source sends (`docs/reporting.md`, "Lookups"): its query with a lookup's
//! values written in as a list of its language, and, in a language without SQL, the report's
//! parameters written in as literals of their types. A SQL source's parameters are left for
//! its driver to bind, as RP1's are.

use std::time::Instant;

use dbrover_driver_duckdb::DuckDbType;
use dbrover_export::ColumnType;
use dbrover_language::Literal;
use dbrover_sql::ParameterValues;

use crate::{
    CostCheck, PART_FROM, PART_TO, PartRange, Plan, QuerySource, RangeKind, RunValues,
    SourceStatement, WholePeriod,
};

/// What a source's statements are made from.
pub(crate) struct Statements<'a> {
    plan: &'a Plan,
    values: &'a RunValues,
    source: &'a QuerySource,
}

impl<'a> Statements<'a> {
    pub(crate) fn new(plan: &'a Plan, values: &'a RunValues, source: &'a QuerySource) -> Self {
        Self {
            plan,
            values,
            source,
        }
    }

    /// The source's one statement, when it has no lookup. Its timeout runs from `started`.
    pub(crate) fn whole(&self, started: Instant) -> SourceStatement<'a> {
        self.statement(&Extra::None, started + self.source.limits.timeout)
    }

    /// The statement that reads `batch` of the source's lookup values, by `deadline`.
    pub(crate) fn batch(&self, batch: &[Literal], deadline: Instant) -> SourceStatement<'a> {
        self.statement(&Extra::Batch(batch), deadline)
    }

    /// The statement that reads `range` of the source's partition, of `kind`, under the
    /// source's timeout from `started`; `cost` is what the cost guard judges before it.
    pub(crate) fn range(
        &self,
        range: &PartRange,
        kind: RangeKind,
        started: Instant,
        cost: CostCheck,
    ) -> SourceStatement<'a> {
        let statement = self.statement(
            &Extra::Range { range, kind },
            started + self.source.limits.timeout,
        );
        SourceStatement { cost, ..statement }
    }

    /// What the cost guard judges for the first of a partitioned source's ranges: the source's
    /// statement over `whole`, its whole period.
    pub(crate) fn whole_period(&self, whole: &PartRange, kind: RangeKind) -> CostCheck {
        let statement = self.statement(&Extra::Range { range: whole, kind }, Instant::now());
        CostCheck::WholePeriod(Box::new(WholePeriod {
            query: statement.query,
            parameters: statement.parameters,
        }))
    }

    fn statement(&self, extra: &Extra<'_>, deadline: Instant) -> SourceStatement<'a> {
        let language = self.source.language;
        let lookup = self
            .source
            .split
            .lookup()
            .map(|lookup| lookup.parameter.as_str());
        let list = match extra {
            Extra::Batch(values) => Some(language.list(values)),
            Extra::None | Extra::Range { .. } => None,
        };
        let bound = language.dialect().is_some();
        let query = language.replace_parameters(&self.source.query, |name| {
            if Some(name) == lookup {
                return list.clone();
            }
            if bound {
                return None;
            }
            extra
                .range_literal(name)
                .or_else(|| self.parameter(name))
                .map(|literal| language.literal(&literal))
        });
        let parameters = if bound {
            let mut parameters = self.values.parameters.clone();
            if let Extra::Range { range, .. } = extra {
                parameters.set(PART_FROM, Some(range.from.clone()));
                parameters.set(PART_TO, Some(range.to.clone()));
            }
            parameters
        } else {
            ParameterValues::default()
        };
        SourceStatement {
            source: self.source,
            query,
            parameters,
            deadline,
            cost: CostCheck::Statement,
        }
    }

    /// The report's parameter `name` as a literal of its type.
    fn parameter(&self, name: &str) -> Option<Literal> {
        let param = self.plan.params.iter().find(|param| param.name == name)?;
        let value = self.values.parameters.get(name)?;
        let Some(text) = value.clone() else {
            return Some(Literal::Null);
        };
        Some(match param.param_type {
            ColumnType::Text => Literal::Text(text),
            ColumnType::Integer | ColumnType::Decimal(_) => Literal::Number(text),
            ColumnType::Boolean => Literal::Boolean(text == "true"),
            ColumnType::Date => Literal::Date(text),
            ColumnType::Timestamp => Literal::Timestamp(text),
        })
    }
}

/// What a statement binds beside the report's parameters.
enum Extra<'a> {
    None,
    /// A batch of a lookup's values, written in as a list.
    Batch(&'a [Literal]),
    /// A range of a partition, bound as `:part_from` and `:part_to`.
    Range {
        range: &'a PartRange,
        kind: RangeKind,
    },
}

impl Extra<'_> {
    /// `:part_from` or `:part_to` as a literal, for a language without bound parameters.
    fn range_literal(&self, name: &str) -> Option<Literal> {
        let Self::Range { range, kind } = self else {
            return None;
        };
        match name {
            PART_FROM => Some(kind.literal(&range.from)),
            PART_TO => Some(kind.literal(&range.to)),
            _ => None,
        }
    }
}

/// A lookup's value, read from a column of `column_type` as text, as a literal: a number
/// unquoted, a date or time as one, anything else as text.
pub(crate) fn lookup_literal(column_type: DuckDbType, text: String) -> Literal {
    match column_type {
        DuckDbType::BigInt
        | DuckDbType::HugeInt
        | DuckDbType::Decimal { .. }
        | DuckDbType::Double => Literal::Number(text),
        DuckDbType::Boolean => Literal::Boolean(text == "true"),
        DuckDbType::Date => Literal::Date(text),
        DuckDbType::Timestamp => Literal::Timestamp(text),
        // DuckDB writes its offset (`+00`), which no language's time literal reads.
        DuckDbType::TimestampTz | DuckDbType::Varchar => Literal::Text(text),
    }
}
