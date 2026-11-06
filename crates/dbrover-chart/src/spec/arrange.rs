//! A spec's `sort` and `limit`, applied to the rows before they are charted.

use std::cmp::Ordering;

use dbrover_core::Row;

use jiff::tz::Offset;

use super::SortBy;
use crate::Role;
use crate::data::seconds;
use crate::values;

/// The rows to chart, as indexes into `rows`: `order` sorted by `sort` and cut to `limit`.
/// `roles` are the rows' columns' roles.
///
/// The sort column is compared by its role: numbers by value, dates by the instant they name,
/// text by its characters' codes, as the grid compares text. A row whose value is null, or
/// doesn't read as its role, comes after every row that has one, whichever the direction, so
/// that "the first 10" are never rows with nothing to show. Equal values keep `order`'s order.
pub fn arrange(
    rows: &[Row],
    order: &[usize],
    roles: &[Role],
    sort: Option<SortBy>,
    limit: Option<usize>,
) -> Vec<usize> {
    let mut arranged = order.to_vec();
    if let Some(sort) = sort {
        let role = roles.get(sort.column).copied().unwrap_or(Role::Text);
        let key = |index: &usize| {
            let text = rows
                .get(*index)
                .and_then(|row| row.get(sort.column))
                .and_then(Option::as_deref);
            SortKey::of(role, text)
        };
        arranged.sort_by(|a, b| {
            let (a, b) = (key(a), key(b));
            let ordering = a.cmp_present(&b);
            match (sort.descending, ordering) {
                (true, Some(ordering)) => ordering.reverse(),
                (false, Some(ordering)) => ordering,
                (_, None) => a.is_missing().cmp(&b.is_missing()),
            }
        });
    }
    if let Some(limit) = limit {
        arranged.truncate(limit);
    }
    arranged
}

/// A value to sort by, read as its column's role; `Missing` when there is none.
enum SortKey<'a> {
    Missing,
    Number(f64),
    Text(&'a str),
}

impl<'a> SortKey<'a> {
    fn of(role: Role, text: Option<&'a str>) -> Self {
        let Some(text) = text else {
            return Self::Missing;
        };
        let number = match role {
            Role::Number => values::number(text),
            Role::Time => values::time(text).map(|time| seconds(time.in_offset(Offset::UTC))),
            Role::Text => return Self::Text(text),
        };
        number.map_or(Self::Missing, Self::Number)
    }

    fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    /// How two present values compare; `None` when either is missing, which sorts last
    /// whichever the direction.
    fn cmp_present(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => Some(a.total_cmp(b)),
            (Self::Text(a), Self::Text(b)) => Some(a.cmp(b)),
            // One role reads every value of a column, so a number and a text never meet.
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(values: &[Option<&str>]) -> Vec<Row> {
        values
            .iter()
            .map(|value| vec![value.map(str::to_owned)])
            .collect()
    }

    fn by(descending: bool) -> Option<SortBy> {
        Some(SortBy {
            column: 0,
            descending,
        })
    }

    #[test]
    fn sorts_by_the_columns_role_with_missing_values_last() {
        let numbers = rows(&[Some("9"), None, Some("10"), Some("x"), Some("-1")]);
        let all: Vec<usize> = (0..numbers.len()).collect();
        let cases = [
            (Role::Number, by(false), vec![4, 0, 2, 1, 3]),
            (Role::Number, by(true), vec![2, 0, 4, 1, 3]),
            (Role::Text, by(false), vec![4, 2, 0, 3, 1]),
            (Role::Number, None, vec![0, 1, 2, 3, 4]),
        ];
        for (role, sort, expected) in cases {
            assert_eq!(
                arrange(&numbers, &all, &[role], sort, None),
                expected,
                "{role:?} {sort:?}"
            );
        }
    }

    #[test]
    fn dates_sort_by_the_instant_they_name() {
        let times = rows(&[
            Some("2026-03-01 01:00:00+02"),
            Some("2026-03-01 00:00:00+00"),
            Some("2026-02-28"),
        ]);
        assert_eq!(
            arrange(&times, &[0, 1, 2], &[Role::Time], by(false), None),
            vec![2, 0, 1]
        );
    }

    #[test]
    fn keeps_the_grids_order_and_filter_then_cuts() {
        let numbers = rows(&[Some("1"), Some("2"), Some("3"), Some("4")]);
        let filtered = [3, 1, 2];
        assert_eq!(
            arrange(&numbers, &filtered, &[Role::Number], None, Some(2)),
            vec![3, 1]
        );
        assert_eq!(
            arrange(&numbers, &filtered, &[Role::Number], by(true), Some(2)),
            vec![3, 2]
        );
        assert_eq!(
            arrange(&numbers, &filtered, &[Role::Number], None, Some(10)),
            vec![3, 1, 2]
        );
    }
}
