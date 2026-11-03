//! The statements a source sends (`docs/reporting.md`, "Lookups"): its query with a lookup's
//! values written in as a list of its language, and, in a language without SQL, the report's
//! parameters written in as literals of their types. A SQL source's parameters are left for
//! its driver to bind, as RP1's are.

use std::time::Instant;

use dbrover_driver_duckdb::DuckDbType;
use dbrover_export::ColumnType;
use dbrover_language::Literal;
use dbrover_sql::ParameterValues;

use crate::{Plan, QuerySource, RunValues, SourceStatement};

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
        self.statement(None, started + self.source.limits.timeout)
    }

    /// The statement that reads `batch` of the source's lookup values, by `deadline`.
    pub(crate) fn batch(&self, batch: &[Literal], deadline: Instant) -> SourceStatement<'a> {
        self.statement(Some(batch), deadline)
    }

    fn statement(&self, batch: Option<&[Literal]>, deadline: Instant) -> SourceStatement<'a> {
        let language = self.source.language;
        let lookup = self
            .source
            .lookup
            .as_ref()
            .map(|lookup| lookup.parameter.as_str());
        let list = batch.map(|values| language.list(values));
        let bound = language.dialect().is_some();
        let query = language.replace_parameters(&self.source.query, |name| {
            if Some(name) == lookup {
                return list.clone();
            }
            if bound {
                return None;
            }
            self.parameter(name)
                .map(|literal| language.literal(&literal))
        });
        SourceStatement {
            source: self.source,
            query,
            parameters: if bound {
                self.values.parameters.clone()
            } else {
                ParameterValues::default()
            },
            deadline,
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
