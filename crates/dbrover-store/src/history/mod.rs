//! Query history: every statement DbRover or an MCP client sends, in `history.sqlite`, with
//! where it came from. Only the SQL and how it went are kept, never the rows.
//!
//! The app and `dbrover mcp` may have the file open at the same time, each writing. The
//! database is in WAL mode, where one writer at a time appends while readers carry on, and
//! each connection waits up to [`BUSY_TIMEOUT`] for the other's write to finish rather than
//! failing at once.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dbrover_core::DataSourceId;
use jiff::Timestamp;
use rusqlite::params;

pub use change::{NamedCell, ReplacedCell, RowChange, RowChangeKind};
pub use export::HistoryExportError;
pub use redact::redact_secrets;
pub use runs::{
    CombineCaps, CombineMetrics, DEFAULT_RUNS_KEPT, OutputMetrics, ReportRun, RunError, RunId,
    RunOrigin, RunOutcome, RunProfile, SourceCaps, SourceMetrics, SourcePhases, StepMetrics,
};
pub use source::{HistorySource, HistorySourceKind};

mod change;
mod export;
mod redact;
mod retention;
mod runs;
mod schema;
mod source;

/// Lexically sortable and RFC 3339: fixed millisecond precision, always UTC. A variable
/// number of fraction digits would sort `…:00.5Z` before `…:00Z`.
const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

/// How long a write waits for the other process's write to finish. Each write is one small
/// row, so a wait anywhere near this long means something is wrong, not busy.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait between attempts to switch the file to WAL (see [`switch_to_wal`]): long
/// enough not to spin, short next to the other process's few milliseconds of opening.
const WAL_RETRY_DELAY: Duration = Duration::from_millis(10);

/// How a statement ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryOutcome {
    Ok,
    Error,
    Cancelled,
}

impl HistoryOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Ok, Self::Error, Self::Cancelled]
            .into_iter()
            .find(|outcome| outcome.as_str() == text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub data_source: DataSourceId,
    pub executed_at: Timestamp,
    pub sql: String,
    pub duration: Option<Duration>,
    /// Rows changed or returned, when the database reported it.
    pub rows: Option<u64>,
    pub outcome: HistoryOutcome,
    pub source: HistorySource,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("could not open the query history at {}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("could not create the folder for {}", path.display())]
    CreateFolder {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the query history database failed")]
    Database(#[from] rusqlite::Error),
    #[error("could not write a report run as JSON")]
    Encode(#[source] serde_json::Error),
}

/// Where the history database lives; [`HistoryFile::open`] opens it.
#[derive(Debug, Clone)]
pub struct HistoryFile {
    path: PathBuf,
}

impl HistoryFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Opens the database, creating it and its folder if needed.
    ///
    /// # Errors
    ///
    /// [`HistoryError::CreateFolder`] or [`HistoryError::Open`] if it can't be created or
    /// isn't a usable database.
    pub fn open(&self) -> Result<History, HistoryError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| HistoryError::CreateFolder {
                path: self.path.clone(),
                source,
            })?;
        }
        let open_error = |source| HistoryError::Open {
            path: self.path.clone(),
            source,
        };
        let mut connection = rusqlite::Connection::open(&self.path).map_err(open_error)?;
        connection.busy_timeout(BUSY_TIMEOUT).map_err(open_error)?;
        switch_to_wal(&connection).map_err(open_error)?;
        commit_without_syncing(&connection).map_err(open_error)?;
        schema::migrate(&mut connection).map_err(open_error)?;
        Ok(History { connection })
    }
}

