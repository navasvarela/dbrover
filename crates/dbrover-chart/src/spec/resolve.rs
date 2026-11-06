//! A spec resolved against a result's columns: the [`Choice`] it draws, and how its rows are
//! sorted, cut and titled.

use std::fmt;

use super::ChartSpec;
use crate::choice::{starting_x, starting_ys};
use crate::{ChartKind, Choice, Column, LayoutOptions, Role, STARTING_SERIES};

/// A spec's chart of one result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub choice: Choice,
    pub sort: Option<SortBy>,
    pub limit: Option<usize>,
    pub options: LayoutOptions,
}

/// A column to sort by, by its index among the result's columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortBy {
    pub column: usize,
    pub descending: bool,
}

/// A column a spec names that the result doesn't have, or has twice, or that can't be what the
/// spec puts it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnProblem {
    pub detail: String,
}

impl fmt::Display for ColumnProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chart: {}", self.detail)
    }
}

impl ChartSpec {
    /// The chart this spec draws of a result with `columns`, whose roles are `roles`. A key the
    /// spec leaves out takes the value the chart tab would start with, given the keys that are
    /// present: without `kind`, an X of dates draws lines.
    ///
    /// # Errors
    ///
    /// Every column the spec names that the result doesn't have (by its exact name, since
    /// databases differ on case), has twice, or holds the wrong role for where it goes; and a
    /// result with no column of numbers left for Y.
    pub fn resolve(
        &self,
        columns: &[Column],
        roles: &[Role],
    ) -> Result<Resolved, Vec<ColumnProblem>> {
        let finder = Finder { columns, roles };
        let mut problems = Vec::new();
        let mut found = |key: &str, name: &str| {
            finder
                .find(key, name)
                .map_err(|problem| problems.push(problem))
                .ok()
        };
        let x = self.x.as_deref().map(|name| found("x", name));
        let named_ys: Vec<Option<usize>> = self.y.iter().map(|name| found("y", name)).collect();
        let sort = self
            .sort
            .as_ref()
            .map(|sort| (found("sort", &sort.column), sort.descending));
        let ys: Vec<usize> = named_ys.iter().flatten().copied().collect();
        problems.extend(finder.not_numbers(&ys));
        if let Some(Some(x)) = x
            && ys.contains(&x)
        {
            problems.push(ColumnProblem {
                detail: format!("`x` and `y` both name `{}`", finder.name(x)),
            });
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let x = match x {
            Some(x) => x,
            None => starting_x(roles, &ys),
        };
        let Some(x) = x else {
            return Err(vec![ColumnProblem {
                detail: "every column is on Y; leave one for `x`".to_owned(),
            }]);
        };
        let ys = if self.y.is_empty() {
            starting_ys(roles, x)
        } else {
            ys
        };
        if ys.is_empty() {
            return Err(vec![ColumnProblem {
                detail: format!(
                    "no column but `{}` holds numbers, so Y has none to measure",
                    finder.name(x)
                ),
            }]);
        }
        Ok(Resolved {
            choice: Choice {
                kind: self
                    .kind
                    .unwrap_or_else(|| ChartKind::starting(roles.get(x).copied())),
                x,
                ys,
            },
            sort: sort.and_then(|(column, descending)| {
                Some(SortBy {
                    column: column?,
                    descending,
                })
            }),
            limit: self.limit,
            options: self.layout_options(),
        })
    }

    /// The spec's titles and Y axis, which name no column and so hold whatever the result.
    pub fn layout_options(&self) -> LayoutOptions {
        LayoutOptions {
            title: self.title.clone(),
            x_title: self.x_title.clone(),
            y_title: self.y_title.clone(),
            y_zero: self.y_zero,
        }
    }
}

impl Resolved {
    /// What `sort` and `limit` did, for the line under the chart: "sorted by net, the first 10".
    /// `None` when the spec sets neither.
    pub fn arrangement(&self, columns: &[Column]) -> Option<String> {
        let sorted = self.sort.map(|sort| {
            let name = columns
                .get(sort.column)
                .map_or("", |column| column.name.as_str());
            let direction = if sort.descending {
                ", largest first"
            } else {
                ""
            };
            format!("sorted by {name}{direction}")
        });
        let cut = self.limit.map(|limit| match limit {
            1 => "the first row".to_owned(),
            limit => format!("the first {limit}"),
        });
        match (sorted, cut) {
            (Some(sorted), Some(cut)) => Some(format!("{sorted}, {cut}")),
            (one, other) => one.or(other),
        }
    }
}

struct Finder<'a> {
    columns: &'a [Column],
    roles: &'a [Role],
}

