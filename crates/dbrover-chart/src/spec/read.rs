//! Reading a spec's text into a [`ChartSpec`], with every problem it has.

use std::ops::Range;

use toml_edit::{Document, Item, Table, Value};

use super::{ChartSpec, Key, SPEC_TABLE, Sort, SpecProblem};
use crate::ChartKind;

/// A misspelt key is suggested when it is at most this many letters' edits from a key: `ys`
/// and `kinds` are one away. Three would suggest `x` for any key of up to three letters.
const MAX_SUGGESTION_DISTANCE: usize = 2;

impl ChartSpec {
    /// Reads a spec's text: a `[chart]` table of the spec's keys, the header included.
    ///
    /// # Errors
    ///
    /// Every problem the text has, in the order of their lines: TOML that doesn't read (only
    /// that one, since nothing after it can be read), keys that don't exist, and values of the
    /// wrong kind. A key is refused rather than ignored, so a misspelt one is never silently
    /// left out of the chart.
    pub fn read(text: &str) -> Result<Self, Vec<SpecProblem>> {
        let document = Document::parse(text).map_err(|error| {
            let span = error.span().unwrap_or(0..0);
            vec![SpecProblem::new(text, span, error.message().trim())]
        })?;
        let mut reader = Reader {
            text,
            problems: Vec::new(),
        };
        let spec = reader.document(&document);
        let mut problems = reader.problems;
        if problems.is_empty() {
            Ok(spec)
        } else {
            problems.sort_by_key(|problem| problem.span.start);
            Err(problems)
        }
    }
}

struct Reader<'a> {
    text: &'a str,
    problems: Vec<SpecProblem>,
}

