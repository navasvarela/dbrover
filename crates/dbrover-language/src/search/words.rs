//! The words completion offers in an Elasticsearch or OpenSearch console, each with what it
//! shows beside it, and the ones the editor highlights.

use std::sync::LazyLock;

use super::Method;

/// The endpoints most requests use, each with how it is called.
const ENDPOINTS: &[(&str, &str)] = &[
    ("_aliases", "POST /_aliases"),
    ("_analyze", "GET /<index>/_analyze"),
    ("_bulk", "POST /<index>/_bulk, then NDJSON"),
    ("_cat", "GET /_cat/indices"),
    ("_cluster", "GET /_cluster/health"),
    ("_count", "GET /<index>/_count"),
    ("_create", "PUT /<index>/_create/<id>"),
    ("_data_stream", "GET /_data_stream/<name>"),
    ("_delete_by_query", "POST /<index>/_delete_by_query"),
    ("_doc", "GET|PUT|DELETE /<index>/_doc/<id>"),
    ("_explain", "GET /<index>/_explain/<id>"),
    ("_field_caps", "GET /<index>/_field_caps?fields=*"),
    ("_index_template", "GET|PUT /_index_template/<name>"),
    ("_mapping", "GET|PUT /<index>/_mapping"),
    ("_mget", "GET /<index>/_mget"),
    ("_msearch", "GET /<index>/_msearch, then NDJSON"),
    ("_nodes", "GET /_nodes/stats"),
    ("_pit", "POST /<index>/_pit?keep_alive=1m"),
    ("_plugins", "POST /_plugins/_sql"),
    ("_refresh", "POST /<index>/_refresh"),
    ("_reindex", "POST /_reindex"),
    ("_search", "GET /<index>/_search"),
    ("_settings", "GET|PUT /<index>/_settings"),
    ("_sql", "POST /_sql?format=json"),
    ("_tasks", "GET /_tasks"),
    ("_update", "POST /<index>/_update/<id>"),
    ("_update_by_query", "POST /<index>/_update_by_query"),
    ("_validate", "GET /<index>/_validate/query"),
];

/// The query DSL's words, and the body's other common keys.
const QUERY_WORDS: &[&str] = &[
    "bool",
    "exists",
    "filter",
    "fuzzy",
    "ids",
    "match",
    "match_all",
    "match_phrase",
    "match_phrase_prefix",
    "multi_match",
    "must",
    "must_not",
    "nested",
    "prefix",
    "query_string",
    "range",
    "regexp",
    "should",
    "simple_query_string",
    "term",
    "terms",
    "wildcard",
];

const BODY_WORDS: &[&str] = &[
    "_source",
    "aggregations",
    "aggs",
    "doc",
    "doc_as_upsert",
    "fields",
    "from",
    "gt",
    "gte",
    "highlight",
    "lt",
    "lte",
    "mappings",
    "number_of_replicas",
    "number_of_shards",
    "order",
    "pit",
    "profile",
    "properties",
    "query",
    "script",
    "search_after",
    "settings",
    "size",
    "sort",
    "track_total_hits",
    "upsert",
];

const AGGREGATIONS: &[&str] = &[
    "avg",
    "cardinality",
    "composite",
    "date_histogram",
    "filters",
    "histogram",
    "max",
    "min",
    "percentiles",
    "stats",
    "sum",
    "top_hits",
    "value_count",
];

/// The field types a mapping names.
const FIELD_TYPES: &[&str] = &[
    "boolean",
    "date",
    "dense_vector",
    "double",
    "float",
    "geo_point",
    "integer",
    "ip",
    "keyword",
    "long",
    "object",
    "text",
];

/// Every word with what completion shows beside it, sorted by word for lookups.
static WORDS: LazyLock<Vec<(&'static str, &'static str)>> = LazyLock::new(|| {
    let mut words: Vec<(&str, &str)> = Method::ALL
        .iter()
        .map(|method| (method.as_str(), "method"))
        .collect();
    words.extend(ENDPOINTS.iter().copied());
    words.extend(QUERY_WORDS.iter().map(|word| (*word, "query")));
    words.extend(AGGREGATIONS.iter().map(|word| (*word, "aggregation")));
    words.extend(FIELD_TYPES.iter().map(|word| (*word, "field type")));
    words.extend(BODY_WORDS.iter().map(|word| (*word, "")));
    let mut seen = std::collections::HashSet::new();
    words.retain(|(word, _)| seen.insert(*word));
    words.sort_unstable_by_key(|(word, _)| *word);
    words
});

static NAMES: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| WORDS.iter().map(|(word, _)| *word).collect());

/// Every word completion offers, sorted, written as the servers read them: the methods in
/// capitals, the endpoints and the body's keys in lower case.
pub(crate) fn completion_words() -> &'static [&'static str] {
    &NAMES
}

/// What completion shows beside `word`: an endpoint's call, or what kind of word it is.
pub(crate) fn detail(word: &str) -> Option<&'static str> {
    WORDS
        .binary_search_by_key(&word, |(listed, _)| listed)
        .ok()
        .map(|index| WORDS[index].1)
        .filter(|detail| !detail.is_empty())
}

/// Whether `word` is highlighted: a method, an endpoint, or one of JSON's literals.
pub(crate) fn is_keyword(word: &str) -> bool {
    Method::from_word(word).is_some()
        || matches!(word, "true" | "false" | "null")
        || ENDPOINTS.iter().any(|(endpoint, _)| *endpoint == word)
}
