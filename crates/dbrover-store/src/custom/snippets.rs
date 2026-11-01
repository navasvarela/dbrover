//! `snippets.toml`: text the console's completion offers, with fields to Tab through.

use std::ops::Range;

use dbrover_core::DriverKind;
use serde::Deserialize;

use super::{Checked, drivers, entries, entry_label, serde_problem};

/// One snippet from `snippets.toml`, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    /// What completion matches the word being typed against.
    pub trigger: String,
    pub description: Option<String>,
    pub body: SnippetBody,
    /// `None` for every driver.
    pub drivers: Option<Vec<DriverKind>>,
}

impl Snippet {
    pub fn runs_on(&self, driver: DriverKind) -> bool {
        self.drivers
            .as_ref()
            .is_none_or(|drivers| drivers.contains(&driver))
    }
}

/// A body in the syntax other editors use: `${1:default}` or `$1` for a field, `$0` for where
/// the cursor ends, and `\$`, `\}` and `\\` for the characters themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetBody {
    /// The text as inserted, each field holding its default.
    text: String,
    /// The fields' byte ranges in `text`, in the order Tab visits them: `$1`, `$2`, … then
    /// `$0`, which is the end of the text when the body doesn't place it.
    stops: Vec<Range<usize>>,
}

impl SnippetBody {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Never empty: the last stop is where the cursor ends.
    pub fn stops(&self) -> &[Range<usize>] {
        &self.stops
    }

    /// Reads a body.
    ///
    /// # Errors
    ///
    /// Why the body can't be used, as a sentence for the problems list.
    pub fn parse(body: &str) -> Result<Self, String> {
        let mut text = String::new();
        let mut fields: Vec<(u32, Range<usize>)> = Vec::new();
        let mut chars = body.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            match c {
                '\\' if chars
                    .peek()
                    .is_some_and(|&(_, next)| matches!(next, '$' | '}' | '\\')) =>
                {
                    if let Some((_, escaped)) = chars.next() {
                        text.push(escaped);
                    }
                }
                '$' => match chars.peek().map(|&(_, next)| next) {
                    Some(digit) if digit.is_ascii_digit() => {
                        let number = take_number(&mut chars);
                        fields.push((number, text.len()..text.len()));
                    }
                    Some('{') => {
                        chars.next();
                        let field = braced_field(&mut chars, &mut text, &body[at..])?;
                        fields.push(field);
                    }
                    _ => text.push('$'),
                },
                _ => text.push(c),
            }
        }
        stops(text, fields)
    }
}

type Chars<'a> = std::iter::Peekable<std::str::CharIndices<'a>>;

fn take_number(chars: &mut Chars<'_>) -> u32 {
    let mut number: u32 = 0;
    while let Some(&(_, digit)) = chars.peek() {
        let Some(value) = digit.to_digit(10) else {
            break;
        };
        number = number.saturating_mul(10).saturating_add(value);
        chars.next();
    }
    number
}

/// `${1}` or `${1:default}`, after its `${`: writes the default to `text` and returns the
/// field's number and range.
fn braced_field(
    chars: &mut Chars<'_>,
    text: &mut String,
    written: &str,
) -> Result<(u32, Range<usize>), String> {
    let shown = field_text(written);
    if !chars.peek().is_some_and(|&(_, c)| c.is_ascii_digit()) {
        return Err(format!(
            "{shown:?}: only numbered fields are supported, as ${{1:default}}"
        ));
    }
    let number = take_number(chars);
    let start = text.len();
    match chars.next().map(|(_, c)| c) {
        Some('}') => return Ok((number, start..start)),
        Some(':') => {}
        _ => {
            return Err(format!(
                "{shown:?}: a field is written ${{1}} or ${{1:default}}"
            ));
        }
    }
    while let Some((_, c)) = chars.next() {
        match c {
            '}' => return Ok((number, start..text.len())),
            '\\' if chars
                .peek()
                .is_some_and(|&(_, next)| matches!(next, '$' | '}' | '\\')) =>
            {
                if let Some((_, escaped)) = chars.next() {
                    text.push(escaped);
                }
            }
            '$' if chars
                .peek()
                .is_some_and(|&(_, next)| next == '{' || next.is_ascii_digit()) =>
            {
                return Err(format!("{shown:?}: fields inside fields aren't supported"));
            }
            _ => text.push(c),
        }
    }
    Err(format!("{shown:?}: the field is never closed with }}"))
}

