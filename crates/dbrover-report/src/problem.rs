//! Where in a report file a problem is (`docs/report-editor.md`, RP8c), so an editor can
//! underline it. A problem's message is the sentence the tab, `dbrover report run` and MCP
//! show; its place is found once more, in the text, only when an editor asks.

use std::fmt;
use std::ops::Range;

use toml::de::{DeTable, DeValue};

/// A table of a report file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Table {
    /// The file's top level, for what belongs to none of its tables.
    File,
    Report,
    /// The `[[source]]` table at this position in the file, counted from 0.
    Source(usize),
    Combine,
    /// The `[[output]]` table at this position in the file, counted from 0.
    Output(usize),
    /// The `[[chart]]` table at this position in the file, counted from 0.
    Chart(usize),
}

/// Where a problem is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// A byte range of the text, for a TOML syntax error.
    Text(Range<usize>),
    /// A table, and the key the problem is about when one is known. A key the table doesn't
    /// have (one that is missing) places the problem on the table's header.
    Key { table: Table, key: Option<String> },
}

impl Place {
    pub fn table(table: Table) -> Self {
        Self::Key { table, key: None }
    }

    pub fn key(table: Table, key: impl Into<String>) -> Self {
        Self::Key {
            table,
            key: Some(key.into()),
        }
    }
}

/// A problem a report file has, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportProblem {
    pub message: String,
    pub place: Place,
}

impl ReportProblem {
    pub fn new(message: impl Into<String>, place: Place) -> Self {
        Self {
            message: message.into(),
            place,
        }
    }
}

impl fmt::Display for ReportProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The problems' messages, as callers that show no places list them.
pub(crate) fn messages(problems: Vec<ReportProblem>) -> Vec<String> {
    problems
        .into_iter()
        .map(|problem| problem.message)
        .collect()
}

/// Every TOML syntax error in `text`, each with the line it is on and its place. Empty when
/// the text is TOML.
pub(crate) fn syntax_problems(text: &str) -> Vec<ReportProblem> {
    let (_, errors) = DeTable::parse_recoverable(text);
    let mut spans: Vec<_> = errors
        .into_iter()
        .map(|error| (error.span().unwrap_or(0..0), error))
        .collect();
    // The parser doesn't report them in the file's order.
    spans.sort_by_key(|(span, _)| span.start);
    spans
        .into_iter()
        .map(|(span, error)| {
            let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
            ReportProblem::new(
                format!("line {line}: {}", error.message().trim()),
                Place::Text(span),
            )
        })
        .collect()
}

/// Finds places in one text, parsed once for all of them.
pub struct Locator<'a> {
    text: &'a str,
    table: DeTable<'a>,
}

impl<'a> Locator<'a> {
    /// Parses `text`, keeping what parses of a text with syntax errors.
    pub fn new(text: &'a str) -> Self {
        let (table, _) = DeTable::parse_recoverable(text);
        Self {
            text,
            table: table.into_inner(),
        }
    }

    /// The bytes to underline for `place`: never empty unless the text is.
    pub fn locate(&self, place: &Place) -> Range<usize> {
        let range = match place {
            Place::Text(range) => range.clone(),
            Place::Key { table, key } => self.key_range(table, key.as_deref()),
        };
        self.widened(range)
    }

    fn key_range(&self, table: &Table, key: Option<&str>) -> Range<usize> {
        let Some((header, keys)) = self.table_of(table) else {
            return self.first_line();
        };
        key.and_then(|key| keys.and_then(|keys| key_span(keys, key)))
            .unwrap_or(header)
    }

    /// A table's header (or inline table) and its keys, if the file has it.
    fn table_of(&self, table: &Table) -> Option<(Range<usize>, Option<&DeTable<'a>>)> {
        let (name, index) = match table {
            Table::File => return Some((self.first_line(), Some(&self.table))),
            Table::Report => ("report", None),
            Table::Combine => ("combine", None),
            Table::Source(index) => ("source", Some(*index)),
            Table::Output(index) => ("output", Some(*index)),
            Table::Chart(index) => ("chart", Some(*index)),
        };
        let (_, value) = self
            .table
            .iter()
            .find(|(key, _)| key.get_ref().as_ref() == name)?;
        let value = match (index, value.get_ref()) {
            (Some(index), DeValue::Array(items)) => items.get(index)?,
            (Some(_), _) => return None,
            (None, _) => value,
        };
        let keys = match value.get_ref() {
            DeValue::Table(keys) => Some(keys),
            _ => None,
        };
        Some((value.span(), keys))
    }