impl Finder<'_> {
    /// The column named exactly `name`, which `key` names.
    fn find(&self, key: &str, name: &str) -> Result<usize, ColumnProblem> {
        let mut exact = self
            .columns
            .iter()
            .enumerate()
            .filter(|(_, column)| column.name == name)
            .map(|(index, _)| index);
        match (exact.next(), exact.next()) {
            (Some(index), None) => return Ok(index),
            (Some(_), Some(_)) => {
                return Err(ColumnProblem {
                    detail: format!("two columns are named `{name}`; name them apart with `AS`"),
                });
            }
            (None, _) => {}
        }
        let same_but_case = self
            .columns
            .iter()
            .find(|column| column.name.to_lowercase() == name.to_lowercase());
        let detail = match same_but_case {
            Some(column) => format!(
                "`{key} = \"{name}\"` isn't a column of this result; did you mean `{}`?",
                column.name
            ),
            None => {
                let names: Vec<&str> = self
                    .columns
                    .iter()
                    .map(|column| column.name.as_str())
                    .collect();
                format!(
                    "`{key} = \"{name}\"` isn't a column of this result: {}",
                    names.join(", ")
                )
            }
        };
        Err(ColumnProblem { detail })
    }

    /// A problem for each of `ys` that doesn't hold numbers, and for more Ys than colours.
    fn not_numbers(&self, ys: &[usize]) -> Vec<ColumnProblem> {
        let mut problems: Vec<ColumnProblem> = ys
            .iter()
            .filter_map(|&y| {
                let role = self.roles.get(y).copied()?;
                let holds = match role {
                    Role::Number => return None,
                    Role::Time => "dates",
                    Role::Text => "text",
                };
                Some(ColumnProblem {
                    detail: format!(
                        "`y = \"{}\"` holds {holds}, and Y needs numbers",
                        self.name(y)
                    ),
                })
            })
            .collect();
        if ys.len() > STARTING_SERIES {
            problems.push(ColumnProblem {
                detail: format!(
                    "`y` names {} columns, and a chart has colours for {STARTING_SERIES}",
                    ys.len()
                ),
            });
        }
        problems
    }

    fn name(&self, index: usize) -> &str {
        self.columns
            .get(index)
            .map_or("", |column| column.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::ValueKind;

    use super::*;
    use crate::spec::Sort;
    use Role::{Number, Text, Time};

    fn columns(names: &[&str]) -> Vec<Column> {
        names
            .iter()
            .map(|name| Column {
                name: (*name).to_owned(),
                kind: ValueKind::Undeclared,
            })
            .collect()
    }

    /// The shop's net sales by month and category.
    const NAMES: [&str; 4] = ["month", "category", "net", "orders"];
    const ROLES: [Role; 4] = [Time, Text, Number, Number];

    fn resolve(spec: &ChartSpec) -> Result<Resolved, Vec<String>> {
        spec.resolve(&columns(&NAMES), &ROLES).map_err(|problems| {
            problems
                .into_iter()
                .map(|problem| problem.to_string())
                .collect()
        })
    }

    fn choice(spec: &ChartSpec) -> Choice {
        resolve(spec).unwrap().choice
    }

    fn named(name: &str) -> Option<String> {
        Some(name.to_owned())
    }

    #[test]
    fn an_empty_spec_is_the_starting_choice() {
        assert_eq!(
            Some(choice(&ChartSpec::default())),
            Choice::starting(&ROLES)
        );
    }

    #[test]
    fn missing_keys_follow_the_keys_present() {
        let cases = [
            (
                ChartSpec {
                    x: named("category"),
                    ..ChartSpec::default()
                },
                Choice {
                    kind: ChartKind::Bars,
                    x: 1,
                    ys: vec![2, 3],
                },
            ),
            (
                ChartSpec {
                    y: vec!["orders".to_owned()],
                    ..ChartSpec::default()
                },
                Choice {
                    kind: ChartKind::Lines,
                    x: 0,
                    ys: vec![3],
                },
            ),
            (
                ChartSpec {
                    kind: Some(ChartKind::Bars),
                    ..ChartSpec::default()
                },
                Choice {
                    kind: ChartKind::Bars,
                    x: 0,
                    ys: vec![2, 3],
                },
            ),
            (
                ChartSpec {
                    x: named("net"),
                    ..ChartSpec::default()
                },
                Choice {
                    kind: ChartKind::Scatter,
                    x: 2,
                    ys: vec![3],
                },
            ),
            (
                ChartSpec {
                    y: vec!["orders".to_owned(), "net".to_owned()],
                    ..ChartSpec::default()
                },
                Choice {
                    kind: ChartKind::Lines,
                    x: 0,
                    ys: vec![3, 2],
                },
            ),
        ];
        for (spec, expected) in cases {
            assert_eq!(choice(&spec), expected, "{spec:?}");
        }
    }

    #[test]
    fn x_avoids_the_columns_named_on_y() {
        let names = ["net", "orders"];
        let roles = [Number, Number];
        let spec = ChartSpec {
            y: vec!["net".to_owned()],
            ..ChartSpec::default()
        };
        let resolved = spec.resolve(&columns(&names), &roles).unwrap();
        assert_eq!(resolved.choice.x, 1);
        assert_eq!(resolved.choice.ys, vec![0]);
    }

    #[test]
    fn says_each_problem_with_a_column() {
        let cases: [(ChartSpec, &[&str]); 7] = [
            (
                ChartSpec {
                    y: vec!["nett".to_owned()],
                    ..ChartSpec::default()
                },
                &[
                    "chart: `y = \"nett\"` isn't a column of this result: month, category, net, \
                   orders",
                ],
            ),
            (
                ChartSpec {
                    x: named("Month"),
                    ..ChartSpec::default()
                },
                &["chart: `x = \"Month\"` isn't a column of this result; did you mean `month`?"],
            ),
            (
                ChartSpec {
                    y: vec!["month".to_owned()],
                    ..ChartSpec::default()
                },
                &["chart: `y = \"month\"` holds dates, and Y needs numbers"],
            ),
            (
                ChartSpec {
                    y: vec!["category".to_owned()],
                    ..ChartSpec::default()
                },
                &["chart: `y = \"category\"` holds text, and Y needs numbers"],
            ),
            (
                ChartSpec {
                    x: named("net"),
                    y: vec!["net".to_owned()],
                    ..ChartSpec::default()
                },
                &["chart: `x` and `y` both name `net`"],
            ),
            (
                ChartSpec {
                    sort: Some(Sort {
                        column: "revenue".to_owned(),
                        descending: true,
                    }),
                    ..ChartSpec::default()
                },
                &[
                    "chart: `sort = \"revenue\"` isn't a column of this result: month, category, \
                   net, orders",
                ],
            ),
            (
                ChartSpec {
                    x: named("week"),
                    y: vec!["nett".to_owned()],
                    ..ChartSpec::default()
                },
                &[
                    "chart: `x = \"week\"` isn't a column of this result: month, category, net, \
                     orders",
                    "chart: `y = \"nett\"` isn't a column of this result: month, category, net, \
                     orders",
                ],
            ),
        ];
        for (spec, expected) in cases {
            assert_eq!(
                resolve(&spec),
                Err(expected.iter().map(|line| (*line).to_owned()).collect()),
                "{spec:?}"
            );
        }
    }

    #[test]
    fn a_column_named_twice_is_named_apart() {
        let spec = ChartSpec {
            y: vec!["count".to_owned()],
            ..ChartSpec::default()
        };
        let problems = spec
            .resolve(
                &columns(&["day", "count", "count"]),
                &[Time, Number, Number],
            )
            .unwrap_err();
        assert_eq!(
            problems[0].to_string(),
            "chart: two columns are named `count`; name them apart with `AS`"
        );
    }

    #[test]
    fn a_result_without_numbers_for_y_says_so() {
        let problems = ChartSpec::default()
            .resolve(&columns(&["month", "category"]), &[Time, Text])
            .unwrap_err();
        assert_eq!(
            problems[0].to_string(),
            "chart: no column but `month` holds numbers, so Y has none to measure"
        );
        let all_on_y = ChartSpec {
            y: vec!["net".to_owned()],
            ..ChartSpec::default()
        };
        let problems = all_on_y.resolve(&columns(&["net"]), &[Number]).unwrap_err();
        assert_eq!(
            problems[0].to_string(),
            "chart: every column is on Y; leave one for `x`"
        );
    }

    #[test]
    fn more_ys_than_colours_is_a_problem() {
        let names: Vec<String> = (0..10).map(|index| format!("c{index}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut roles = vec![Text];
        roles.extend([Number; 9]);
        let spec = ChartSpec {
            y: names[1..].iter().map(|name| (*name).to_owned()).collect(),
            ..ChartSpec::default()
        };
        let problems = spec.resolve(&columns(&names), &roles).unwrap_err();
        assert_eq!(
            problems[0].to_string(),
            "chart: `y` names 9 columns, and a chart has colours for 8"
        );
    }

    #[test]
    fn sort_limit_and_titles_carry_through() {
        let spec = ChartSpec {
            title: named("Net sales"),
            y_zero: true,
            sort: Some(Sort {
                column: "net".to_owned(),
                descending: true,
            }),
            limit: Some(10),
            ..ChartSpec::default()
        };
        let resolved = resolve(&spec).unwrap();
        assert_eq!(
            resolved.sort,
            Some(SortBy {
                column: 2,
                descending: true
            })
        );
        assert_eq!(resolved.limit, Some(10));
        assert_eq!(resolved.options.title.as_deref(), Some("Net sales"));
        assert!(resolved.options.y_zero);
        assert_eq!(
            resolved.arrangement(&columns(&NAMES)).as_deref(),
            Some("sorted by net, largest first, the first 10")
        );
    }

    #[test]
    fn says_how_the_rows_were_arranged() {
        let resolved = |sort: Option<SortBy>, limit: Option<usize>| Resolved {
            choice: Choice {
                kind: ChartKind::Bars,
                x: 1,
                ys: vec![2],
            },
            sort,
            limit,
            options: LayoutOptions::default(),
        };
        let by_month = Some(SortBy {
            column: 0,
            descending: false,
        });
        let cases = [
            (None, None, None),
            (by_month, None, Some("sorted by month")),
            (None, Some(1), Some("the first row")),
            (None, Some(5), Some("the first 5")),
        ];
        for (sort, limit, expected) in cases {
            assert_eq!(
                resolved(sort, limit)
                    .arrangement(&columns(&NAMES))
                    .as_deref(),
                expected
            );
        }
    }
}
