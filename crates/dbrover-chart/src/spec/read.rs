//! Reading a spec into a [`ChartSpec`], with every problem it has: from a block's text, whose
//! problems are placed at their bytes, or from a report's `[[chart]]` table, whose problems are
//! placed at their keys. The keys are read by one [`Reader`], whichever TOML library read them.

use std::ops::Range;

use toml_edit::{Document, Item, Table, Value};

use super::{ChartSpec, Key, SPEC_TABLE, Sort, SpecProblem};
use crate::ChartKind;

/// A misspelt key is suggested when it is at most this many letters' edits from a key: `ys`
/// and `kinds` are one away. Three would suggest `x` for any key of up to three letters.
const MAX_SUGGESTION_DISTANCE: usize = 2;

/// A problem with a key of a report's `[[chart]]`: the key it is about, and what is wrong, as a
/// sentence without the chart it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyProblem {
    pub key: String,
    pub detail: String,
}

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
        let mut reader = Reader::default();
        let spec = document_spec(text, &document, &mut reader);
        let mut problems: Vec<SpecProblem> = reader
            .problems
            .into_iter()
            .map(|(span, detail)| SpecProblem::new(text, span, detail))
            .collect();
        if problems.is_empty() {
            Ok(spec)
        } else {
            problems.sort_by_key(|problem| problem.span.start);
            Err(problems)
        }
    }

    /// Reads a spec from the keys of a report's `[[chart]]` table, leaving `others`, the keys
    /// the report reads itself (`file`, `width`, …), to the report. A misspelt key is suggested
    /// from both sets.
    ///
    /// # Errors
    ///
    /// Every problem the keys have, by key: keys that are neither the spec's nor `others`, and
    /// values of the wrong kind.
    pub fn from_table(table: &toml::Table, others: &[&str]) -> Result<Self, Vec<KeyProblem>> {
        let mut reader = Reader::default();
        let mut spec = Self::default();
        for (name, value) in table {
            if others.contains(&name.as_str()) {
                continue;
            }
            let Some(key) = Key::named(name) else {
                reader.problem(name.clone(), unknown_key(name, others));
                continue;
            };
            reader.key(&mut spec, key, &TomlValue { value, key: name });
        }
        if reader.problems.is_empty() {
            Ok(spec)
        } else {
            Err(reader
                .problems
                .into_iter()
                .map(|(key, detail)| KeyProblem { key, detail })
                .collect())
        }
    }
}

/// The spec in a block's document: the keys of its `[chart]` table, and a problem for anything
/// beside it.
fn document_spec(
    text: &str,
    document: &Document<&str>,
    reader: &mut Reader<Range<usize>>,
) -> ChartSpec {
    let mut spec = ChartSpec::default();
    for (name, item) in document.iter() {
        match (name, item) {
            (SPEC_TABLE, Item::Table(table)) => spec = table_spec(text, table, reader),
            _ => {
                let span = key_span(text, document.as_table(), name, item);
                reader.problem(
                    span,
                    format!("a chart is the keys under `[{SPEC_TABLE}]`, not `{name}`"),
                );
            }
        }
    }
    spec
}

fn table_spec(text: &str, table: &Table, reader: &mut Reader<Range<usize>>) -> ChartSpec {
    let mut spec = ChartSpec::default();
    for (name, item) in table.iter() {
        let key_span = key_span(text, table, name, item);
        let Some(key) = Key::named(name) else {
            reader.problem(key_span, unknown_key(name, &[]));
            continue;
        };
        let value = EditValue {
            value: item.as_value(),
            span: item.span().unwrap_or(key_span),
            text,
        };
        reader.key(&mut spec, key, &value);
    }
    spec
}

fn key_span(text: &str, table: &Table, name: &str, item: &Item) -> Range<usize> {
    table
        .key(name)
        .and_then(toml_edit::Key::span)
        .or_else(|| item.span())
        .unwrap_or(0..text.len())
}

/// A key's value as the [`Reader`] checks it, and where a problem with it is placed.
trait KeyValue: Sized {
    type Place;

    fn place(&self) -> Self::Place;
    fn text(&self) -> Option<&str>;
    fn flag(&self) -> Option<bool>;
    fn integer(&self) -> Option<i64>;
    /// An array's items; `None` for a value that isn't an array.
    fn items(&self) -> Option<Vec<Self>>;
    /// The value as the file writes it, for "not …": `"area"`, `3`, or `a table`.
    fn written(&self) -> String;
}