    fn first_line(&self) -> Range<usize> {
        0..self.text.find('\n').unwrap_or(self.text.len())
    }

    /// `range`, or when it is empty a character to underline: the one at its start, or the one
    /// before when that is a line break or the end, which an underline wouldn't show.
    fn widened(&self, range: Range<usize>) -> Range<usize> {
        let start = range.start.min(self.text.len());
        if range.end > start {
            return start..range.end.min(self.text.len());
        }
        let after = self.text[start..].chars().next().filter(|c| *c != '\n');
        let before = self.text[..start]
            .chars()
            .next_back()
            .filter(|c| *c != '\n');
        match (after, before) {
            (Some(after), _) => start..start + after.len_utf8(),
            (None, Some(before)) => start - before.len_utf8()..start,
            (None, None) => start..start + usize::from(start < self.text.len()),
        }
    }
}

fn key_span(keys: &DeTable<'_>, wanted: &str) -> Option<Range<usize>> {
    keys.iter()
        .find(|(key, _)| key.get_ref().as_ref() == wanted)
        .map(|(key, _)| key.span())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
[report]
name = \"Sales\"

[[source]]
id = \"a\"
query = \"select 1\"

[[source]]
id = \"b\"

[combine]
query = \"select 2\"

[[output]]
format = \"csv\"
sheet = 2
";

    #[test]
    fn finds_tables_and_their_keys() {
        let locator = Locator::new(FILE);
        let text = |place: Place| &FILE[locator.locate(&place)];
        let cases = [
            (Place::table(Table::Report), "[report]"),
            (Place::key(Table::Report, "name"), "name"),
            (Place::table(Table::Source(1)), "[[source]]"),
            (Place::key(Table::Source(1), "id"), "id"),
            (Place::key(Table::Source(0), "query"), "query"),
            (Place::key(Table::Combine, "query"), "query"),
            (Place::key(Table::Output(0), "sheet"), "sheet"),
            // A key the table doesn't have underlines its header.
            (Place::key(Table::Source(1), "query"), "[[source]]"),
            (Place::key(Table::File, "output"), "output"),
            (Place::table(Table::File), "[report]"),
            // A table the file doesn't have underlines the first line.
            (Place::table(Table::Output(3)), "[report]"),
        ];
        for (place, expected) in cases {
            assert_eq!(text(place.clone()), expected, "{place:?}");
        }
        let second_source = FILE.match_indices("[[source]]").nth(1).map(|(at, _)| at);
        assert_eq!(
            Some(locator.locate(&Place::table(Table::Source(1))).start),
            second_source
        );
    }

    #[test]
    fn lists_every_syntax_error_with_its_line() {
        let text = "[report]\nname = \nother = 1\n[x\n";
        let problems = syntax_problems(text);
        let messages: Vec<&str> = problems
            .iter()
            .map(|problem| problem.message.as_str())
            .collect();
        assert_eq!(problems.len(), 2, "{messages:?}");
        assert!(messages[0].starts_with("line 2: "), "{messages:?}");
        assert!(messages[1].starts_with("line 4: "), "{messages:?}");
        assert!(syntax_problems(FILE).is_empty());
    }

    #[test]
    fn widens_an_empty_place_to_a_character() {
        let locator = Locator::new("ab\n\nc");
        assert_eq!(locator.locate(&Place::Text(1..1)), 1..2);
        assert_eq!(locator.locate(&Place::Text(2..2)), 1..2, "the line break's");
        assert_eq!(locator.locate(&Place::Text(3..3)), 3..4, "an empty line's");
        assert_eq!(locator.locate(&Place::Text(5..5)), 4..5, "the end's");
        assert_eq!(Locator::new("").locate(&Place::Text(0..0)), 0..0);
    }
}
