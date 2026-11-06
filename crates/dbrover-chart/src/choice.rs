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

    /// The kind's name in a chart spec: `kind = "bars"`.
    pub fn spec_name(self) -> &'static str {
        match self {
            Self::Bars => "bars",
            Self::Lines => "lines",
            Self::Scatter => "scatter",
        }
    }

    /// The kind a spec names, exactly as [`Self::spec_name`] writes it.
    pub fn from_spec_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.spec_name() == name)
    }

    /// The kind a chart starts with over an X of `role`: lines over times, a scatter over
    /// numbers, bars over text.
    pub(crate) fn starting(role: Option<Role>) -> Self {
        match role {
            Some(Role::Time) => Self::Lines,
            Some(Role::Number) => Self::Scatter,
            Some(Role::Text) | None => Self::Bars,
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
    /// A column whose values split the one Y into a series each (CH2): `GROUP BY month,
    /// category` charted with a series per category. `ys` then holds one column.
    pub series: Option<usize>,
    /// Stack the series' bars rather than set them side by side. Only bars stack.
    pub stack: bool,
}

impl Choice {
    /// The chart a result starts with, from its columns' roles: X is the first time or text
    /// column (the first column when all are numbers), Y every other column of numbers, up to
    /// [`STARTING_SERIES`]. Lines over times, a scatter over numbers, bars over text. `None`
    /// when no column of numbers is left for Y.
    pub fn starting(roles: &[Role]) -> Option<Self> {
        let x = starting_x(roles, &[])?;
        let ys = starting_ys(roles, x);
        if ys.is_empty() {
            return None;
        }
        let kind = ChartKind::starting(roles.get(x).copied());
        Some(Self::of(kind, x, ys))
    }

    /// A chart of `ys` over `x`, neither split into series nor stacked.
    pub fn of(kind: ChartKind, x: usize, ys: Vec<usize>) -> Self {
        Self {
            kind,
            x,
            ys,
            series: None,
            stack: false,
        }
    }

    /// Picks `x` for the X axis, and stops measuring it on Y or splitting by it.
    pub fn set_x(&mut self, x: usize) {
        self.x = x;
        self.ys.retain(|&y| y != x);
        if self.series == Some(x) {
            self.series = None;
        }
    }

    /// Adds `y` to the series, or takes it away, keeping them in the result's column order. The
    /// X column is never a series. While the chart is split by a column, `y` replaces the one Y
    /// instead.
    pub fn toggle_y(&mut self, y: usize) {
        if y == self.x || Some(y) == self.series {
            return;
        }
        if self.series.is_some() {
            self.ys = vec![y];
        } else if let Some(position) = self.ys.iter().position(|&shown| shown == y) {
            self.ys.remove(position);
        } else {
            let position = self.ys.partition_point(|&shown| shown < y);
            self.ys.insert(position, y);
        }
    }

    /// Splits the one Y by `series`'s values, or stops splitting. Splitting keeps the first Y
    /// only, and never the X column or the series column itself.
    pub fn set_series(&mut self, series: Option<usize>) {
        let Some(column) = series else {
            self.series = None;
            return;
        };
        if column == self.x {
            return;
        }
        self.ys.retain(|&y| y != column);
        self.ys.truncate(1);
        self.series = Some(column);
    }

    /// Whether the chart draws more than one series from its columns: several Ys, or one split
    /// by a column. Only then is there anything to stack.
    pub fn has_series(&self) -> bool {
        self.series.is_some() || self.ys.len() > 1
    }

    /// Whether its bars are drawn stacked: asked for, and bars.
    pub fn stacked(&self) -> bool {
        self.stack && self.kind == ChartKind::Bars
    }
}

/// The X a chart starts with, among the columns not in `taken`: the first time or text column,
/// else the first column. `None` when `taken` holds every column.
pub(crate) fn starting_x(roles: &[Role], taken: &[usize]) -> Option<usize> {
    let free = || (0..roles.len()).filter(|index| !taken.contains(index));
    free()
        .find(|&index| roles.get(index) != Some(&Role::Number))
        .or_else(|| free().next())
}

/// The Ys a chart starts with over `x`: every other column of numbers, up to
/// [`STARTING_SERIES`].
pub(crate) fn starting_ys(roles: &[Role], x: usize) -> Vec<usize> {
    roles
        .iter()
        .enumerate()
        .filter(|&(index, role)| index != x && *role == Role::Number)
        .map(|(index, _)| index)
        .take(STARTING_SERIES)
        .collect()
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
                Some(Choice::of(ChartKind::Bars, 0, vec![1])),
            ),
            (
                &[Time, Number, Text, Number],
                Some(Choice::of(ChartKind::Lines, 0, vec![1, 3])),
            ),
            (
                &[Number, Text, Number],
                Some(Choice::of(ChartKind::Bars, 1, vec![0, 2])),
            ),
            (
                &[Number, Number],
                Some(Choice::of(ChartKind::Scatter, 0, vec![1])),
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
        let mut choice = Choice::of(ChartKind::Bars, 0, vec![1, 2]);
        choice.set_x(2);
        assert_eq!(choice.ys, vec![1]);
        choice.toggle_y(2);
        assert_eq!(choice.ys, vec![1]);
    }

    #[test]
    fn a_split_chart_has_one_y_and_never_splits_by_x() {
        let mut choice = Choice::of(ChartKind::Bars, 0, vec![2, 3]);
        choice.set_series(Some(0));
        assert_eq!(choice.series, None, "X can't split itself");
        choice.set_series(Some(1));
        assert_eq!((choice.series, choice.ys.clone()), (Some(1), vec![2]));
        assert!(choice.has_series());
        choice.toggle_y(3);
        assert_eq!(choice.ys, vec![3], "Y is one column while split");
        choice.toggle_y(1);
        assert_eq!(choice.ys, vec![3], "the series column is never Y");
        choice.set_x(1);
        assert_eq!(choice.series, None, "X can't be the series column");
        choice.set_series(Some(3));
        assert_eq!((choice.series, choice.ys.clone()), (Some(3), vec![]));
    }

    #[test]
    fn only_bars_stack() {
        let mut choice = Choice::of(ChartKind::Bars, 0, vec![1, 2]);
        choice.stack = true;
        assert!(choice.stacked());
        choice.kind = ChartKind::Lines;
        assert!(!choice.stacked());
    }

    #[test]
    fn series_toggle_in_column_order() {
        let mut choice = Choice::of(ChartKind::Bars, 0, vec![3]);
        choice.toggle_y(1);
        choice.toggle_y(4);
        assert_eq!(choice.ys, vec![1, 3, 4]);
        choice.toggle_y(3);
        assert_eq!(choice.ys, vec![1, 4]);
    }
}
