use std::sync::LazyLock;

use crate::dialect::Dialect;

/// Words highlighted as keywords, for every dialect alike. Not a complete list for any engine: the common
/// statement and clause keywords, types and literals that make a query readable at a
/// glance. Kept sorted for binary search; a test checks that.
const KEYWORDS: &[&str] = &[
    "add",
    "all",
    "alter",
    "analyze",
    "and",
    "any",
    "apply",
    "array",
    "as",
    "asc",
    "asof",
    "attach",
    "autoincrement",
    "begin",
    "between",
    "bigint",
    "boolean",
    "both",
    "by",
    "call",
    "cascade",
    "case",
    "cast",
    "catch",
    "check",
    "collate",
    "column",
    "commit",
    "concurrently",
    "conflict",
    "constraint",
    "create",
    "cross",
    "current_date",
    "current_time",
    "current_timestamp",
    "database",
    "date",
    "declare",
    "default",
    "deferrable",
    "delete",
    "desc",
    "describe",
    "detach",
    "distinct",
    "do",
    "double",
    "drop",
    "else",
    "end",
    "except",
    "exclude",
    "exec",
    "execute",
    "exists",
    "explain",
    "extension",
    "false",
    "fetch",
    "filter",
    "first",
    "following",
    "for",
    "foreign",
    "from",
    "full",
    "function",
    "grant",
    "group",
    "having",
    "identity",
    "if",
    "ilike",
    "in",
    "index",
    "inner",
    "insert",
    "integer",
    "intersect",
    "interval",
    "into",
    "is",
    "join",
    "json",
    "jsonb",
    "key",
    "language",
    "last",
    "lateral",
    "leading",
    "left",
    "like",
    "limit",
    "macro",
    "matched",
    "materialized",
    "merge",
    "natural",
    "next",
    "nocount",
    "nolock",
    "not",
    "nothing",
    "null",
    "nulls",
    "numeric",
    "nvarchar",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "output",
    "over",
    "partition",
    "percent",
    "pivot",
    "positional",
    "pragma",
    "preceding",
    "primary",
    "print",
    "procedure",
    "qualify",
    "range",
    "real",
    "recursive",
    "references",
    "refresh",
    "reindex",
    "release",
    "rename",
    "replace",
    "restrict",
    "returning",
    "returns",
    "revoke",
    "right",
    "rollback",
    "row",
    "rows",
    "sample",
    "savepoint",
    "schema",
    "select",
    "sequence",
    "serial",
    "set",
    "show",
    "similar",
    "smallint",
    "some",
    "summarize",
    "table",
    "temp",
    "temporary",
    "text",
    "then",
    "throw",
    "ties",
    "time",
    "timestamp",
    "timestamptz",
    "to",
    "top",
    "trailing",
    "tran",
    "transaction",
    "trigger",
    "true",
    "truncate",
    "try",
    "type",
    "union",
    "unique",
    "unlogged",
    "unpivot",
    "update",
    "using",
    "uuid",
    "vacuum",
    "values",
    "varchar",
    "view",
    "when",
    "where",
    "window",
    "with",
    "without",
];

/// The keywords, lowercase and sorted, for completion.
pub fn keywords() -> &'static [&'static str] {
    KEYWORDS
}

/// CQL's own words beyond [`KEYWORDS`], highlighted and completed in CQL only: elsewhere `map`,
/// `list` or `token` name a column as often as not, and the formatter would re-case them.
/// Sorted, like [`KEYWORDS`].
const CQL_KEYWORDS: &[&str] = &[
    "aggregate",
    "allow",
    "ascii",
    "batch",
    "blob",
    "bypass",
    "clustering",
    "compact",
    "contains",
    "counter",
    "custom",
    "decimal",
    "durable_writes",
    "duration",
    "filtering",
    "float",
    "frozen",
    "inet",
    "int",
    "keyspace",
    "keyspaces",
    "list",
    "map",
    "norecursive",
    "per",
    "permissions",
    "replication",
    "static",
    "superuser",
    "timeout",
    "timeuuid",
    "tinyint",
    "token",
    "ttl",
    "tuple",
    "use",
    "varint",
    "writetime",
];

/// [`KEYWORDS`] and [`CQL_KEYWORDS`] together, sorted, for a CQL console's completion.
static CQL_COMPLETION: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut words: Vec<&'static str> = KEYWORDS.iter().chain(CQL_KEYWORDS).copied().collect();
    words.sort_unstable();
    words.dedup();
    words
});

impl Dialect {
    /// The keywords completion offers in this dialect, lowercase and sorted.
    pub fn keywords(self) -> &'static [&'static str] {
        match self {
            Self::Cassandra => &CQL_COMPLETION,
            _ => KEYWORDS,
        }
    }

    /// Whether `word` is highlighted as a keyword in this dialect, ignoring case.
    pub fn is_keyword(self, word: &str) -> bool {
        is_keyword(word)
            || (self == Self::Cassandra
                && CQL_KEYWORDS
                    .binary_search(&word.to_ascii_lowercase().as_str())
                    .is_ok())
    }
}

/// Whether `word` is a highlighted SQL keyword, ignoring case.
pub fn is_keyword(word: &str) -> bool {
    KEYWORDS
        .binary_search(&word.to_ascii_lowercase().as_str())
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_sorted_and_lookup_ignores_case() {
        assert!(KEYWORDS.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(is_keyword("SELECT"));
        assert!(is_keyword("Returning"));
        assert!(!is_keyword("users"));
    }

    #[test]
    fn cql_has_words_of_its_own() {
        assert!(CQL_KEYWORDS.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(Dialect::Cassandra.is_keyword("KEYSPACE"));
        assert!(Dialect::Cassandra.is_keyword("select"));
        assert!(!Dialect::Postgres.is_keyword("keyspace"));
        let words = Dialect::Cassandra.keywords();
        assert!(words.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(words.contains(&"filtering") && words.contains(&"select"));
    }
}
