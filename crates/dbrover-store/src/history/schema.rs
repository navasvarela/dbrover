//! The history's tables and indexes, and bringing an older file up to date.
//!
//! `PRAGMA user_version` numbers the schema. Every migration only adds nullable columns,
//! indexes and tables, never rewrites rows: an older DbRover may still have the file open during an
//! upgrade, and its `INSERT`s, which leave the new columns out, keep working.

use rusqlite::TransactionBehavior;

/// The schema this DbRover writes. 0 is every file from before provenance (W4), with or without
/// `source`; 3 adds report runs (RP4).
const VERSION: i32 = 3;

/// The table as the first DbRover with MCP made it; [`COLUMNS`] adds the rest.
const TABLE: &str = "
CREATE TABLE IF NOT EXISTS history (
    id            INTEGER PRIMARY KEY,
    data_source   TEXT NOT NULL,
    executed_at   TEXT NOT NULL,
    sql           TEXT NOT NULL,
    duration_ms   INTEGER,
    rows          INTEGER,
    outcome       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS history_by_source ON history (data_source, executed_at DESC);
";

/// Columns added since the first version, each with the definition that adds it.
const COLUMNS: [(&str, &str); 4] = [
    // Before MCP (0.2.0 and earlier) every row came from the app, which is what the default
    // says.
    ("source", "TEXT NOT NULL DEFAULT 'app'"),
    // Where in DbRover: see `HistorySource::origin`. `NULL` in rows from before it, and in rows
    // an older DbRover writes, which read as `source`.
    ("origin", "TEXT"),
    // The MCP client's name, or the imported file.
    ("detail", "TEXT"),
    // A grid edit's row change, as JSON.
    ("changes", "TEXT"),
];

/// The searches the History window makes: every data source by time, and by kind and time.
/// The kind is spelt exactly as the searches spell it, or SQLite won't use the index.
const INDEXES: &str = "
CREATE INDEX IF NOT EXISTS history_by_time ON history (executed_at DESC);
CREATE INDEX IF NOT EXISTS history_by_origin
    ON history (coalesce(origin, source), executed_at DESC);
";

/// Each run of a report (`docs/reporting.md`, "Run history"), and its newest runs found by
/// the report's name. A table of its own, which an older DbRover neither reads nor minds.
const REPORT_RUNS: &str = "
CREATE TABLE IF NOT EXISTS report_runs (
    id          INTEGER PRIMARY KEY,
    report      TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    elapsed_ms  INTEGER NOT NULL,
    outcome     TEXT NOT NULL,
    params      TEXT NOT NULL,
    steps       TEXT NOT NULL,
    failure     TEXT
);
CREATE INDEX IF NOT EXISTS report_runs_by_report ON report_runs (report, started_at DESC);
";

/// Brings the schema up to date. In an immediate transaction, so when the app and the MCP
/// server open an old file at the same moment, the second waits, then finds it migrated.
///
/// A file from a newer DbRover (a higher version) is left as it is: its additions are more
/// nullable columns and tables, which this one doesn't write.
pub(super) fn migrate(connection: &mut rusqlite::Connection) -> rusqlite::Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i32 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < VERSION {
        transaction.execute_batch(TABLE)?;
        for (column, definition) in COLUMNS {
            if !has_column(&transaction, column)? {
                transaction.execute(
                    &format!("ALTER TABLE history ADD COLUMN {column} {definition}"),
                    [],
                )?;
            }
        }
        transaction.execute_batch(INDEXES)?;
        transaction.execute_batch(REPORT_RUNS)?;
        transaction.pragma_update(None, "user_version", VERSION)?;
    }
    transaction.commit()
}

fn has_column(connection: &rusqlite::Connection, column: &str) -> rusqlite::Result<bool> {
    connection
        .prepare_cached("SELECT 1 FROM pragma_table_info('history') WHERE name = ?1")?
        .exists([column])
}