/// Commits append to the WAL and return without waiting for the disk (`synchronous =
/// NORMAL`); only checkpoints sync. If the app or `dbrover mcp` crashes, nothing is lost; a
/// power cut may lose the last few statements recorded; the file is never corrupted.
///
/// With the default (`FULL`) every commit holds the write lock through a sync, which takes
/// several milliseconds on Windows. SQLite's busy handler polls instead of queueing, so while
/// one process records statements back to back (a script, or the test's 200 rows), the other
/// never gets in between two commits and waits out the whole run. The wait was in `migrate`,
/// and once the run lasted longer than [`BUSY_TIMEOUT`], `open` failed with `SQLITE_BUSY`. That
/// was `tests::two_processes_write_at_once` failing on Windows CI. Without the sync a commit
/// takes microseconds, and so does the wait.
fn commit_without_syncing(connection: &rusqlite::Connection) -> rusqlite::Result<()> {
    connection.pragma_update(None, "synchronous", "NORMAL")
}

/// Puts the file in WAL mode. Persistent: once one process switches the file, every later
/// connection, an older DbRover's included, opens it in WAL mode.
///
/// The switch needs an exclusive lock, which SQLite takes without calling the busy handler, so
/// when the app and `dbrover mcp` open a new file at the same moment, one of them fails at once
/// with `SQLITE_BUSY` whatever [`BUSY_TIMEOUT`] says. That was
/// `tests::two_processes_write_at_once` failing about 1 run in 1,000 (in 0.08 s, nowhere near
/// the timeout). So the switch is retried here for as long as a write would wait.
fn switch_to_wal(connection: &rusqlite::Connection) -> rusqlite::Result<()> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        let switched = connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        });
        match switched {
            Err(error) if is_busy(&error) && Instant::now() < deadline => {
                std::thread::sleep(WAL_RETRY_DELAY);
            }
            other => return other.map(drop),
        }
    }
}

fn is_busy(error: &rusqlite::Error) -> bool {
    error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
}

/// An open history database.
pub struct History {
    connection: rusqlite::Connection,
}

impl History {
    /// Adds the entry, with any password or connection-string secret in its SQL replaced by
    /// `<redacted>` (see [`redact_secrets`]): every writer comes through here, so nothing
    /// that reaches the file has one.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Database`] if the row can't be written.
    pub fn record(&self, entry: &HistoryEntry) -> Result<(), HistoryError> {
        let duration_ms = entry
            .duration
            .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX));
        let rows = entry
            .rows
            .map(|rows| i64::try_from(rows).unwrap_or(i64::MAX));
        self.connection.execute(
            "INSERT INTO history
                 (data_source, executed_at, sql, duration_ms, rows, outcome, source, origin, detail,
                  changes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                entry.data_source.to_string(),
                format_timestamp(entry.executed_at),
                redact_secrets(&entry.sql),
                duration_ms,
                rows,
                entry.outcome.as_str(),
                entry.source.legacy_source(),
                entry.source.origin(),
                entry.source.detail(),
                entry.source.changes(),
            ],
        )?;
        Ok(())
    }

    /// The statements `query` asks for, newest first, at most `limit` of them. Rows that
    /// don't parse (edited by hand, say) are skipped.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Database`] if the query fails.
    pub fn search(
        &self,
        query: &HistoryQuery,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, HistoryError> {
        let mut entries = Vec::new();
        self.each_match(query, Some(limit), |entry| {
            entries.push(entry);
            Ok::<_, HistoryError>(())
        })?;
        Ok(entries)
    }

    /// Hands each statement `query` asks for to `each`, newest first, as SQLite reads them, so
    /// that every match can be gone through without holding them all. Stops at the first
    /// error `each` returns.
    fn each_match<E: From<HistoryError>>(
        &self,
        query: &HistoryQuery,
        limit: Option<usize>,
        mut each: impl FnMut(HistoryEntry) -> Result<(), E>,
    ) -> Result<(), E> {
        let search = Search::new(query, limit);
        let mut statement = self
            .connection
            .prepare_cached(&search.sql)
            .map_err(HistoryError::from)?;
        let mut rows = statement
            .query(rusqlite::params_from_iter(&search.values))
            .map_err(HistoryError::from)?;
        while let Some(row) = rows.next().map_err(HistoryError::from)? {
            let stored = StoredRow::read(row).map_err(HistoryError::from)?;
            if let Some(entry) = stored.into_entry() {
                each(entry)?;
            }
        }
        Ok(())
    }
}