impl Reader<'_> {
    fn document(&mut self, document: &Document<&str>) -> ChartSpec {
        let mut spec = ChartSpec::default();
        for (name, item) in document.iter() {
            match (name, item) {
                (SPEC_TABLE, Item::Table(table)) => spec = self.table(table),
                _ => {
                    let span = self.key_span(document.as_table(), name, item);
                    self.problem(
                        span,
                        format!("a chart is the keys under `[{SPEC_TABLE}]`, not `{name}`"),
                    );
                }
            }
        }
        spec
    }

    fn table(&mut self, table: &Table) -> ChartSpec {
        let mut spec = ChartSpec::default();
        for (name, item) in table.iter() {
            let key_span = self.key_span(table, name, item);
            let Some(key) = Key::named(name) else {
                self.problem(key_span, unknown_key(name));
                continue;
            };
            let value = ValueAt {
                item,
                span: item.span().unwrap_or_else(|| key_span.clone()),
            };
            self.key(&mut spec, key, &value);
        }
        spec
    }

    fn key(&mut self, spec: &mut ChartSpec, key: Key, value: &ValueAt<'_>) {
        match key {
            Key::Kind => spec.kind = self.kind(value),
            Key::X => spec.x = self.text_value(value, "`x` is a column's name, in quotes"),
            Key::Y => spec.y = self.columns(value),
            Key::Series => {
                spec.series = self.text_value(value, "`series` is a column's name, in quotes");
            }
            Key::Title => spec.title = self.text_value(value, "`title` is text, in quotes"),
            Key::XTitle => spec.x_title = self.text_value(value, "`x_title` is text, in quotes"),
            Key::YTitle => spec.y_title = self.text_value(value, "`y_title` is text, in quotes"),
            Key::YZero => spec.y_zero = self.y_zero(value),
            Key::Sort => spec.sort = self.sort(value),
            Key::Limit => spec.limit = self.limit(value),
        }
    }

    fn kind(&mut self, value: &ValueAt<'_>) -> Option<ChartKind> {
        let kind = value.as_str().and_then(ChartKind::from_spec_name);
        if kind.is_none() {
            let names: Vec<&str> = ChartKind::ALL.map(ChartKind::spec_name).to_vec();
            let detail = format!("`kind` is {}", or_list(&names));
            self.wrong(value, &detail);
        }
        kind
    }

    fn text_value(&mut self, value: &ValueAt<'_>, what: &str) -> Option<String> {
        let text = value.as_str().map(str::to_owned);
        if text.is_none() {
            self.wrong(value, what);
        }
        text
    }

    /// `y`: one column's name, or a list of them.
    fn columns(&mut self, value: &ValueAt<'_>) -> Vec<String> {
        const WHAT: &str = "`y` is a column's name, or a list of them, in quotes";
        if let Some(name) = value.as_str() {
            return vec![name.to_owned()];
        }
        let Some(Value::Array(items)) = value.item.as_value() else {
            self.wrong(value, WHAT);
            return Vec::new();
        };
        if items.is_empty() {
            self.problem(
                value.span.clone(),
                "`y = []` names no column; leave `y` out to chart every column of numbers",
            );
            return Vec::new();
        }
        let mut names = Vec::new();
        for item in items {
            match item.as_str() {
                Some(name) => names.push(name.to_owned()),
                None => {
                    let span = item.span().unwrap_or_else(|| value.span.clone());
                    let detail = format!("{WHAT}, not {}", self.raw(&span));
                    self.problem(span, detail);
                }
            }
        }
        names
    }

    fn y_zero(&mut self, value: &ValueAt<'_>) -> bool {
        let flag = value.item.as_value().and_then(Value::as_bool);
        if flag.is_none() {
            self.wrong(value, "`y_zero` is true or false");
        }
        flag.unwrap_or_default()
    }

    fn sort(&mut self, value: &ValueAt<'_>) -> Option<Sort> {
        let Some(text) = value.as_str() else {
            self.wrong(
                value,
                "`sort` is a column's name, then asc or desc, in quotes",
            );
            return None;
        };
        let sort = Sort::parse(text);
        if sort.is_none() {
            let detail = format!(
                "`sort = {}`: write a column, then `asc` or `desc`",
                self.raw(&value.span)
            );
            self.problem(value.span.clone(), detail);
        }
        sort
    }

    fn limit(&mut self, value: &ValueAt<'_>) -> Option<usize> {
        let limit = value
            .item
            .as_value()
            .and_then(Value::as_integer)
            .and_then(|rows| usize::try_from(rows).ok())
            .filter(|rows| *rows > 0);
        if limit.is_none() {
            self.wrong(value, "`limit` is a number of rows, 1 or more");
        }
        limit
    }

    /// A value of the wrong kind: what the key takes, and what it was given.
    fn wrong(&mut self, value: &ValueAt<'_>, what: &str) {
        let given = match value.item {
            Item::Value(_) => self.raw(&value.span).to_owned(),
            Item::Table(_) | Item::ArrayOfTables(_) | Item::None => "a table".to_owned(),
        };
        self.problem(value.span.clone(), format!("{what}, not {given}"));
    }

    fn raw(&self, span: &Range<usize>) -> &str {
        self.text.get(span.clone()).map_or("", str::trim)
    }

    fn key_span(&self, table: &Table, name: &str, item: &Item) -> Range<usize> {
        table
            .key(name)
            .and_then(toml_edit::Key::span)
            .or_else(|| item.span())
            .unwrap_or(0..self.text.len())
    }

    fn problem(&mut self, span: Range<usize>, detail: impl Into<String>) {
        self.problems
            .push(SpecProblem::new(self.text, span, detail.into()));
    }
}

/// An item with the bytes it was read from.
struct ValueAt<'a> {
    item: &'a Item,
    span: Range<usize>,
}

impl ValueAt<'_> {
    fn as_str(&self) -> Option<&str> {
        self.item.as_value().and_then(Value::as_str)
    }
}

impl Sort {
    /// `"net desc"`: a column's name, which may hold spaces, then `asc` or `desc` in any case.
    fn parse(text: &str) -> Option<Self> {
        let (column, direction) = text.trim().rsplit_once(char::is_whitespace)?;
        let descending = match direction.to_ascii_lowercase().as_str() {
            "asc" => false,
            "desc" => true,
            _ => return None,
        };
        let column = column.trim_end();
        (!column.is_empty()).then(|| Self {
            column: column.to_owned(),
            descending,
        })
    }