/// The field as written, up to its first `}`, for problems; a few words of it if that is long.
fn field_text(written: &str) -> String {
    const SHOWN_CHARS: usize = 24;
    let field = written.find('}').map_or(written, |end| &written[..=end]);
    if field.chars().count() <= SHOWN_CHARS {
        field.to_owned()
    } else {
        let start: String = field.chars().take(SHOWN_CHARS).collect();
        format!("{start}…")
    }
}

/// The fields in Tab order, `$0` last (or the end of the text).
fn stops(text: String, mut fields: Vec<(u32, Range<usize>)>) -> Result<SnippetBody, String> {
    let mut numbers: Vec<u32> = fields.iter().map(|(number, _)| *number).collect();
    numbers.sort_unstable();
    if let Some(pair) = numbers.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(format!(
            "${} is used twice; each field needs its own number",
            pair[0]
        ));
    }
    // Zero sorts last: it is where the cursor ends.
    fields.sort_by_key(|(number, _)| number.checked_sub(1).unwrap_or(u32::MAX));
    let mut stops: Vec<Range<usize>> = fields.into_iter().map(|(_, range)| range).collect();
    if numbers.first() != Some(&0) {
        stops.push(text.len()..text.len());
    }
    Ok(SnippetBody { text, stops })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSnippet {
    trigger: String,
    #[serde(default)]
    description: Option<String>,
    body: String,
    #[serde(default)]
    drivers: Option<Vec<String>>,
}

/// Checks the text of `snippets.toml`.
pub fn parse_snippets(text: &str) -> Checked<Vec<Snippet>> {
    let mut checked = Checked::<Vec<Snippet>>::default();
    for (index, entry) in entries(text, "snippet", &mut checked.problems)
        .into_iter()
        .enumerate()
    {
        let label = entry_label("snippet", &entry, "trigger", index);
        let snippet = entry
            .try_into::<RawSnippet>()
            .map_err(|error| serde_problem(&error))
            .and_then(check);
        match snippet {
            Ok(snippet) => checked.value.push(snippet),
            Err(problem) => checked.problems.push(format!("{label}: {problem}")),
        }
    }
    checked
}

