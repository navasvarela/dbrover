//! What a request may change, from its method and path, and whether it may touch everything
//! (`docs/nosql.md`, N3).
//!
//! The path's endpoint is its first part starting with `_` (`_search` in
//! `/orders/_search`, `_cluster` in `/_cluster/health`); a path with none names an index.

use dbrover_document::Value;
use dbrover_sql::StatementKind;

use super::{Method, Request};

/// Endpoints that only read, whatever the method: searches and their relatives, the point in
/// time and scroll contexts a search reads through, and text analysis.
const READ_ENDPOINTS: &[&str] = &[
    "_analyze",
    "_async_search",
    "_count",
    "_eql",
    "_explain",
    "_field_caps",
    "_knn_search",
    "_mget",
    "_msearch",
    "_mtermvectors",
    "_pit",
    "_query",
    "_rank_eval",
    "_render",
    "_resolve",
    "_search",
    "_search_shards",
    "_terms_enum",
    "_termvectors",
    "_validate",
];

/// Endpoints that write documents.
const WRITE_ENDPOINTS: &[&str] = &[
    "_bulk",
    "_create",
    "_delete_by_query",
    "_doc",
    "_reindex",
    "_update",
    "_update_by_query",
];

/// Endpoints that create, change or delete indices and what describes them: mappings,
/// settings, aliases, data streams and templates.
const SCHEMA_ENDPOINTS: &[&str] = &[
    "_alias",
    "_aliases",
    "_block",
    "_clone",
    "_close",
    "_component_template",
    "_data_stream",
    "_index_template",
    "_mapping",
    "_mappings",
    "_open",
    "_rollover",
    "_settings",
    "_shrink",
    "_split",
    "_template",
];

/// The SQL endpoints, whose statement says what they do: Elasticsearch's, and OpenSearch's
/// SQL and PPL plugins under their current and their old prefixes.
const SQL_PREFIXES: &[&[&str]] = &[
    &["_sql"],
    &["_plugins", "_sql"],
    &["_plugins", "_ppl"],
    &["_opendistro", "_sql"],
    &["_opendistro", "_ppl"],
];

/// Statements of the SQL endpoints that only read.
const SQL_READS: &[&str] = &["describe", "desc", "explain", "select", "show", "with"];

/// What `request` may change.
pub(crate) fn kind(request: &Request) -> StatementKind {
    let segments = request.segments();
    if let Some(rest) = sql_endpoint(&segments) {
        return sql_kind(request, rest);
    }
    match request.method {
        Method::Get | Method::Head => StatementKind::Read,
        Method::Post | Method::Put => change_kind(&segments),
        Method::Delete => delete_kind(&segments),
    }
}

/// The parts after a SQL endpoint's own, when `segments` name one.
fn sql_endpoint<'a, 'b>(segments: &'a [&'b str]) -> Option<&'a [&'b str]> {
    SQL_PREFIXES
        .iter()
        .find_map(|prefix| segments.strip_prefix(*prefix))
}

/// A SQL statement reads unless it is one of the few that change data (OpenSearch's
/// `DELETE`); PPL only reads; closing a cursor and translating a statement read.
fn sql_kind(request: &Request, rest: &[&str]) -> StatementKind {
    if !rest.is_empty() {
        return StatementKind::Read;
    }
    let Some(Value::String(statement)) = request.body_field("query") else {
        return StatementKind::Read;
    };
    let is_ppl = request.segments().contains(&"_ppl");
    let first = statement
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if is_ppl || first.is_empty() || SQL_READS.contains(&first.as_str()) {
        StatementKind::Read
    } else {
        StatementKind::Write
    }
}

/// The endpoint of `segments`. An index's name never starts with `_`, but `_all`, every
/// index, stands where one does.
fn endpoint<'a>(segments: &[&'a str]) -> Option<&'a str> {
    segments
        .iter()
        .enumerate()
        .find(|(index, part)| part.starts_with('_') && !(*index == 0 && **part == "_all"))
        .map(|(_, part)| *part)
}

fn change_kind(segments: &[&str]) -> StatementKind {
    match endpoint(segments) {
        // `PUT /orders` creates an index.
        None if !segments.is_empty() => StatementKind::Schema,
        None => StatementKind::Admin,
        Some(name) => endpoint_kind(name),
    }
}

fn endpoint_kind(name: &str) -> StatementKind {
    if READ_ENDPOINTS.contains(&name) {
        StatementKind::Read
    } else if WRITE_ENDPOINTS.contains(&name) {
        StatementKind::Write
    } else if SCHEMA_ENDPOINTS.contains(&name) {
        StatementKind::Schema
    } else {
        StatementKind::Admin
    }
}

/// `DELETE` of an index changes the schema, of a document writes, and of a search's point in
/// time or scroll only lets it go.
fn delete_kind(segments: &[&str]) -> StatementKind {
    match endpoint(segments) {
        None if !segments.is_empty() => StatementKind::Schema,
        None => StatementKind::Admin,
        Some("_doc") => StatementKind::Write,
        Some(name) if READ_ENDPOINTS.contains(&name) => StatementKind::Read,
        Some(name) if SCHEMA_ENDPOINTS.contains(&name) => StatementKind::Schema,
        Some(_) => StatementKind::Admin,
    }
}

/// Why `request` may touch everything, for the warning before it runs.
pub(crate) fn hazard(request: &Request) -> Option<&'static str> {
    let segments = request.segments();
    match (request.method, endpoint(&segments)) {
        (Method::Post, Some("_delete_by_query")) if matches_everything(request) => Some(
            "_delete_by_query without a query, or with match_all, deletes every document it reaches.",
        ),
        (Method::Post, Some("_update_by_query")) if matches_everything(request) => Some(
            "_update_by_query without a query, or with match_all, changes every document it reaches.",
        ),
        (Method::Delete, None) if names_many(&segments) => Some(
            "DELETE of a pattern or _all deletes every index it matches, with their documents.",
        ),
        (Method::Delete, None) if !segments.is_empty() => {
            Some("DELETE of an index deletes it, with its documents and mappings.")
        }
        _ => None,
    }
}

/// Whether the request's query matches every document: none at all, or `match_all`.
fn matches_everything(request: &Request) -> bool {
    match request.body_field("query") {
        None => true,
        Some(Value::Object(fields)) => {
            matches!(fields.as_slice(), [(name, _)] if name == "match_all")
        }
        Some(_) => false,
    }
}

/// Whether the index part of the path may name several: a wildcard, a list, or `_all`.
fn names_many(segments: &[&str]) -> bool {
    segments
        .first()
        .is_some_and(|indices| *indices == "_all" || indices.contains(['*', ',']))
}