    /// The way the spec writes it back: `net desc`.
    pub fn written(&self) -> String {
        let direction = if self.descending { "desc" } else { "asc" };
        format!("{} {direction}", self.column)
    }
}

/// "no key `ys`; did you mean `y`?", or the keys there are when none is near.
fn unknown_key(name: &str) -> String {
    let lowered = name.to_lowercase();
    let nearest = Key::ALL
        .into_iter()
        .map(|key| (distance(&lowered, key.name()), key.name()))
        .filter(|(distance, _)| *distance <= MAX_SUGGESTION_DISTANCE)
        .min_by_key(|(distance, _)| *distance);
    match nearest {
        Some((_, suggestion)) => format!("no key `{name}`; did you mean `{suggestion}`?"),
        None => {
            let names: Vec<String> = Key::ALL
                .into_iter()
                .map(|key| format!("`{}`", key.name()))
                .collect();
            format!("no key `{name}`; a chart's keys are {}", names.join(", "))
        }
    }
}

/// How many letters to insert, delete or change to turn `a` into `b` (Levenshtein's distance).
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, a_char) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, b_char) in b.iter().enumerate() {
            let changed = previous[j] + usize::from(a_char != *b_char);
            let inserted = current[j] + 1;
            let deleted = previous[j + 1] + 1;
            current.push(changed.min(inserted).min(deleted));
        }
        previous = current;
    }
    previous[b.len()]
}