/// A value of a block's document, with the bytes it was read from.
struct EditValue<'a> {
    /// `None` for a table.
    value: Option<&'a Value>,
    span: Range<usize>,
    text: &'a str,
}

impl KeyValue for EditValue<'_> {
    type Place = Range<usize>;

    fn place(&self) -> Range<usize> {
        self.span.clone()
    }

    fn text(&self) -> Option<&str> {
        self.value.and_then(Value::as_str)
    }

    fn flag(&self) -> Option<bool> {
        self.value.and_then(Value::as_bool)
    }

    fn integer(&self) -> Option<i64> {
        self.value.and_then(Value::as_integer)
    }

    fn items(&self) -> Option<Vec<Self>> {
        let Some(Value::Array(items)) = self.value else {
            return None;
        };
        Some(
            items
                .iter()
                .map(|item| EditValue {
                    value: Some(item),
                    span: item.span().unwrap_or_else(|| self.span.clone()),
                    text: self.text,
                })
                .collect(),
        )
    }

    fn written(&self) -> String {
        match self.value {
            Some(_) => self
                .text
                .get(self.span.clone())
                .map_or("", str::trim)
                .to_owned(),
            None => "a table".to_owned(),
        }
    }
}

/// A value of a report's `[[chart]]`, placed at its key.
struct TomlValue<'a> {
    value: &'a toml::Value,
    key: &'a str,
}

impl KeyValue for TomlValue<'_> {
    type Place = String;

    fn place(&self) -> String {
        self.key.to_owned()
    }

    fn text(&self) -> Option<&str> {
        self.value.as_str()
    }

    fn flag(&self) -> Option<bool> {
        self.value.as_bool()
    }

    fn integer(&self) -> Option<i64> {
        self.value.as_integer()
    }

    fn items(&self) -> Option<Vec<Self>> {
        let items = self.value.as_array()?;
        Some(
            items
                .iter()
                .map(|value| TomlValue {
                    value,
                    key: self.key,
                })
                .collect(),
        )
    }

    fn written(&self) -> String {
        match self.value {
            toml::Value::Table(_) => "a table".to_owned(),
            value => value.to_string(),
        }
    }
}

/// Reads a spec's keys, collecting a problem, placed by `P`, for each one it can't.
struct Reader<P> {
    problems: Vec<(P, String)>,
}

impl<P> Default for Reader<P> {
    fn default() -> Self {
        Self {
            problems: Vec::new(),
        }
    }
}

