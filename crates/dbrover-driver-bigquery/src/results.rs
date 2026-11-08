//! A page of `jobs.getQueryResults` (or `tabledata.list`) as DbRover's columns and rows.
//!
//! BigQuery writes every value as a JSON string, `null` for `NULL`, an array as
//! `[{"v": …}, …]` and a record as `{"f": [{"v": …}, …]}` whose fields the schema names. The
//! console shows them as BigQuery's own console does: scalars as their text, arrays and records
//! as JSON, a `TIMESTAMP` as `2026-01-02 03:04:05.000000 UTC`.

use dbrover_core::{ColumnInfo, Row};
use jiff::Timestamp;
use serde::Deserialize;
use serde_json::{Map, Value};

/// A result's or a table's schema.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Schema {
    #[serde(default)]
    pub fields: Vec<Field>,
}

/// One column, or one field of a record.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Field {
    pub name: String,
    /// The REST API's legacy name (`INTEGER`, `FLOAT`, `BOOLEAN`, `RECORD`) or GoogleSQL's.
    #[serde(rename = "type")]
    pub kind: String,
    /// `NULLABLE`, `REQUIRED` or `REPEATED`; absent means `NULLABLE`.
    #[serde(default)]
    pub mode: Option<String>,
    /// A record's fields.
    #[serde(default)]
    pub fields: Vec<Field>,
}

impl Field {
    fn is_repeated(&self) -> bool {
        self.mode.as_deref() == Some("REPEATED")
    }

    /// The type as GoogleSQL writes it: `INT64`, `ARRAY<STRING>`, `STRUCT<a INT64, b STRING>`.
    pub(crate) fn type_name(&self) -> String {
        let element = match googlesql_name(&self.kind) {
            "STRUCT" => {
                let fields: Vec<String> = self
                    .fields
                    .iter()
                    .map(|field| format!("{} {}", field.name, field.type_name()))
                    .collect();
                format!("STRUCT<{}>", fields.join(", "))
            }
            name => name.to_owned(),
        };
        if self.is_repeated() {
            format!("ARRAY<{element}>")
        } else {
            element
        }
    }

    pub(crate) fn column(&self) -> ColumnInfo {
        ColumnInfo {
            name: self.name.clone(),
            type_name: Some(self.type_name()),
        }
    }
}

/// The REST API's legacy type names in GoogleSQL's spelling; others as they are.
fn googlesql_name(kind: &str) -> &str {
    match kind {
        "INTEGER" => "INT64",
        "FLOAT" => "FLOAT64",
        "BOOLEAN" => "BOOL",
        "RECORD" => "STRUCT",
        other => other,
    }
}

/// One row of the answer: a cell per column.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ApiRow {
    #[serde(default)]
    f: Vec<Cell>,
}

#[derive(Debug, Clone, Deserialize)]
struct Cell {
    #[serde(default)]
    v: Value,
}

/// The rows of a page, as text, in the schema's column order.
pub(crate) fn rows(schema: &Schema, rows: &[ApiRow]) -> Vec<Row> {
    rows.iter()
        .map(|row| {
            schema
                .fields
                .iter()
                .zip(
                    row.f
                        .iter()
                        .map(|cell| &cell.v)
                        .chain(std::iter::repeat(&Value::Null)),
                )
                .map(|(field, value)| text(field, value))
                .collect()
        })
        .collect()
}

/// A cell's text; `None` for `NULL`.
fn text(field: &Field, value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(scalar) if !field.is_repeated() => Some(scalar_text(field, scalar)),
        complex => Some(json(field, complex).to_string()),
    }
}

/// A scalar's text, which is BigQuery's own but for a `TIMESTAMP`.
fn scalar_text(field: &Field, scalar: &str) -> String {
    if googlesql_name(&field.kind) == "TIMESTAMP" {
        return timestamp(scalar).unwrap_or_else(|| scalar.to_owned());
    }
    scalar.to_owned()
}

/// A `TIMESTAMP` as BigQuery's console shows it, from microseconds since the epoch (what
/// `formatOptions.useInt64Timestamp` asks for) or from seconds with a fraction (BigQuery's
/// default, `1.767323045E9`, and the emulator's, `1767323045.000000`).
fn timestamp(scalar: &str) -> Option<String> {
    let micros = match scalar.parse::<i64>() {
        Ok(micros) => micros,
        Err(_) => {
            let seconds: f64 = scalar.parse().ok()?;
            // A float's seconds lose precision below a microsecond past 2^53 µs (285 years),
            // which BigQuery's own default form loses as well.
            #[allow(clippy::cast_possible_truncation)]
            let micros = (seconds * 1_000_000.0).round() as i64;
            micros
        }
    };
    let instant = Timestamp::from_microsecond(micros).ok()?;
    let fraction = micros.rem_euclid(1_000_000);
    Some(format!(
        "{}.{fraction:06} UTC",
        instant.strftime("%Y-%m-%d %H:%M:%S")
    ))
}