/// "bars, lines or scatter".
fn or_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(keys: &str) -> Result<ChartSpec, Vec<String>> {
        ChartSpec::read(&format!("[chart]\n{keys}")).map_err(|problems| {
            problems
                .into_iter()
                .map(|problem| problem.to_string())
                .collect()
        })
    }

    #[test]
    fn reads_every_key() {
        let spec = read(
            r#"
kind = "bars"   # by month
x = "month"
y = ["net", "refunds"]
series = "category"
title = "Net sales"
x_title = "Month"
y_title = "Euros"
y_zero = true
sort = "net desc"
limit = 10
"#,
        );
        assert_eq!(
            spec,
            Ok(ChartSpec {
                kind: Some(ChartKind::Bars),
                x: Some("month".to_owned()),
                y: vec!["net".to_owned(), "refunds".to_owned()],
                series: Some("category".to_owned()),
                title: Some("Net sales".to_owned()),
                x_title: Some("Month".to_owned()),
                y_title: Some("Euros".to_owned()),
                y_zero: true,
                sort: Some(Sort {
                    column: "net".to_owned(),
                    descending: true,
                }),
                limit: Some(10),
            })
        );
    }

    #[test]
    fn an_empty_chart_is_a_chart() {
        assert_eq!(read(""), Ok(ChartSpec::default()));
        assert_eq!(
            read(r#"y = "net""#).map(|spec| spec.y),
            Ok(vec!["net".to_owned()])
        );
    }

    #[test]
    fn says_each_problem_at_its_line() {
        let cases: [(&str, &[&str]); 17] = [
            (
                "kind = ",
                &["chart, line 2: string values must be quoted, expected literal string"],
            ),
            (
                "ys = \"net\"",
                &["chart, line 2: no key `ys`; did you mean `y`?"],
            ),
            (
                "\nKind = \"bars\"",
                &["chart, line 3: no key `Kind`; did you mean `kind`?"],
            ),
            (
                "colour = \"red\"",
                &[
                    "chart, line 2: no key `colour`; a chart's keys are `kind`, `x`, `y`, \
                   `series`, `title`, `x_title`, `y_title`, `y_zero`, `sort`, `limit`",
                ],
            ),
            (
                "kind = \"area\"",
                &["chart, line 2: `kind` is bars, lines or scatter, not \"area\""],
            ),
            (
                "kind = \"Bars\"",
                &["chart, line 2: `kind` is bars, lines or scatter, not \"Bars\""],
            ),
            (
                "x = 3",
                &["chart, line 2: `x` is a column's name, in quotes, not 3"],
            ),
            (
                "y = 3",
                &["chart, line 2: `y` is a column's name, or a list of them, in quotes, not 3"],
            ),
            (
                "y = [\"net\", 2]",
                &["chart, line 2: `y` is a column's name, or a list of them, in quotes, not 2"],
            ),
            (
                "y = []",
                &[
                    "chart, line 2: `y = []` names no column; leave `y` out to chart every \
                   column of numbers",
                ],
            ),
            (
                "title = 1",
                &["chart, line 2: `title` is text, in quotes, not 1"],
            ),
            (
                "y_zero = \"yes\"",
                &["chart, line 2: `y_zero` is true or false, not \"yes\""],
            ),
            (
                "sort = \"net up\"",
                &["chart, line 2: `sort = \"net up\"`: write a column, then `asc` or `desc`"],
            ),
            (
                "sort = \"net\"",
                &["chart, line 2: `sort = \"net\"`: write a column, then `asc` or `desc`"],
            ),
            (
                "limit = 0",
                &["chart, line 2: `limit` is a number of rows, 1 or more, not 0"],
            ),
            (
                "limit = \"ten\"",
                &["chart, line 2: `limit` is a number of rows, 1 or more, not \"ten\""],
            ),
            (
                "x = 1\nlimit = -1",
                &[
                    "chart, line 2: `x` is a column's name, in quotes, not 1",
                    "chart, line 3: `limit` is a number of rows, 1 or more, not -1",
                ],
            ),
        ];
        for (keys, expected) in cases {
            assert_eq!(
                read(keys),
                Err(expected.iter().map(|line| (*line).to_owned()).collect()),
                "{keys}"
            );
        }
    }

    #[test]
    fn a_table_inside_the_chart_is_a_value_of_the_wrong_kind() {
        assert_eq!(
            read("[chart.x]\nname = \"month\""),
            Err(vec![
                "chart, line 2: `x` is a column's name, in quotes, not a table".to_owned()
            ])
        );
    }

    #[test]
    fn a_table_beside_the_chart_is_refused() {
        let problems = ChartSpec::read("[chart]\nx = \"month\"\n[other]\ny = 1").unwrap_err();
        assert_eq!(
            problems[0].to_string(),
            "chart, line 3: a chart is the keys under `[chart]`, not `other`"
        );
    }

    #[test]
    fn a_problem_points_at_its_bytes_and_moves_with_its_block() {
        let text = "[chart]\nkind = \"area\"";
        let problem = ChartSpec::read(text).unwrap_err().remove(0);
        assert_eq!(&text[problem.span.clone()], "\"area\"");
        assert_eq!(
            problem.moved_to(10).to_string(),
            "chart, line 11: `kind` is bars, lines or scatter, not \"area\""
        );
    }

    #[test]
    fn sort_takes_a_column_then_a_direction() {
        let cases = [
            ("net desc", Some(("net", true))),
            ("net ASC", Some(("net", false))),
            ("net sales  desc", Some(("net sales", true))),
            ("net", None),
            ("desc", None),
            ("net up", None),
        ];
        for (text, expected) in cases {
            let sort = Sort::parse(text);
            assert_eq!(
                sort.as_ref()
                    .map(|sort| (sort.column.as_str(), sort.descending)),
                expected,
                "{text}"
            );
        }
        let sort = Sort::parse("net sales DESC").unwrap();
        assert_eq!(sort.written(), "net sales desc");
    }

    #[test]
    fn measures_edits_between_names() {
        assert_eq!(distance("ys", "y"), 1);
        assert_eq!(distance("kinds", "kind"), 1);
        assert_eq!(distance("y_zeroo", "y_zero"), 1);
        assert_eq!(distance("colour", "x"), 6);
        assert_eq!(distance("", "kind"), 4);
    }
}
