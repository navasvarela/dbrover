//! Which columns a chart draws, and how.

use crate::Role;

/// How many series a chart starts with: one per colour of the palette, so no two start alike.
pub const STARTING_SERIES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChartKind {
    Bars,
    Lines,
    Scatter,
}

impl ChartKind {
    pub const ALL: [Self; 3] = [Self::Bars, Self::Lines, Self::Scatter];

    pub fn label(self) -> &'static str {
        match self {
            Self::Bars => "Bars",
            Self::Lines => "Lines",
            Self::Scatter => "Scatter",
        }
    }
}

/// A chart of some columns of a result: `x` along the bottom, and each of `ys` measured up
/// the side, by their indexes among the result's columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub kind: ChartKind,
    pub x: usize,
    pub ys: Vec<usize>,
}

impl Choice {
    /// The chart a result starts with, from its columns' roles: X is the first time or text
    /// column (the first column when all are numbers), Y every other column of numbers, up to
    /// [`STARTING_SERIES`]. Lines over times, a scatter over numbers, bars over text. `None`
    /// when no column of numbers is left for Y.
    pub fn starting(roles: &[Role]) -> Option<Self> {
        let x = roles
            .iter()
            .position(|role| *role != Role::Number)
            .unwrap_or(0);
        let ys: Vec<usize> = roles
            .iter()
            .enumerate()
            .filter(|&(index, role)| index != x && *role == Role::Number)
            .map(|(index, _)| index)
            .take(STARTING_SERIES)
            .collect();
        if ys.is_empty() {
            return None;
        }
        let kind = match roles.get(x) {
            Some(Role::Time) => ChartKind::Lines,
            Some(Role::Number) => ChartKind::Scatter,
            Some(Role::Text) | None => ChartKind::Bars,
        };
        Some(Self { kind, x, ys })
    }

    /// Picks `x` for the X axis, and stops measuring it on Y.
    pub fn set_x(&mut self, x: usize) {
        self.x = x;
        self.ys.retain(|&y| y != x);
    }

    /// Adds `y` to the series, or takes it away, keeping them in the result's column order. The
    /// X column is never a series.
    pub fn toggle_y(&mut self, y: usize) {
        if let Some(position) = self.ys.iter().position(|&shown| shown == y) {
            self.ys.remove(position);
        } else if y != self.x {
            let position = self.ys.partition_point(|&shown| shown < y);
            self.ys.insert(position, y);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Role::{Number, Text, Time};

    #[test]
    fn starts_from_the_columns_roles() {
        let cases: [(&[Role], Option<Choice>); 6] = [
            (
                &[Text, Number],
                Some(Choice {
                    kind: ChartKind::Bars,
                    x: 0,
                    ys: vec![1],
                }),
            ),
            (
                &[Time, Number, Text, Number],
                Some(Choice {
                    kind: ChartKind::Lines,
                    x: 0,
                    ys: vec![1, 3],
                }),
            ),
            (
                &[Number, Text, Number],
                Some(Choice {
                    kind: ChartKind::Bars,
                    x: 1,
                    ys: vec![0, 2],
                }),
            ),
            (
                &[Number, Number],
                Some(Choice {
                    kind: ChartKind::Scatter,
                    x: 0,
                    ys: vec![1],
                }),
            ),
            (&[Text, Time], None),
            (&[Number], None),
        ];
        for (roles, expected) in cases {
            assert_eq!(Choice::starting(roles), expected, "{roles:?}");
        }
    }

    #[test]
    fn starts_with_at_most_one_series_per_colour() {
        let mut roles = vec![Text];
        roles.extend([Number; 12]);
        let choice = Choice::starting(&roles).unwrap();
        assert_eq!(choice.ys, (1..=STARTING_SERIES).collect::<Vec<_>>());
    }

    #[test]
    fn the_x_column_is_never_a_series() {
        let mut choice = Choice {
            kind: ChartKind::Bars,
            x: 0,
            ys: vec![1, 2],
        };
        choice.set_x(2);
        assert_eq!(choice.ys, vec![1]);
        choice.toggle_y(2);
        assert_eq!(choice.ys, vec![1]);
    }

    #[test]
    fn series_toggle_in_column_order() {
        let mut choice = Choice {
            kind: ChartKind::Bars,
            x: 0,
            ys: vec![3],
        };
        choice.toggle_y(1);
        choice.toggle_y(4);
        assert_eq!(choice.ys, vec![1, 3, 4]);
        choice.toggle_y(3);
        assert_eq!(choice.ys, vec![1, 4]);
    }
}
