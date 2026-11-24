//! A statement's result as the Statement Execution API describes it, and its values as text.
//!
//! In `JSON_ARRAY`, the format the console reads, every value is a JSON string, or `null` for
//! `NULL`, written by Databricks: DbRover shows them as they come (ADR 0004). How Databricks
//! writes each type isn't documented, which C6's live check is to see.

use dbrover_core::{ColumnInfo, Row};
use serde::Deserialize;
use serde_json::Value;

/// What a finished statement's answer says about its result.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Manifest {
    #[serde(default)]
    pub schema: Schema,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Schema {
    #[serde(default)]
    pub columns: Vec<Column>,
}

/// A column, as the manifest names it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Column {
    pub name: String,
    /// The type as Databricks writes it, with its parameters: `DECIMAL(10,2)`,
    /// `ARRAY<STRING>`, `TIMESTAMP_NTZ`.
    #[serde(default)]
    pub type_text: Option<String>,
    /// The type's name alone, which `type_text` says better; for a manifest without the text.
    #[serde(default)]
    pub type_name: Option<String>,
}

impl Column {
    pub(crate) fn info(&self) -> ColumnInfo {
        ColumnInfo {
            name: self.name.clone(),
            type_name: self.type_text.clone().or_else(|| self.type_name.clone()),
        }
    }
}

/// One chunk of a result, as the statement's answer or a chunk's own request gives it.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Chunk {
    #[serde(default)]
    pub external_links: Vec<ExternalLink>,
    /// Rows written in the answer itself, for `INLINE`, which C6 doesn't ask for but reads.
    #[serde(default)]
    pub data_array: Option<Vec<Vec<Value>>>,
    #[serde(default)]
    pub next_chunk_index: Option<u64>,
}

/// Where a chunk's rows are: an address signed for the statement's user, for 15 minutes at
/// most, and the headers its download must carry, which may hold a decryption key. Its
/// `Debug` shows neither.
#[derive(Clone, Deserialize)]
pub(crate) struct ExternalLink {
    pub external_link: String,
    #[serde(default)]
    pub http_headers: std::collections::BTreeMap<String, String>,
}

impl std::fmt::Debug for ExternalLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExternalLink { .. }")
    }
}

impl ExternalLink {
    pub(crate) fn headers(&self) -> Vec<(String, String)> {
        self.http_headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
}

/// `data`'s rows as text: a string as it is, `null` as `NULL`, anything else (which
/// `JSON_ARRAY` isn't documented to send) as its JSON.
pub(crate) fn rows(data: Vec<Vec<Value>>) -> Vec<Row> {
    data.into_iter()
        .map(|row| row.into_iter().map(text).collect())
        .collect()
}

fn text(value: Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_stays_apart_from_empty_text_and_values_stay_as_sent() {
        let data = serde_json::json!([
            ["1", "", null, "2026-01-02T03:04:05.123Z", "{\"a\":1}"],
            ["99999999999999999999", "Ada", "x", null, "[1,2]"]
        ]);
        let Value::Array(data) = data else {
            panic!("an array")
        };
        let data = data
            .into_iter()
            .map(|row| match row {
                Value::Array(values) => values,
                other => vec![other],
            })
            .collect();
        let rows = rows(data);
        assert_eq!(
            rows[0],
            [
                Some("1".to_owned()),
                Some(String::new()),
                None,
                Some("2026-01-02T03:04:05.123Z".to_owned()),
                Some("{\"a\":1}".to_owned())
            ]
        );
        assert_eq!(rows[1][0].as_deref(), Some("99999999999999999999"));
    }

    #[test]
    fn columns_are_named_with_their_type_text() {
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "format": "JSON_ARRAY",
            "schema": {"column_count": 2, "columns": [
                {"name": "total", "type_text": "DECIMAL(10,2)", "type_name": "DECIMAL",
                 "type_precision": 10, "type_scale": 2, "position": 0},
                {"name": "n", "type_name": "LONG", "position": 1}
            ]},
            "total_chunk_count": 1
        }))
        .expect("a manifest");
        let named: Vec<(String, Option<String>)> = manifest
            .schema
            .columns
            .iter()
            .map(|column| {
                let info = column.info();
                (info.name, info.type_name)
            })
            .collect();
        assert_eq!(
            named,
            [
                ("total".to_owned(), Some("DECIMAL(10,2)".to_owned())),
                ("n".to_owned(), Some("LONG".to_owned()))
            ]
        );
    }

    #[test]
    fn a_links_debug_shows_neither_its_address_nor_its_headers() {
        let link: ExternalLink = serde_json::from_value(serde_json::json!({
            "external_link": "https://bucket.s3.amazonaws.com/x?X-Amz-Signature=secret",
            "http_headers": {"x-amz-server-side-encryption-customer-key": "key"}
        }))
        .expect("a link");
        let printed = format!("{link:?}");
        assert!(
            !printed.contains("secret") && !printed.contains("key"),
            "{printed}"
        );
    }
}
