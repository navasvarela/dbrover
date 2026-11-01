//! What each column can be on a chart: numbers to measure, times to place in order, or text to
//! name categories.

use dbrover_core::{Cell, ValueKind};

use crate::values;

/// A result column, as a chart sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// What its declared type holds; [`ValueKind::Undeclared`] when the driver reports none.
    pub kind: ValueKind,
}

/// What a column's values are, for placing them on an axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Number,
    Time,
    Text,
}

impl Role {
    /// The role of a column of `kind`. A column that declares no type takes it from its values
    /// instead: numbers if every value that isn't null reads as one, times if every one reads
    /// as a date or timestamp, text otherwise, and text when every value is null.
    pub fn of<'a>(kind: ValueKind, values: impl IntoIterator<Item = &'a Cell>) -> Self {
        match kind {
            ValueKind::Integer | ValueKind::Decimal | ValueKind::Float => Self::Number,
            ValueKind::Date | ValueKind::Timestamp | ValueKind::TimestampWithZone => Self::Time,
            ValueKind::Boolean | ValueKind::Text => Self::Text,
            ValueKind::Undeclared => Self::from_values(values),
        }
    }

    fn from_values<'a>(values: impl IntoIterator<Item = &'a Cell>) -> Self {
        let mut texts = values.into_iter().flatten().peekable();
        if texts.peek().is_none() {
            return Self::Text;
        }
        let mut numbers = true;
        let mut times = true;
        for text in texts {
            numbers &= values::undeclared_number(text).is_some();
            times &= values::time(text).is_some();
            if !numbers && !times {
                return Self::Text;
            }
        }
        if numbers { Self::Number } else { Self::Time }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(values: &[Option<&str>]) -> Vec<Cell> {
        values
            .iter()
            .map(|value| value.map(str::to_owned))
            .collect()
    }

    #[test]
    fn a_declared_type_decides_the_role() {
        let anything = cells(&[Some("not a number")]);
        let cases = [
            (ValueKind::Integer, Role::Number),
            (ValueKind::Decimal, Role::Number),
            (ValueKind::Float, Role::Number),
            (ValueKind::Date, Role::Time),
            (ValueKind::Timestamp, Role::Time),
            (ValueKind::TimestampWithZone, Role::Time),
            (ValueKind::Boolean, Role::Text),
            (ValueKind::Text, Role::Text),
        ];
        for (kind, role) in cases {
            assert_eq!(Role::of(kind, &anything), role, "{kind:?}");
        }
    }

    #[test]
    fn an_undeclared_column_takes_its_role_from_its_values() {
        let cases: [(&[Option<&str>], Role); 8] = [
            (&[Some("1"), None, Some("2.5")], Role::Number),
            (
                &[Some("2026-01-01"), Some("2026-02-01 00:00:00+00")],
                Role::Time,
            ),
            (&[Some("2026-01"), Some("2026-02")], Role::Text),
            (&[Some("1"), Some("two")], Role::Text),
            (&[Some("00501"), Some("10001")], Role::Text),
            (&[Some("t"), Some("f")], Role::Text),
            (&[None, None], Role::Text),
            (&[], Role::Text),
        ];
        for (values, role) in cases {
            assert_eq!(
                Role::of(ValueKind::Undeclared, &cells(values)),
                role,
                "{values:?}"
            );
        }
    }
}