impl<P> Reader<P> {
    fn key<V: KeyValue<Place = P>>(&mut self, spec: &mut ChartSpec, key: Key, value: &V) {
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
            Key::YZero => spec.y_zero = self.flag(value, "`y_zero` is true or false"),
            Key::Stack => spec.stack = self.flag(value, "`stack` is true or false"),
            Key::Sort => spec.sort = self.sort(value),
            Key::Limit => spec.limit = self.limit(value),
        }
    }

    fn kind<V: KeyValue<Place = P>>(&mut self, value: &V) -> Option<ChartKind> {
        let kind = value.text().and_then(ChartKind::from_spec_name);
        if kind.is_none() {
            let names: Vec<&str> = ChartKind::ALL.map(ChartKind::spec_name).to_vec();
            let detail = format!("`kind` is {}", or_list(&names));
            self.wrong(value, &detail);
        }
        kind
    }

    fn text_value<V: KeyValue<Place = P>>(&mut self, value: &V, what: &str) -> Option<String> {
        let text = value.text().map(str::to_owned);
        if text.is_none() {
            self.wrong(value, what);
        }
        text
    }

    /// `y`: one column's name, or a list of them.
    fn columns<V: KeyValue<Place = P>>(&mut self, value: &V) -> Vec<String> {
        const WHAT: &str = "`y` is a column's name, or a list of them, in quotes";
        if let Some(name) = value.text() {
            return vec![name.to_owned()];
        }
        let Some(items) = value.items() else {
            self.wrong(value, WHAT);
            return Vec::new();
        };
        if items.is_empty() {
            self.problem(
                value.place(),
                "`y = []` names no column; leave `y` out to chart every column of numbers",
            );
            return Vec::new();
        }
        let mut names = Vec::new();
        for item in items {
            match item.text() {
                Some(name) => names.push(name.to_owned()),
                None => self.wrong(&item, WHAT),
            }
        }
        names
    }

    fn flag<V: KeyValue<Place = P>>(&mut self, value: &V, what: &str) -> bool {
        let flag = value.flag();
        if flag.is_none() {
            self.wrong(value, what);
        }
        flag.unwrap_or_default()
    }

    fn sort<V: KeyValue<Place = P>>(&mut self, value: &V) -> Option<Sort> {
        let Some(text) = value.text() else {
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
                value.written()
            );
            self.problem(value.place(), detail);
        }
        sort
    }

    fn limit<V: KeyValue<Place = P>>(&mut self, value: &V) -> Option<usize> {
        let limit = value
            .integer()
            .and_then(|rows| usize::try_from(rows).ok())
            .filter(|rows| *rows > 0);
        if limit.is_none() {
            self.wrong(value, "`limit` is a number of rows, 1 or more");
        }
        limit
    }

    /// A value of the wrong kind: what the key takes, and what it was given.
    fn wrong<V: KeyValue<Place = P>>(&mut self, value: &V, what: &str) {
        self.problem(value.place(), format!("{what}, not {}", value.written()));
    }

    fn problem(&mut self, place: P, detail: impl Into<String>) {
        self.problems.push((place, detail.into()));
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

/// "no key `ys`; did you mean `y`?", or the keys there are when none is near. `others` are keys
/// a report's `[[chart]]` takes beside the spec's.
fn unknown_key<'a>(name: &str, others: &[&'a str]) -> String {
    let lowered = name.to_lowercase();
    let known: Vec<&'a str> = Key::ALL
        .into_iter()
        .map(|key| -> &'a str { key.name() })
        .chain(others.iter().copied())
        .collect();
    let nearest = known
        .iter()
        .map(|known| (distance(&lowered, known), *known))
        .filter(|(distance, _)| *distance <= MAX_SUGGESTION_DISTANCE)
        .min_by_key(|(distance, _)| *distance);
    match nearest {
        Some((_, suggestion)) => format!("no key `{name}`; did you mean `{suggestion}`?"),
        None => {
            let names: Vec<String> = known.iter().map(|known| format!("`{known}`")).collect();
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
stack = true
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
                stack: true,
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
        let cases: [(&str, &[&str]); 18] = [
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
                   `series`, `stack`, `title`, `x_title`, `y_title`, `y_zero`, `sort`, `limit`",
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
                "stack = 1",
                &["chart, line 2: `stack` is true or false, not 1"],
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

    fn from_table(keys: &str) -> Result<ChartSpec, Vec<KeyProblem>> {
        let table: toml::Table = toml::from_str(keys).unwrap();
        ChartSpec::from_table(&table, &["file", "width"])
    }

    #[test]
    fn reads_a_report_charts_keys_and_leaves_the_reports_own() {
        let spec = from_table(
            "file = \"net.svg\"\nwidth = 600\nkind = \"bars\"\nx = \"category\"\ny = [\"net\"]\nlimit = 10",
        );
        assert_eq!(
            spec,
            Ok(ChartSpec {
                kind: Some(ChartKind::Bars),
                x: Some("category".to_owned()),
                y: vec!["net".to_owned()],
                limit: Some(10),
                ..ChartSpec::default()
            })
        );
    }

    #[test]
    fn places_a_report_charts_problems_at_their_keys() {
        let problem = |key: &str, detail: &str| KeyProblem {
            key: key.to_owned(),
            detail: detail.to_owned(),
        };
        let cases = [
            (
                "fiel = \"net.svg\"",
                problem("fiel", "no key `fiel`; did you mean `file`?"),
            ),
            (
                "kind = \"area\"",
                problem("kind", "`kind` is bars, lines or scatter, not \"area\""),
            ),
            (
                "y = [\"net\", 3]",
                problem(
                    "y",
                    "`y` is a column's name, or a list of them, in quotes, not 3",
                ),
            ),
            (
                "sort = \"net up\"",
                problem(
                    "sort",
                    "`sort = \"net up\"`: write a column, then `asc` or `desc`",
                ),
            ),
            (
                "x = { name = \"month\" }",
                problem("x", "`x` is a column's name, in quotes, not a table"),
            ),
        ];
        for (keys, expected) in cases {
            assert_eq!(from_table(keys), Err(vec![expected]), "{keys}");
        }
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
