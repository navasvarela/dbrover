//! Trino's client protocol: the JSON each answer carries ([`QueryResults`]), and the session a
//! connection keeps between statements in headers ([`Session`]), as Trino's client protocol
//! documentation and its own `StatementClientV1` describe them.

use std::collections::BTreeMap;

use dbrover_core::{ByteCount, Cell, ColumnInfo, JobProgress, Row};
use hyper::header::HeaderMap;
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::http::Headers;

/// One answer to a statement's `POST` or to a `GET` of its `nextUri`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueryResults {
    /// Where the next part of the answer is; `None` once the query is over.
    pub next_uri: Option<String>,
    /// Known once the query has been analysed; repeated on every answer after.
    pub columns: Option<Vec<Column>>,
    /// Rows, each value as the raw JSON Trino wrote.
    pub data: Option<Vec<Vec<Box<RawValue>>>>,
    pub stats: Option<Stats>,
    pub error: Option<QueryError>,
    /// What a statement without a result did (`INSERT`, `CREATE TABLE`, `SET SESSION`).
    pub update_type: Option<String>,
    /// The rows an `INSERT`, `UPDATE`, `DELETE` or `CREATE TABLE … AS` changed.
    pub update_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Stats {
    pub processed_rows: Option<u64>,
    pub processed_bytes: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueryError {
    pub message: String,
    /// `SYNTAX_ERROR`, `TABLE_NOT_FOUND`, `USER_CANCELED`, …
    pub error_name: Option<String>,
    pub error_location: Option<Location>,
}

/// Where in the statement an error is, both counted from 1.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Location {
    pub line_number: usize,
    pub column_number: usize,
}

impl QueryResults {
    pub(crate) fn columns(&self) -> Option<Vec<ColumnInfo>> {
        self.columns.as_ref().map(|columns| {
            columns
                .iter()
                .map(|column| ColumnInfo {
                    name: column.name.clone(),
                    type_name: Some(column.type_name.clone()),
                })
                .collect()
        })
    }

    /// The rows of this answer, each value as text.
    ///
    /// # Errors
    ///
    /// When a string value isn't valid JSON, which Trino never writes.
    pub(crate) fn rows(&self) -> Result<Vec<Row>, serde_json::Error> {
        self.data
            .iter()
            .flatten()
            .map(|row| row.iter().map(|value| cell(value)).collect())
            .collect()
    }

    /// What the query had read when the coordinator answered.
    pub(crate) fn progress(&self) -> Option<JobProgress> {
        let stats = self.stats.as_ref()?;
        Some(JobProgress {
            rows_read: stats.processed_rows,
            bytes_read: stats.processed_bytes.map(ByteCount),
        })
    }
}

/// A value as text: strings unquoted, `null` as SQL `NULL`, and anything else (numbers,
/// booleans, arrays, maps, rows) as the JSON Trino wrote, so a `bigint` or a `double` keeps
/// every digit.
fn cell(value: &RawValue) -> Result<Cell, serde_json::Error> {
    let text = value.get();
    Ok(match text {
        "null" => None,
        quoted if quoted.starts_with('"') => Some(serde_json::from_str(quoted)?),
        other => Some(other.to_owned()),
    })
}

/// What the coordinator keeps of a client between its statements, which the client sends
/// with each one and the coordinator changes through its answers' headers: the catalog and
/// schema (`USE`), session properties (`SET SESSION`), prepared statements (`PREPARE`) and the
/// open transaction (`START TRANSACTION`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Session {
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub properties: BTreeMap<String, String>,
    pub prepared: BTreeMap<String, String>,
    pub transaction: Option<String>,
}