/// An array's or a record's value as JSON: numbers and booleans as JSON's own, the rest as
/// strings, records as objects with their fields' names.
fn json(field: &Field, value: &Value) -> Value {
    if field.is_repeated() {
        let element = Field {
            mode: None,
            ..field.clone()
        };
        let items = value.as_array().map_or(&[][..], Vec::as_slice);
        return Value::Array(
            items
                .iter()
                .map(|item| json(&element, item.get("v").unwrap_or(&Value::Null)))
                .collect(),
        );
    }
    match (googlesql_name(&field.kind), value) {
        (_, Value::Null) => Value::Null,
        ("STRUCT", record) => {
            let cells = record
                .get("f")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice);
            let mut object = Map::new();
            for (inner, cell) in field.fields.iter().zip(cells) {
                let value = cell.get("v").unwrap_or(&Value::Null);
                object.insert(inner.name.clone(), json(inner, value));
            }
            Value::Object(object)
        }
        ("INT64" | "FLOAT64" | "NUMERIC" | "BIGNUMERIC", Value::String(number)) => {
            serde_json::from_str::<serde_json::Number>(number)
                .map_or_else(|_| Value::String(number.clone()), Value::Number)
        }
        ("BOOL", Value::String(flag)) => match flag.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::String(flag.clone()),
        },
        ("TIMESTAMP", Value::String(scalar)) => {
            Value::String(timestamp(scalar).unwrap_or_else(|| scalar.clone()))
        }
        (_, other) => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(json: &str) -> Schema {
        serde_json::from_str(json).unwrap()
    }

    fn page(json: &str) -> Vec<ApiRow> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn values_read_as_bigquerys_console_shows_them() {
        let schema = schema(
            r#"{"fields": [
                {"name": "id", "type": "INTEGER", "mode": "REQUIRED"},
                {"name": "name", "type": "STRING"},
                {"name": "amount", "type": "NUMERIC"},
                {"name": "placed", "type": "TIMESTAMP"},
                {"name": "tags", "type": "STRING", "mode": "REPEATED"},
                {"name": "address", "type": "RECORD", "fields": [
                    {"name": "city", "type": "STRING"},
                    {"name": "floors", "type": "INTEGER", "mode": "REPEATED"},
                    {"name": "main", "type": "BOOLEAN"}]},
                {"name": "photo", "type": "BYTES"}
            ]}"#,
        );
        let rows = rows(
            &schema,
            &page(
                r#"[
                {"f": [{"v": "1"}, {"v": "Ada"}, {"v": "12.50"}, {"v": "1767323045123456"},
                       {"v": [{"v": "a"}, {"v": "b"}]},
                       {"v": {"f": [{"v": "Paris"}, {"v": [{"v": "1"}, {"v": "2"}]}, {"v": "true"}]}},
                       {"v": "YWI="}]},
                {"f": [{"v": "2"}, {"v": ""}, {"v": null}, {"v": "1.767323045E9"},
                       {"v": []}, {"v": null}, {"v": null}]}
            ]"#,
            ),
        );
        let text = |value: &str| Some(value.to_owned());
        assert_eq!(
            rows,
            vec![
                vec![
                    text("1"),
                    text("Ada"),
                    text("12.50"),
                    text("2026-01-02 03:04:05.123456 UTC"),
                    text(r#"["a","b"]"#),
                    text(r#"{"city":"Paris","floors":[1,2],"main":true}"#),
                    text("YWI="),
                ],
                vec![
                    text("2"),
                    text(""),
                    None,
                    text("2026-01-02 03:04:05.000000 UTC"),
                    text("[]"),
                    None,
                    None,
                ],
            ]
        );
    }

    #[test]
    fn columns_are_named_in_googlesql() {
        let schema = schema(
            r#"{"fields": [
                {"name": "n", "type": "INTEGER"},
                {"name": "xs", "type": "FLOAT", "mode": "REPEATED"},
                {"name": "s", "type": "RECORD", "mode": "REPEATED", "fields": [
                    {"name": "a", "type": "BOOLEAN"}, {"name": "b", "type": "DATE"}]},
                {"name": "j", "type": "JSON"}
            ]}"#,
        );
        let names: Vec<String> = schema.fields.iter().map(Field::type_name).collect();
        assert_eq!(
            names,
            [
                "INT64",
                "ARRAY<FLOAT64>",
                "ARRAY<STRUCT<a BOOL, b DATE>>",
                "JSON"
            ]
        );
    }

    #[test]
    fn timestamps_from_either_form_and_before_1970() {
        for (scalar, shown) in [
            ("0", "1970-01-01 00:00:00.000000 UTC"),
            ("-1", "1969-12-31 23:59:59.999999 UTC"),
            ("1767323045.000000", "2026-01-02 03:04:05.000000 UTC"),
            ("1.7673230455E9", "2026-01-02 03:04:05.500000 UTC"),
        ] {
            assert_eq!(timestamp(scalar).as_deref(), Some(shown), "{scalar}");
        }
        assert_eq!(timestamp("soon"), None);
    }
}
