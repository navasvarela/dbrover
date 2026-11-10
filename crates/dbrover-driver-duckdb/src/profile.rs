//! The profiler's tree of the last statement, as `duckdb-rs` hands it over (each node a map of
//! metric names to text), written as JSON in the shape of DuckDB's own JSON profile, which
//! `dbrover-plan` reads (`docs/reports-at-scale.md`, RS2b): the metrics' names in lower case,
//! numbers as numbers, the operator's extra information as an object whose values of several
//! lines are lists of them, and each node's operators under `children`.

use duckdb::profiling::ProfilingInfo;
use serde_json::{Map, Number, Value};

/// The metric holding an operator's extra information, a map DuckDB's C API writes as text.
const EXTRA_INFO: &str = "EXTRA_INFO";

/// `info` as DuckDB's JSON profile writes it; `None` when it profiled no operator, as for a
/// statement that isn't a query.
pub(crate) fn to_json(info: &ProfilingInfo) -> Option<String> {
    if info.children.is_empty() {
        return None;
    }
    Some(node(info).to_string())
}

fn node(info: &ProfilingInfo) -> Value {
    let mut object = Map::new();
    // In name order, so the same profile always writes the same text.
    let mut metrics: Vec<(&String, &String)> = info.metrics.iter().collect();
    metrics.sort();
    for (name, value) in metrics {
        let value = if name == EXTRA_INFO {
            Value::Object(extra_info(value))
        } else {
            number_or_text(value)
        };
        object.insert(name.to_lowercase(), value);
    }
    let children = info.children.iter().map(node).collect();
    object.insert("children".to_owned(), Value::Array(children));
    Value::Object(object)
}

fn number_or_text(value: &str) -> Value {
    value
        .parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .map_or_else(|| Value::String(value.to_owned()), Value::Number)
}

/// The extra information as DuckDB writes a `MAP(VARCHAR, VARCHAR)` as text:
/// `{Table=orders, Filters='a=1', Projections=a\nb}`. A key or value holding one of DuckDB's
/// special characters (`=`, `,`, quotes, brackets) is quoted with `'`, a `'` or `\` in it
/// escaped with `\`. A value of several lines becomes a list of its lines.
fn extra_info(text: &str) -> Map<String, Value> {
    let inner = text
        .trim()
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or(text);
    let mut entries = Map::new();
    let mut reader = MapText {
        characters: inner.chars().peekable(),
    };
    while let Some(key) = reader.token(&['=']) {
        let value = reader.token(&[',']).unwrap_or_default();
        let lines: Vec<&str> = value.split('\n').collect();
        let value = match lines.as_slice() {
            [line] => Value::String((*line).to_owned()),
            lines => Value::Array(
                lines
                    .iter()
                    .map(|line| Value::String((*line).to_owned()))
                    .collect(),
            ),
        };
        entries.insert(key, value);
    }
    entries
}

/// The text of a map, read a key or value at a time.
struct MapText<'a> {
    characters: std::iter::Peekable<std::str::Chars<'a>>,
}

impl MapText<'_> {
    /// The next key or value, up to the first of `ends` outside quotes, which is passed over;
    /// `None` once the text is read.
    fn token(&mut self, ends: &[char]) -> Option<String> {
        while self
            .characters
            .next_if(|character| *character == ' ')
            .is_some()
        {}
        self.characters.peek()?;
        let mut token = String::new();
        let mut quoted = false;
        while let Some(character) = self.characters.next() {
            match character {
                '\'' => quoted = !quoted,
                '\\' if quoted => {
                    if let Some(escaped) = self.characters.next() {
                        token.push(escaped);
                    }
                }
                end if !quoted && ends.contains(&end) => break,
                other => token.push(other),
            }
        }
        Some(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_extra_information_with_quoted_and_several_line_values() {
        let entries = extra_info(
            "{Table=orders, Type=Sequential Scan, Projections=region\namount, \
             Filters='region=''EU'' AND note=\\'a, b\\'', Estimated Cardinality=1000}",
        );
        assert_eq!(entries.get("Table"), Some(&Value::from("orders")));
        assert_eq!(entries.get("Type"), Some(&Value::from("Sequential Scan")));
        assert_eq!(
            entries.get("Projections"),
            Some(&Value::from(vec!["region", "amount"]))
        );
        assert_eq!(
            entries.get("Filters"),
            Some(&Value::from("region=EU AND note='a, b'"))
        );
        assert_eq!(
            entries.get("Estimated Cardinality"),
            Some(&Value::from("1000"))
        );
    }

    #[test]
    fn reads_no_extra_information_from_an_empty_map() {
        assert!(extra_info("{}").is_empty());
    }
}