impl Session {
    /// The headers a statement is sent with.
    pub(crate) fn request_headers(&self, names: Headers) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        if let Some(catalog) = &self.catalog {
            headers.push((names.name("Catalog"), catalog.clone()));
        }
        if let Some(schema) = &self.schema {
            headers.push((names.name("Schema"), schema.clone()));
        }
        for (name, value) in &self.properties {
            headers.push((names.name("Session"), format!("{name}={}", encode(value))));
        }
        for (name, statement) in &self.prepared {
            headers.push((
                names.name("Prepared-Statement"),
                format!("{}={}", encode(name), encode(statement)),
            ));
        }
        // `NONE` asks for a transaction of each statement's own, as every client sends it.
        headers.push((
            names.name("Transaction-Id"),
            self.transaction
                .clone()
                .unwrap_or_else(|| "NONE".to_owned()),
        ));
        headers
    }

    /// Takes what an answer's headers change.
    pub(crate) fn apply(&mut self, headers: &HeaderMap, names: Headers) {
        let values = |name: &str| {
            headers
                .get_all(names.name(name))
                .iter()
                .filter_map(|value| value.to_str().ok())
                .map(str::to_owned)
                .collect::<Vec<String>>()
        };
        if let Some(catalog) = values("Set-Catalog").pop() {
            self.catalog = Some(catalog);
        }
        if let Some(schema) = values("Set-Schema").pop() {
            self.schema = Some(schema);
        }
        for pair in values("Set-Session") {
            if let Some((name, value)) = pair.split_once('=') {
                self.properties
                    .insert(name.trim().to_owned(), decode(value));
            }
        }
        for name in values("Clear-Session") {
            self.properties.remove(name.trim());
        }
        for pair in values("Added-Prepare") {
            if let Some((name, statement)) = pair.split_once('=') {
                self.prepared.insert(decode(name), decode(statement));
            }
        }
        for name in values("Deallocated-Prepare") {
            self.prepared.remove(&decode(&name));
        }
        if let Some(transaction) = values("Started-Transaction-Id").pop() {
            self.transaction = Some(transaction);
        }
        if !values("Clear-Transaction-Id").is_empty() {
            self.transaction = None;
        }
    }
}

/// A header value as Trino's client encodes it: as a form value, a space as `+`.
fn encode(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

fn decode(text: &str) -> String {
    form_urlencoded::parse(format!("x={text}").as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use dbrover_core::TrinoProtocol;
    use hyper::header::HeaderValue;

    use super::*;

    #[test]
    fn values_are_text_with_every_digit() {
        let answer: QueryResults = serde_json::from_str(
            r#"{"id": "q", "columns": [{"name": "n", "type": "bigint"}],
                "data": [[9223372036854775807, "1.50", null, "", [1, 2], {"a": true}, "NaN", false]],
                "stats": {"state": "RUNNING", "processedRows": 5, "processedBytes": 1200}}"#,
        )
        .unwrap();
        let text = |value: &str| Some(value.to_owned());
        assert_eq!(
            answer.rows().unwrap(),
            [vec![
                text("9223372036854775807"),
                text("1.50"),
                None,
                text(""),
                text("[1, 2]"),
                text("{\"a\": true}"),
                text("NaN"),
                text("false"),
            ]]
        );
        assert_eq!(
            answer.progress(),
            Some(JobProgress {
                rows_read: Some(5),
                bytes_read: Some(ByteCount(1200)),
            })
        );
    }

    #[test]
    fn the_session_follows_the_answers_headers() {
        let names = Headers::of(TrinoProtocol::Trino);
        let mut session = Session::default();
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("x-trino-set-catalog", "memory"),
            ("x-trino-set-schema", "default"),
            ("x-trino-set-session", "query_max_run_time=1h"),
            ("x-trino-set-session", "join_distribution_type=BROADCAST"),
            (
                "x-trino-added-prepare",
                "q=SELECT+*+FROM+t+WHERE+id+%3D+%3F",
            ),
            ("x-trino-started-transaction-id", "tx-1"),
        ] {
            headers.append(name, HeaderValue::from_static(value));
        }
        session.apply(&headers, names);
        assert_eq!(session.catalog.as_deref(), Some("memory"));
        assert_eq!(session.properties.len(), 2);
        assert_eq!(
            session.prepared.get("q").map(String::as_str),
            Some("SELECT * FROM t WHERE id = ?")
        );
        let sent = session.request_headers(names);
        assert!(sent.contains(&("x-trino-catalog".to_owned(), "memory".to_owned())));
        assert!(sent.contains(&(
            "x-trino-session".to_owned(),
            "query_max_run_time=1h".to_owned()
        )));
        assert!(sent.contains(&(
            "x-trino-prepared-statement".to_owned(),
            "q=SELECT+*+FROM+t+WHERE+id+%3D+%3F".to_owned()
        )));
        assert!(sent.contains(&("x-trino-transaction-id".to_owned(), "tx-1".to_owned())));

        let mut cleared = HeaderMap::new();
        for (name, value) in [
            ("x-trino-clear-session", "query_max_run_time"),
            ("x-trino-deallocated-prepare", "q"),
            ("x-trino-clear-transaction-id", "true"),
        ] {
            cleared.append(name, HeaderValue::from_static(value));
        }
        session.apply(&cleared, names);
        assert_eq!(
            session.properties.keys().collect::<Vec<_>>(),
            ["join_distribution_type"]
        );
        assert!(session.prepared.is_empty());
        let sent = session.request_headers(names);
        assert!(sent.contains(&("x-trino-transaction-id".to_owned(), "NONE".to_owned())));
    }
}