fn check(raw: RawSnippet) -> Result<Snippet, String> {
    if raw.trigger.is_empty() {
        return Err("the trigger is empty".to_owned());
    }
    if !raw.trigger.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err("a trigger may only have letters, digits and \"_\"".to_owned());
    }
    if raw.body.is_empty() {
        return Err("the body is empty".to_owned());
    }
    Ok(Snippet {
        trigger: raw.trigger,
        description: raw.description.filter(|text| !text.trim().is_empty()),
        body: SnippetBody::parse(&raw.body)?,
        drivers: drivers(raw.drivers)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(body: &SnippetBody) -> Vec<&str> {
        body.stops()
            .iter()
            .map(|range| &body.text()[range.clone()])
            .collect()
    }

    #[test]
    fn reads_fields_in_tab_order_with_the_end_last() {
        let body =
            SnippetBody::parse("SELECT ${1:*}\nFROM ${2:table}\nWHERE ${3:condition};").unwrap();
        assert_eq!(body.text(), "SELECT *\nFROM table\nWHERE condition;");
        assert_eq!(fields(&body), ["*", "table", "condition", ""]);
        assert_eq!(
            body.stops().last(),
            Some(&(body.text().len()..body.text().len()))
        );

        let body = SnippetBody::parse("$0 FROM ${2} WHERE $1").unwrap();
        assert_eq!(body.text(), " FROM  WHERE ");
        assert_eq!(body.stops(), [13..13, 6..6, 0..0]);
    }

    #[test]
    fn keeps_dollars_that_start_no_field_and_reads_escapes() {
        let body =
            SnippetBody::parse(r"CREATE FUNCTION f() AS $$ SELECT \$1 $$; ${1:a\}b}").unwrap();
        assert_eq!(body.text(), "CREATE FUNCTION f() AS $$ SELECT $1 $$; a}b");
        assert_eq!(fields(&body), ["a}b", ""]);
    }

    #[test]
    fn says_why_a_body_cant_be_used() {
        let cases = [
            (
                "${TM_SELECTED_TEXT}",
                "\"${TM_SELECTED_TEXT}\": only numbered fields are supported, as ${1:default}",
            ),
            (
                "${1|a,b|}",
                "\"${1|a,b|}\": a field is written ${1} or ${1:default}",
            ),
            (
                "${1:a ${2:b}}",
                "\"${1:a ${2:b}\": fields inside fields aren't supported",
            ),
            (
                "${1:never closed",
                "\"${1:never closed\": the field is never closed with }",
            ),
            (
                "$1 and $1",
                "$1 is used twice; each field needs its own number",
            ),
        ];
        for (body, problem) in cases {
            assert_eq!(SnippetBody::parse(body), Err(problem.to_owned()), "{body}");
        }
    }

    #[test]
    fn skips_and_names_each_broken_snippet() {
        let cases = [
            (
                "[[snippet]]\nbody = \"x\"\n",
                "snippet 1: missing field `trigger`",
            ),
            (
                "[[snippet]]\ntrigger = \"\"\nbody = \"x\"\n",
                "snippet \"\": the trigger is empty",
            ),
            (
                "[[snippet]]\ntrigger = \"a b\"\nbody = \"x\"\n",
                "snippet \"a b\": a trigger may only have letters, digits and \"_\"",
            ),
            (
                "[[snippet]]\ntrigger = \"sel\"\nbody = \"\"\n",
                "snippet \"sel\": the body is empty",
            ),
            (
                "[[snippet]]\ntrigger = \"sel\"\nbody = \"$1 $1\"\n",
                "snippet \"sel\": $1 is used twice; each field needs its own number",
            ),
            (
                "[[snippet]]\ntrigger = \"sel\"\nbody = \"x\"\ndrivers = [\"pg\"]\n",
                "snippet \"sel\": \"pg\" is not a driver; the drivers are postgres, sqlite, \
                 mysql, sqlserver, duckdb, clickhouse, redis, mongodb, elasticsearch, \
                 cassandra, dynamodb",
            ),
            (
                "[[snippet]]\ntrigger = \"sel\"\nbody = \"x\"\nprefix = \"s\"\n",
                "snippet \"sel\": unknown field `prefix`, expected one of `trigger`, \
                 `description`, `body`, `drivers`",
            ),
        ];
        for (text, problem) in cases {
            let checked = parse_snippets(text);
            assert_eq!(checked.problems, [problem], "{text}");
            assert!(checked.value.is_empty(), "{text}");
        }
    }

    #[test]
    fn reads_the_plans_example() {
        let checked = parse_snippets(
            r#"
            [[snippet]]
            trigger = "sel"
            description = "Select with a filter"
            body = "SELECT ${1:*}\nFROM ${2:table}\nWHERE ${3:condition};"
            drivers = ["postgres"]
            "#,
        );
        assert!(checked.problems.is_empty(), "{:?}", checked.problems);
        let [snippet] = checked.value.as_slice() else {
            panic!("{:?}", checked.value);
        };
        assert_eq!(snippet.trigger, "sel");
        assert!(snippet.runs_on(DriverKind::Postgres));
        assert!(!snippet.runs_on(DriverKind::Sqlite));
    }
}
