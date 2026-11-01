//! The classifier against a corpus per dialect, in `tests/classify/<dialect>.sql`.
//!
//! Each statement in a corpus follows a comment naming what it must be classified as,
//! `-- expect: read` (or `write`, `schema`, `admin`, `transaction`, `unparseable`). The
//! files are SQL so they read, and highlight, like the statements an assistant sends.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use dbrover_sql::{Dialect, StatementKind, Unclassified, classify, split_statements};

const EXPECT: &str = "-- expect:";

fn check_corpus(dialect: Dialect, corpus: &str) {
    let mut previous_end = 0;
    let mut checked = 0;
    for range in split_statements(corpus, dialect) {
        let before = &corpus[previous_end..range.start];
        previous_end = range.end;
        let statement = &corpus[range];
        let expected = before
            .lines()
            .filter_map(|line| line.trim().strip_prefix(EXPECT))
            .next_back()
            .unwrap_or_else(|| panic!("{dialect:?}: no `{EXPECT}` line before {statement}"))
            .trim();
        let actual = match classify(statement, dialect) {
            Ok(StatementKind::Read) => "read",
            Ok(StatementKind::Write) => "write",
            Ok(StatementKind::Schema) => "schema",
            Ok(StatementKind::Transaction) => "transaction",
            Ok(StatementKind::Admin) => "admin",
            Err(Unclassified::Unparseable(_)) => "unparseable",
            Err(error) => panic!("{dialect:?}: {statement}: {error}"),
        };
        assert_eq!(actual, expected, "{dialect:?}: {statement}");
        checked += 1;
    }
    assert!(
        checked > 20,
        "{dialect:?}: only {checked} statements checked"
    );
}

#[test]
fn postgres() {
    check_corpus(Dialect::Postgres, include_str!("classify/postgres.sql"));
}

#[test]
fn mysql() {
    check_corpus(Dialect::Mysql, include_str!("classify/mysql.sql"));
}

#[test]
fn sqlite() {
    check_corpus(Dialect::Sqlite, include_str!("classify/sqlite.sql"));
}

#[test]
fn sql_server() {
    check_corpus(Dialect::SqlServer, include_str!("classify/sqlserver.sql"));
}

#[test]
fn cql() {
    check_corpus(Dialect::Cassandra, include_str!("classify/cql.sql"));
}
