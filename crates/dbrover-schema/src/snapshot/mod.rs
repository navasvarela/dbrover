//! Schema snapshots (VC3): a schema written to a TOML file that is the same, byte for byte,
//! each time it is written from an unchanged schema, and read back to compare with a live one.
//! See `docs/schema-compare.md`, "Snapshot files".

mod read;
#[cfg(test)]
mod tests;
mod write;

use dbrover_sql::Dialect;

pub use read::read_snapshot;
pub use write::write_snapshot;

/// The file's version, which a reader checks before anything else. Raised when the layout
/// changes in a way an older DbRover would misread.
const FORMAT: u32 = 1;

/// Why a file can't be read as a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("It isn't a schema snapshot: {0}")]
    Invalid(String),
    #[error("It was written by a newer DbRover (format {0}); update DbRover to read it.")]
    Newer(u32),
    #[error("It names an engine DbRover doesn't know: \u{201c}{0}\u{201d}.")]
    Engine(String),
    #[error("It has a {what} DbRover doesn't know: \u{201c}{value}\u{201d}.")]
    Value { what: &'static str, value: String },
}

const ENGINES: [(Dialect, &str); 7] = [
    (Dialect::Postgres, "postgresql"),
    (Dialect::Sqlite, "sqlite"),
    (Dialect::Mysql, "mysql"),
    (Dialect::SqlServer, "sqlserver"),
    (Dialect::DuckDb, "duckdb"),
    (Dialect::ClickHouse, "clickhouse"),
    (Dialect::Cassandra, "cql"),
];

/// How a snapshot names the engine it was taken on.
fn engine_name(dialect: Dialect) -> &'static str {
    ENGINES
        .iter()
        .find(|(candidate, _)| *candidate == dialect)
        .map_or("unknown", |(_, name)| name)
}

fn dialect_named(name: &str) -> Option<Dialect> {
    ENGINES
        .iter()
        .find(|(_, candidate)| *candidate == name)
        .map(|(dialect, _)| *dialect)
}