/// Which past statements to look up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryQuery {
    /// Only this data source's statements; `None` for every data source in the file.
    pub data_source: Option<DataSourceId>,
    /// Statements containing it, ignoring ASCII case; empty for all.
    pub text: String,
    /// Only statements from there; `None` for all.
    pub kind: Option<HistorySourceKind>,
    /// Only statements run at or after it.
    pub since: Option<Timestamp>,
    /// Only statements run before it.
    pub until: Option<Timestamp>,
}

impl HistoryQuery {
    /// Every recorded statement of `data_source`.
    pub fn all(data_source: DataSourceId) -> Self {
        Self {
            data_source: Some(data_source),
            ..Self::everywhere()
        }
    }

    /// Every recorded statement, of every data source.
    pub fn everywhere() -> Self {
        Self {
            data_source: None,
            text: String::new(),
            kind: None,
            since: None,
            until: None,
        }
    }
}

/// A [`HistoryQuery`] as SQL and its values.
///
/// The `WHERE` clause names only the filters the query sets. Written once with
/// `?n IS NULL OR …` for every filter, it would read the same, but SQLite plans a statement
/// before it sees the values, so it couldn't use the index that fits (by data source, by kind,
/// or by time alone) and would read the whole file.
struct Search {
    sql: String,
    values: Vec<rusqlite::types::Value>,
}

impl Search {
    fn new(query: &HistoryQuery, limit: Option<usize>) -> Self {
        let mut conditions = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        let mut filter = |condition: &str, value: String| {
            values.push(value.into());
            conditions.push(format!("{condition} ?{}", values.len()));
        };
        if let Some(data_source) = query.data_source {
            filter("data_source =", data_source.to_string());
        }
        if let Some(kind) = query.kind {
            filter("coalesce(origin, source) =", kind.as_str().to_owned());
        }
        if let Some(since) = query.since {
            filter("executed_at >=", format_timestamp(since));
        }
        if let Some(until) = query.until {
            filter("executed_at <", format_timestamp(until));
        }
        if !query.text.is_empty() {
            values.push(escape_like(&query.text).into());
            conditions.push(format!(
                "sql LIKE '%' || ?{} || '%' ESCAPE '\\'",
                values.len()
            ));
        }
        let mut sql = format!("SELECT {STORED_COLUMNS} FROM history");
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        sql.push_str(" ORDER BY executed_at DESC, id DESC");
        if let Some(limit) = limit {
            values.push(i64::try_from(limit).unwrap_or(i64::MAX).into());
            sql.push_str(&format!(" LIMIT ?{}", values.len()));
        }
        Self { sql, values }
    }
}

/// The columns [`StoredRow::read`] reads, in its order.
const STORED_COLUMNS: &str = "data_source, executed_at, sql, duration_ms, rows, outcome, \
     coalesce(origin, source), detail, changes";

/// A history row as stored, before parsing.
struct StoredRow {
    data_source: String,
    executed_at: String,
    sql: String,
    duration_ms: Option<i64>,
    rows: Option<i64>,
    outcome: String,
    /// `coalesce(origin, source)`.
    kind: String,
    detail: Option<String>,
    changes: Option<String>,
}

impl StoredRow {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            data_source: row.get(0)?,
            executed_at: row.get(1)?,
            sql: row.get(2)?,
            duration_ms: row.get(3)?,
            rows: row.get(4)?,
            outcome: row.get(5)?,
            kind: row.get(6)?,
            detail: row.get(7)?,
            changes: row.get(8)?,
        })
    }

    fn into_entry(self) -> Option<HistoryEntry> {
        Some(HistoryEntry {
            data_source: self.data_source.parse().ok()?,
            executed_at: self.executed_at.parse().ok()?,
            sql: self.sql,
            duration: self
                .duration_ms
                .and_then(|ms| u64::try_from(ms).ok())
                .map(Duration::from_millis),
            rows: self.rows.and_then(|rows| u64::try_from(rows).ok()),
            outcome: HistoryOutcome::parse(&self.outcome)?,
            source: HistorySource::from_stored(&self.kind, self.detail, self.changes.as_deref())?,
        })
    }
}

fn format_timestamp(timestamp: Timestamp) -> String {
    timestamp.strftime(TIMESTAMP_FORMAT).to_string()
}

/// Makes `%`, `_` and the escape character itself match literally in a `LIKE` pattern.
fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(data_source: DataSourceId, at_ms: i64, sql: &str) -> HistoryEntry {
        HistoryEntry {
            data_source,
            executed_at: Timestamp::from_millisecond(at_ms).unwrap(),
            sql: sql.to_owned(),
            duration: Some(Duration::from_millis(12)),
            rows: Some(3),
            outcome: HistoryOutcome::Ok,
            source: HistorySource::Console,
        }
    }

    #[test]
    fn finds_a_data_sources_statements_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let history = HistoryFile::new(dir.path().join("nested/history.sqlite"))
            .open()
            .unwrap();
        let (shop, crm) = (DataSourceId::generate(), DataSourceId::generate());
        // 1_000 ms formats as `…:01.000Z`; a variable-width fraction would sort it after
        // `…:01.500Z`.
        let older = entry(shop, 1_000, "SELECT * FROM orders");
        let newer = HistoryEntry {
            outcome: HistoryOutcome::Cancelled,
            duration: None,
            rows: None,
            ..entry(shop, 1_500, "select count(*) from ORDERS")
        };
        for recorded in [&older, &newer, &entry(crm, 2_000, "SELECT * FROM orders")] {
            history.record(recorded).unwrap();
        }

        assert_eq!(
            history.search(&containing(shop, ""), 10).unwrap(),
            [newer.clone(), older]
        );
        assert_eq!(
            history.search(&containing(shop, "count"), 10).unwrap(),
            std::slice::from_ref(&newer)
        );
        assert_eq!(
            history.search(&containing(shop, "orders"), 1).unwrap(),
            [newer]
        );
    }

    fn containing(data_source: DataSourceId, text: &str) -> HistoryQuery {
        HistoryQuery {
            text: text.to_owned(),
            ..HistoryQuery::all(data_source)
        }
    }

    #[test]
    fn filters_by_source() {
        let dir = tempfile::tempdir().unwrap();
        let history = HistoryFile::new(dir.path().join("history.sqlite"))
            .open()
            .unwrap();
        let shop = DataSourceId::generate();
        let from_app = entry(shop, 1_000, "SELECT 1");
        let from_mcp = HistoryEntry {
            source: HistorySource::Mcp {
                client: Some("claude-ai".to_owned()),
            },
            ..entry(shop, 2_000, "SELECT 2")
        };
        history.record(&from_app).unwrap();
        history.record(&from_mcp).unwrap();

        let only = |kind| HistoryQuery {
            kind: Some(kind),
            ..HistoryQuery::all(shop)
        };
        assert_eq!(
            history.search(&only(HistorySourceKind::Mcp), 10).unwrap(),
            std::slice::from_ref(&from_mcp)
        );
        assert_eq!(
            history
                .search(&only(HistorySourceKind::Console), 10)
                .unwrap(),
            std::slice::from_ref(&from_app)
        );
        assert_eq!(
            history
                .search(&only(HistorySourceKind::Import), 10)
                .unwrap(),
            []
        );
        assert_eq!(
            history.search(&HistoryQuery::all(shop), 10).unwrap(),
            [from_mcp, from_app]
        );
    }

    /// A grid edit comes back with the values it replaced; one whose change can't be read
    /// (from a newer DbRover, or edited by hand) still comes back, without it.
    #[test]
    fn keeps_a_grid_edits_replaced_values() {
        let dir = tempfile::tempdir().unwrap();
        let file = HistoryFile::new(dir.path().join("history.sqlite"));
        let history = file.open().unwrap();
        let shop = DataSourceId::generate();
        let edit = HistoryEntry {
            source: HistorySource::GridEdit(Some(RowChange {
                table: "main.people".to_owned(),
                kind: RowChangeKind::Updated {
                    key: vec![NamedCell {
                        column: "id".to_owned(),
                        value: Some("1".to_owned()),
                    }],
                    cells: vec![ReplacedCell {
                        column: "name".to_owned(),
                        old: Some("ada".to_owned()),
                        new: Some("Ada".to_owned()),
                    }],
                },
                revert: Some("UPDATE \"main\".\"people\" SET \"name\" = 'ada'".to_owned()),
            })),
            ..entry(
                shop,
                1_000,
                "UPDATE \"main\".\"people\" SET \"name\" = 'Ada'",
            )
        };
        history.record(&edit).unwrap();
        rusqlite::Connection::open(file.path())
            .unwrap()
            .execute(
                "UPDATE history SET changes = '{\"from\":\"the future\"}'
                 WHERE id = (SELECT max(id) FROM history)",
                [],
            )
            .unwrap();
        history.record(&edit).unwrap();

        let sources: Vec<HistorySource> = history
            .search(&HistoryQuery::all(shop), 10)
            .unwrap()
            .into_iter()
            .map(|entry| entry.source)
            .collect();
        assert_eq!(sources, [edit.source, HistorySource::GridEdit(None)]);
    }

    /// Across data sources, by kind, text and time, alone and together.
    #[test]
    fn searches_every_data_source_by_kind_text_and_time() {
        let dir = tempfile::tempdir().unwrap();
        let history = HistoryFile::new(dir.path().join("history.sqlite"))
            .open()
            .unwrap();
        let (shop, crm) = (DataSourceId::generate(), DataSourceId::generate());
        let mcp = HistorySource::Mcp {
            client: Some("claude-ai".to_owned()),
        };
        let recorded = [
            entry(shop, 1_000, "SELECT * FROM orders"),
            HistoryEntry {
                source: mcp.clone(),
                ..entry(crm, 2_000, "SELECT * FROM leads")
            },
            entry(crm, 3_000, "DELETE FROM leads"),
            HistoryEntry {
                source: mcp,
                ..entry(shop, 4_000, "SELECT count(*) FROM orders")
            },
        ];
        for entry in &recorded {
            history.record(entry).unwrap();
        }

        let found = |query: HistoryQuery| -> Vec<String> {
            history
                .search(&query, 10)
                .unwrap()
                .into_iter()
                .map(|entry| entry.sql)
                .collect()
        };
        let at = |ms| Some(Timestamp::from_millisecond(ms).unwrap());
        let everywhere = HistoryQuery::everywhere;
        assert_eq!(
            found(everywhere()),
            [
                "SELECT count(*) FROM orders",
                "DELETE FROM leads",
                "SELECT * FROM leads",
                "SELECT * FROM orders"
            ]
        );
        assert_eq!(
            found(HistoryQuery {
                kind: Some(HistorySourceKind::Mcp),
                ..everywhere()
            }),
            ["SELECT count(*) FROM orders", "SELECT * FROM leads"]
        );
        assert_eq!(
            found(HistoryQuery {
                text: "leads".to_owned(),
                since: at(2_500),
                ..everywhere()
            }),
            ["DELETE FROM leads"]
        );
        // From inclusive, until exclusive.
        assert_eq!(
            found(HistoryQuery {
                since: at(2_000),
                until: at(4_000),
                ..everywhere()
            }),
            ["DELETE FROM leads", "SELECT * FROM leads"]
        );
        assert_eq!(
            found(HistoryQuery {
                kind: Some(HistorySourceKind::Mcp),
                until: at(3_000),
                ..HistoryQuery::all(crm)
            }),
            ["SELECT * FROM leads"]
        );
        let shops: Vec<DataSourceId> = history
            .search(&everywhere(), 10)
            .unwrap()
            .into_iter()
            .map(|entry| entry.data_source)
            .collect();
        assert_eq!(shops, [shop, crm, crm, shop]);
    }

    /// Each search the History window makes reads the history through an index, in order:
    /// never the whole table, and never sorting every match before the first comes back.
    #[test]
    fn searches_through_an_index() {
        let dir = tempfile::tempdir().unwrap();
        let file = HistoryFile::new(dir.path().join("history.sqlite"));
        let history = file.open().unwrap();
        let shop = DataSourceId::generate();
        let since = Some(Timestamp::UNIX_EPOCH);
        let queries = [
            HistoryQuery::everywhere(),
            HistoryQuery {
                text: "orders".to_owned(),
                since,
                ..HistoryQuery::everywhere()
            },
            HistoryQuery {
                kind: Some(HistorySourceKind::GridEdit),
                ..HistoryQuery::everywhere()
            },
            HistoryQuery {
                text: "orders".to_owned(),
                ..HistoryQuery::all(shop)
            },
            HistoryQuery {
                kind: Some(HistorySourceKind::Mcp),
                since,
                ..HistoryQuery::all(shop)
            },
        ];
        for query in queries {
            let search = Search::new(&query, Some(500));
            let plan: Vec<String> = history
                .connection
                .prepare(&format!("EXPLAIN QUERY PLAN {}", search.sql))
                .unwrap()
                .query_map(rusqlite::params_from_iter(&search.values), |row| row.get(3))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let reads_the_table = plan
                .iter()
                .any(|step| step.starts_with("SCAN history") && !step.contains("INDEX"));
            let sorts_everything = plan
                .iter()
                .any(|step| step == "USE TEMP B-TREE FOR ORDER BY");
            assert!(!reads_the_table && !sorts_everything, "{query:?}: {plan:?}");
        }
    }

    #[test]
    fn matches_like_wildcards_literally() {
        let dir = tempfile::tempdir().unwrap();
        let history = HistoryFile::new(dir.path().join("history.sqlite"))
            .open()
            .unwrap();
        let shop = DataSourceId::generate();
        for sql in ["select '100%'", "select 1000", "select a_b", "select axb"] {
            history.record(&entry(shop, 0, sql)).unwrap();
        }

        let found = |text| -> Vec<String> {
            history
                .search(&containing(shop, text), 10)
                .unwrap()
                .into_iter()
                .map(|entry| entry.sql)
                .collect()
        };
        assert_eq!(found("0%"), ["select '100%'"]);
        assert_eq!(found("a_b"), ["select a_b"]);
    }

    /// The table as DbRover 0.2.0 made it, before MCP.
    const SCHEMA_0_2: &str = "CREATE TABLE history (
        id INTEGER PRIMARY KEY, data_source TEXT NOT NULL, executed_at TEXT NOT NULL,
        sql TEXT NOT NULL, duration_ms INTEGER, rows INTEGER, outcome TEXT NOT NULL
    );";

    /// The table as DbRover made it from MCP until provenance: `source`, and no
    /// `user_version`.
    const SCHEMA_BEFORE_PROVENANCE: &str = "CREATE TABLE history (
        id INTEGER PRIMARY KEY, data_source TEXT NOT NULL, executed_at TEXT NOT NULL,
        sql TEXT NOT NULL, duration_ms INTEGER, rows INTEGER, outcome TEXT NOT NULL,
        source TEXT NOT NULL DEFAULT 'app'
    );
    CREATE INDEX history_by_source ON history (data_source, executed_at DESC);";

    /// Writes a row as DbRover before provenance did: `source` if the table has it, never
    /// `origin` or `detail`.
    fn insert_as_before(connection: &rusqlite::Connection, shop: DataSourceId, sql: &str) {
        let has_source = connection
            .prepare("SELECT 1 FROM pragma_table_info('history') WHERE name = 'source'")
            .unwrap()
            .exists([])
            .unwrap();
        let source = if has_source { ", source" } else { "" };
        let value = if has_source { ", 'app'" } else { "" };
        connection
            .execute(
                &format!(
                    "INSERT INTO history (data_source, executed_at, sql, outcome{source})
                     VALUES (?1, '2026-10-01T10:00:00.000Z', ?2, 'ok'{value})"
                ),
                params![shop.to_string(), sql],
            )
            .unwrap();
    }

    #[test]
    fn migrates_files_from_before_provenance() {
        for old_schema in [SCHEMA_0_2, SCHEMA_BEFORE_PROVENANCE] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.sqlite");
            let shop = DataSourceId::generate();
            let old = rusqlite::Connection::open(&path).unwrap();
            old.execute_batch(old_schema).unwrap();
            insert_as_before(&old, shop, "SELECT 1");

            // An older DbRover still has the file open, as when the app runs during an upgrade,
            // and goes on writing rows without provenance.
            let history = HistoryFile::new(&path).open().unwrap();
            let from_mcp = HistorySource::Mcp {
                client: Some("claude-ai".to_owned()),
            };
            history
                .record(&HistoryEntry {
                    source: from_mcp.clone(),
                    ..entry(shop, 1_800_000_000_000, "SELECT 2")
                })
                .unwrap();
            insert_as_before(&old, shop, "SELECT 3");

            let sources: Vec<(String, HistorySource)> = history
                .search(&HistoryQuery::all(shop), 10)
                .unwrap()
                .into_iter()
                .map(|entry| (entry.sql, entry.source))
                .collect();
            assert_eq!(
                sources,
                [
                    ("SELECT 2".to_owned(), from_mcp),
                    ("SELECT 3".to_owned(), HistorySource::DbRover),
                    ("SELECT 1".to_owned(), HistorySource::DbRover),
                ]
            );
            // What an older DbRover's search reads: every row has a `source` it parses.
            let legacy: Vec<String> = old
                .prepare("SELECT source FROM history ORDER BY id")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(legacy, ["app", "mcp", "app"]);
            let (mode, version): (String, i32) = old
                .query_row(
                    "SELECT * FROM pragma_journal_mode, pragma_user_version",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!((mode.as_str(), version), ("wal", 6));
        }
    }

    #[test]
    fn leaves_a_file_from_a_newer_dbrover_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let file = HistoryFile::new(dir.path().join("history.sqlite"));
        drop(file.open().unwrap());
        let newer = rusqlite::Connection::open(file.path()).unwrap();
        newer
            .execute_batch("ALTER TABLE history ADD COLUMN later TEXT; PRAGMA user_version = 7;")
            .unwrap();

        let history = file.open().unwrap();
        let shop = DataSourceId::generate();
        history.record(&entry(shop, 0, "SELECT 1")).unwrap();
        assert_eq!(
            history.search(&HistoryQuery::all(shop), 10).unwrap().len(),
            1
        );
        let version: i32 = newer
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
    }

    /// The guard against secrets in the history: statements carrying one in every form
    /// [`redact_secrets`] knows, from every kind of writer, and then the file and its WAL read
    /// as bytes, where neither may hold it.
    #[test]
    fn never_writes_a_secret_to_the_file() {
        const SECRET: &str = "Hunter2Secret";
        let dir = tempfile::tempdir().unwrap();
        let file = HistoryFile::new(dir.path().join("history.sqlite"));
        let history = file.open().unwrap();
        let shop = DataSourceId::generate();
        let statements = [
            format!("CREATE ROLE app LOGIN PASSWORD '{SECRET}'"),
            format!("CREATE LOGIN app WITH PASSWORD = N'{SECRET}'"),
            format!("ALTER USER 'app'@'%' IDENTIFIED BY '{SECRET}'"),
            format!("CREATE USER app IDENTIFIED WITH plaintext_password BY '{SECRET}'"),
            format!("CREATE SECRET (TYPE s3, KEY_ID 'k', SECRET '{SECRET}')"),
            format!("SELECT dblink_connect('host=db password={SECRET}')"),
            format!("ATTACH 'postgresql://app:{SECRET}@db/crm' AS crm"),
            format!("SELECT 'Server=db;Pwd={SECRET};'"),
        ];
        let sources = [
            HistorySource::Console,
            HistorySource::ObjectHelper,
            HistorySource::Mcp {
                client: Some("claude-ai".to_owned()),
            },
        ];
        for (sql, source) in statements.iter().zip(sources.iter().cycle()) {
            history
                .record(&HistoryEntry {
                    source: source.clone(),
                    ..entry(shop, 0, sql)
                })
                .unwrap();
        }

        let found = history.search(&HistoryQuery::all(shop), 100).unwrap();
        assert_eq!(found.len(), statements.len());
        assert!(found.iter().all(|entry| entry.sql.contains("<redacted>")));
        for path in [
            file.path().to_owned(),
            dir.path().join("history.sqlite-wal"),
        ] {
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes
                    .windows(SECRET.len())
                    .any(|window| window == SECRET.as_bytes()),
                "{} holds the secret",
                path.display()
            );
        }
    }

    /// Set to a history file's path in the child process of
    /// [`two_processes_write_at_once`]; [`write_as_second_process`] writes there.
    const CHILD_PATH: &str = "DBROVER_HISTORY_TEST_CHILD_PATH";
    const WRITES_PER_PROCESS: i64 = 200;

    /// Runs only as the child of [`two_processes_write_at_once`].
    #[test]
    #[ignore = "a helper process for two_processes_write_at_once"]
    fn write_as_second_process() {
        let Ok(path) = std::env::var(CHILD_PATH) else {
            return;
        };
        write_many(
            &HistoryFile::new(path),
            &HistorySource::Mcp {
                client: Some("claude-ai".to_owned()),
            },
        );
    }

    fn write_many(file: &HistoryFile, source: &HistorySource) {
        let history = file.open().unwrap();
        let shop = DataSourceId::generate();
        for at in 0..WRITES_PER_PROCESS {
            history
                .record(&HistoryEntry {
                    source: source.clone(),
                    ..entry(shop, at, "SELECT 1")
                })
                .unwrap();
        }
    }

    /// The app and `dbrover mcp` are separate processes, and SQLite's locks between processes
    /// differ from those between connections in one; so the second writer here really is
    /// another process: this test binary again, running [`write_as_second_process`]. The
    /// file is from before provenance, so both also migrate it at once.
    #[test]
    fn two_processes_write_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = HistoryFile::new(dir.path().join("history.sqlite"));
        rusqlite::Connection::open(file.path())
            .unwrap()
            .execute_batch(SCHEMA_BEFORE_PROVENANCE)
            .unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "history::tests::write_as_second_process",
                "--ignored",
                "--quiet",
            ])
            .env(CHILD_PATH, file.path())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        write_many(&file, &HistorySource::Console);
        // libtest prints a failed test's panic to stdout, so that is where the child's is.
        let child = child.wait_with_output().unwrap();
        assert!(
            child.status.success(),
            "the second process failed:\n{}",
            String::from_utf8_lossy(&child.stdout)
        );

        let counts: Vec<(String, i64)> = rusqlite::Connection::open(file.path())
            .unwrap()
            .prepare("SELECT origin, count(*) FROM history GROUP BY origin ORDER BY origin")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            counts,
            [
                ("console".to_owned(), WRITES_PER_PROCESS),
                ("mcp".to_owned(), WRITES_PER_PROCESS)
            ]
        );
    }
}
