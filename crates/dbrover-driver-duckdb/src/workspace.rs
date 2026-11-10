//! A DuckDB database of one report run's own (`docs/reporting.md`, "RP2 — Several sources"):
//! in memory, with its memory, threads and spilling bounded, the run's sources loaded into its
//! tables, DuckDB files attached to it read-only, and then locked down for the query that
//! combines them.
//!
//! A run goes through it in this order:
//!
//! 1. [`Workspace::open`] sets its limits;
//! 2. [`Workspace::attach`] attaches each DuckDB file, and [`Workspace::loader`] loads each
//!    other source: its rows a page of text at a time, then each column cast to its type;
//! 3. [`Workspace::lock`] turns off everything outside the database and freezes the limits;
//! 4. the combine query runs on [`Workspace::connection`], as any statement on a connection;
//! 5. [`Workspace::profile`] says the most memory and disk the database used, as DuckDB's
//!    profiler measured them, and the query's plan with what each operator did.
//!
//! [`Workspace::explain`] plans a query without running it, for a report explained before it
//! runs (`docs/reports-at-scale.md`, RS2).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dbrover_core::{Cell, Connection, DriverError, DuckDbConfig, DuckDbDatabase, Row};

use crate::errors::statement_error;
use crate::{DuckDbConnection, connection_error, poisoned};

/// The schema each source's text is appended to, before its columns are cast into a table of
/// the source's name in `main`. Not a name a report's source can have, since those start with
/// a letter and this one is dropped before the combine query runs.
const LOAD_SCHEMA: &str = "dbrover_load";

/// The profiler's metrics a workspace keeps, by DuckDB's names, which [`Workspace::profile`]
/// reads. Both are the database's peaks since it opened, not the last statement's alone.
const PEAK_MEMORY_METRIC: &str = "SYSTEM_PEAK_BUFFER_MEMORY";
const PEAK_TEMP_SIZE_METRIC: &str = "SYSTEM_PEAK_TEMP_DIR_SIZE";

/// The profiler's metrics of each operator, which make the query's plan: what it is, what it
/// works on, its time and the rows it returned. DuckDB measures them as it runs anyway; asking
/// for them only keeps them.
const OPERATOR_METRICS: [&str; 6] = [
    "OPERATOR_NAME",
    "OPERATOR_TYPE",
    "OPERATOR_TIMING",
    "OPERATOR_CARDINALITY",
    "OPERATOR_ROWS_SCANNED",
    "EXTRA_INFO",
];

/// DuckDB's optimizers that plan from what a table holds: propagating its statistics, and
/// pulling an empty result up through the operators above it.
const STATISTICS_OPTIMIZERS: &str = "statistics_propagation,empty_result_pullup";

/// The query's own metrics, beside its operators': its time and the rows it returned.
const QUERY_METRICS: [&str; 2] = ["LATENCY", "ROWS_RETURNED"];

/// What goes before a query to have DuckDB plan it and write the plan as JSON, which
/// `dbrover-plan` reads.
const EXPLAIN_JSON: &str = "EXPLAIN (FORMAT JSON) ";

/// The column of `EXPLAIN`'s answer holding the plan, after `explain_key`.
const EXPLAIN_VALUE_COLUMN: usize = 1;

/// What bounds a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLimits {
    /// Bytes DuckDB may hold in memory before it spills to `temp_directory`.
    pub memory_limit: u64,
    pub threads: usize,
    /// Where DuckDB spills, made if it doesn't exist; whoever opened the workspace removes it.
    pub temp_directory: PathBuf,
    /// Bytes DuckDB may spill.
    pub max_temp_size: u64,
}

/// A limit a statement in a workspace ran into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceLimit {
    Memory,
    TempDirectory,
}

impl WorkspaceLimit {
    /// The limit `error` says the statement ran into, if it says one.
    ///
    /// DuckDB reports both as an `Out of Memory Error`; one that couldn't spill names the
    /// `max_temp_directory_size` setting.
    pub fn of(error: &DriverError) -> Option<Self> {
        let message = dbrover_core::describe_error(error);
        if message.contains("max_temp_directory_size") {
            Some(Self::TempDirectory)
        } else if message.contains("Out of Memory Error") {
            Some(Self::Memory)
        } else {
            None
        }
    }
}

/// The most a workspace has used, as DuckDB's profiler reports it after a statement; `None`
/// where this DuckDB doesn't report one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkspacePeaks {
    /// Bytes its buffer manager held at once. It can pass `memory_limit` a little: DuckDB
    /// counts some memory the limit doesn't hold back.
    pub memory: Option<u64>,
    /// Bytes it held in its temporary folder at once, which `max_temp_size` bounds.
    pub temp_size: Option<u64>,
}

/// A type as [`DuckDbType`]'s `Display` writes it, read back: the type a run loaded a column
/// as, which the history keeps by name.
impl std::str::FromStr for DuckDbType {
    type Err = UnknownDuckDbType;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let unknown = || UnknownDuckDbType(text.to_owned());
        let plain = match text {
            "BIGINT" => Some(Self::BigInt),
            "HUGEINT" => Some(Self::HugeInt),
            "DOUBLE" => Some(Self::Double),
            "BOOLEAN" => Some(Self::Boolean),
            "DATE" => Some(Self::Date),
            "TIMESTAMP" => Some(Self::Timestamp),
            "TIMESTAMP WITH TIME ZONE" => Some(Self::TimestampTz),
            "VARCHAR" => Some(Self::Varchar),
            _ => None,
        };
        if let Some(plain) = plain {
            return Ok(plain);
        }
        let (precision, scale) = text
            .strip_prefix("DECIMAL(")
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|digits| digits.split_once(','))
            .ok_or_else(unknown)?;
        let precision: u8 = precision.trim().parse().map_err(|_| unknown())?;
        let scale: u8 = scale.trim().parse().map_err(|_| unknown())?;
        if precision == 0 || precision > Self::MAX_DECIMAL_DIGITS || scale > precision {
            return Err(unknown());
        }
        Ok(Self::Decimal { precision, scale })
    }
}

/// A type name [`DuckDbType`] doesn't read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} isn't a type a report loads a column as")]
pub struct UnknownDuckDbType(pub String);

/// What the profiler measured of the last statement on [`Workspace::connection`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkspaceProfile {
    pub peaks: WorkspacePeaks,
    /// The statement's plan, each operator with its time and rows, as DuckDB's JSON profile
    /// writes it; `None` for a statement that ran no operator.
    pub plan: Option<String>,
}

/// The type a loaded column is cast to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuckDbType {
    BigInt,
    HugeInt,
    /// At most 38 digits, `scale` of them after the point.
    Decimal {
        precision: u8,
        scale: u8,
    },
    Double,
    Boolean,
    Date,
    Timestamp,
    TimestampTz,
    Varchar,
}

impl DuckDbType {
    /// The most digits a `DECIMAL` holds.
    pub const MAX_DECIMAL_DIGITS: u8 = 38;
}

/// As DuckDB writes the type, which is also how a cast names it.
impl fmt::Display for DuckDbType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BigInt => f.write_str("BIGINT"),
            Self::HugeInt => f.write_str("HUGEINT"),
            Self::Decimal { precision, scale } => write!(f, "DECIMAL({precision},{scale})"),
            Self::Double => f.write_str("DOUBLE"),
            Self::Boolean => f.write_str("BOOLEAN"),
            Self::Date => f.write_str("DATE"),
            Self::Timestamp => f.write_str("TIMESTAMP"),
            Self::TimestampTz => f.write_str("TIMESTAMP WITH TIME ZONE"),
            Self::Varchar => f.write_str("VARCHAR"),
        }
    }
}

/// A DuckDB database in memory, for one run.
pub struct Workspace {
    connection: DuckDbConnection,
}

impl Workspace {
    /// Opens a database in memory under `limits`, with extensions neither downloaded nor
    /// loaded on their own, and its time zone UTC, so times with a zone read the same on
    /// every machine.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if DuckDB refuses a limit, such as a temporary folder it can't make.
    pub async fn open(limits: &WorkspaceLimits) -> Result<Self, DriverError> {
        std::fs::create_dir_all(&limits.temp_directory).map_err(connection_error)?;
        let connection = crate::connect(&DuckDbConfig::new(DuckDbDatabase::InMemory)).await?;
        let settings = [
            format!("SET memory_limit = '{}'", kibibytes(limits.memory_limit)),
            format!("SET threads = {}", limits.threads.max(1)),
            format!(
                "SET temp_directory = {}",
                string_literal(&limits.temp_directory.to_string_lossy())
            ),
            format!(
                "SET max_temp_directory_size = '{}'",
                kibibytes(limits.max_temp_size)
            ),
            "SET autoinstall_known_extensions = false".to_owned(),
            "SET autoload_known_extensions = false".to_owned(),
            // Measures without writing anywhere, so it needs no file once external access is
            // off, and only what `profile` reads, which DuckDB measures as it runs anyway.
            "PRAGMA enable_profiling = 'no_output'".to_owned(),
            format!("SET custom_profiling_settings = '{}'", profiling_settings()),
            format!("CREATE SCHEMA {LOAD_SCHEMA}"),
        ];
        connection
            .run_blocking(move |connection| {
                for setting in &settings {
                    connection
                        .execute_batch(setting)
                        .map_err(|error| statement_error(error, setting))?;
                }
                // DuckDB built without ICU, as the bundled build is, has no time zones and
                // keeps every time in UTC already, and no such setting.
                if let Err(error) = connection.execute_batch("SET TimeZone = 'UTC'") {
                    tracing::debug!("DuckDB has no time zones to set: {error}");
                }
                Ok(())
            })
            .await?;
        Ok(Self { connection })
    }

    /// Attaches the DuckDB file at `path` read-only, as the catalog `alias`.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if the file doesn't exist, isn't a DuckDB database, or another process
    /// has it open for writing.
    pub async fn attach(&self, alias: &str, path: &Path) -> Result<(), DriverError> {
        if !path.is_file() {
            return Err(connection_error(format!(
                "{} doesn't exist",
                path.display()
            )));
        }
        let sql = format!(
            "ATTACH {} AS {} (READ_ONLY)",
            string_literal(&path.to_string_lossy()),
            identifier(alias)
        );
        self.connection
            .run_blocking(move |connection| {
                connection
                    .execute_batch(&sql)
                    .map_err(|error| statement_error(error, &sql))
            })
            .await
    }

    /// Starts loading the table `table`, with `columns`, each of text until
    /// [`TableLoader::finish`] gives it its type.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if the table can't be made, as with two columns of one name.
    pub async fn loader(
        &self,
        table: &str,
        columns: &[String],
    ) -> Result<TableLoader, DriverError> {
        let loading = format!("{LOAD_SCHEMA}.{}", identifier(table));
        let column_list: Vec<String> = columns
            .iter()
            .map(|column| format!("{} VARCHAR", identifier(column)))
            .collect();
        let sql = format!("CREATE TABLE {loading} ({})", column_list.join(", "));
        let own = self
            .connection
            .run_blocking(move |connection| {
                connection
                    .execute_batch(&sql)
                    .map_err(|error| statement_error(error, &sql))?;
                connection.try_clone().map_err(connection_error)
            })
            .await?;
        Ok(TableLoader {
            connection: Arc::new(Mutex::new(own)),
            table: table.to_owned(),
            columns: columns.to_vec(),
        })
    }

    /// The distinct values of `column` in the loaded table `table`, without `NULL`, as text,
    /// in the column's own order: a lookup's values (`docs/reporting.md`, "Lookups").
    ///
    /// # Errors
    ///
    /// [`DriverError`] if the table or the column doesn't exist, or DuckDB runs out of memory.
    pub async fn distinct_values(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Vec<String>, DriverError> {
        let column = identifier(column);
        let sql = format!(
            "SELECT CAST({column} AS VARCHAR) FROM (SELECT DISTINCT {column} FROM main.{} \
             WHERE {column} IS NOT NULL) ORDER BY {column}",
            identifier(table)
        );
        self.connection
            .run_blocking(move |connection| {
                let mut statement = connection
                    .prepare(&sql)
                    .map_err(|error| statement_error(error, &sql))?;
                let values = statement
                    .query_map([], |row| row.get::<_, String>(0))
                    .and_then(Iterator::collect)
                    .map_err(|error| statement_error(error, &sql))?;
                Ok(values)
            })
            .await
    }

    /// Has DuckDB plan as if its tables held rows, for a query explained over empty tables
    /// (RS2d). DuckDB otherwise sees from a table's statistics that it is empty, and plans a
    /// filtered scan of it as an empty result, which hides the columns and filters the query
    /// reads it with. Call it before [`Workspace::lock`], which freezes the settings.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if DuckDB refuses the setting.
    pub async fn plan_without_statistics(&self) -> Result<(), DriverError> {
        self.connection
            .run_blocking(|connection| {
                let sql = format!("SET disabled_optimizers = '{STATISTICS_OPTIMIZERS}'");
                connection
                    .execute_batch(&sql)
                    .map_err(|error| statement_error(error, &sql))
            })
            .await
    }

    /// Cuts the database off from everything outside it, files and extensions included, and
    /// freezes its settings, so the query that runs next reads only what was loaded and
    /// attached, within the limits it was opened with.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if DuckDB refuses a setting.
    pub async fn lock(&self) -> Result<(), DriverError> {
        self.connection
            .run_blocking(|connection| {
                let sql = format!(
                    "DROP SCHEMA IF EXISTS {LOAD_SCHEMA} CASCADE; \
                     SET enable_external_access = false; \
                     SET lock_configuration = true"
                );
                connection
                    .execute_batch(&sql)
                    .map_err(|error| statement_error(error, &sql))
            })
            .await
    }

    /// The most memory and disk the database has used since it opened, and the plan of the
    /// last statement on [`Workspace::connection`], once it has finished: the profiler
    /// measures a statement as it ends.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if the connection is in use or has failed.
    pub async fn profile(&self) -> Result<WorkspaceProfile, DriverError> {
        self.connection
            .run_blocking(|connection| {
                let Some(info) = connection.get_profiling_info() else {
                    return Ok(WorkspaceProfile::default());
                };
                let metric = |name: &str| info.metrics.get(name)?.parse::<u64>().ok();
                Ok(WorkspaceProfile {
                    peaks: WorkspacePeaks {
                        memory: metric(PEAK_MEMORY_METRIC),
                        temp_size: metric(PEAK_TEMP_SIZE_METRIC),
                    },
                    plan: crate::profile::to_json(&info),
                })
            })
            .await
    }

    /// How DuckDB would run `sql` with `values` bound, without running it: its plan as
    /// `EXPLAIN (FORMAT JSON)` writes it, which `dbrover-plan` reads.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if DuckDB can't plan the query, as when it names a table or column the
    /// database doesn't have.
    pub async fn explain(&mut self, sql: &str, values: &[Cell]) -> Result<String, DriverError> {
        let explained = format!("{EXPLAIN_JSON}{sql}");
        let mut cursor = self.connection.execute_bound(&explained, values).await?;
        let mut rows = Vec::new();
        while !cursor.is_exhausted() {
            rows.extend(cursor.next_page(EXPLAIN_PAGE_ROWS).await?);
        }
        cursor.finish().await?;
        rows.into_iter()
            .find_map(|mut row| row.get_mut(EXPLAIN_VALUE_COLUMN)?.take())
            .ok_or_else(|| connection_error("DuckDB's EXPLAIN answered with no plan"))
    }

    /// The database's connection, to run a query on and to cancel it.
    pub fn connection(&mut self) -> &mut DuckDbConnection {
        &mut self.connection
    }

    /// Closes the database, which frees its memory and its spilled files.
    pub async fn close(self) {
        if let Err(error) = Box::new(self.connection).close().await {
            tracing::warn!(
                "could not close the run's DuckDB: {}",
                dbrover_core::describe_error(&error)
            );
        }
    }
}

/// A table being loaded, on a connection of its own, so tables load at the same time.
pub struct TableLoader {
    connection: Arc<Mutex<duckdb::Connection>>,
    table: String,
    columns: Vec<String>,
}

impl TableLoader {
    /// Appends `rows`, each value text, to the table.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if a row hasn't as many values as the table has columns, or DuckDB
    /// runs out of memory.
    pub async fn append(&mut self, rows: Vec<Row>) -> Result<(), DriverError> {
        let connection = Arc::clone(&self.connection);
        let table = self.table.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection.lock().map_err(|_| poisoned())?;
            let mut appender = connection
                .appender_to_db(&table, LOAD_SCHEMA)
                .map_err(connection_error)?;
            for row in &rows {
                appender
                    .append_row(duckdb::appender_params_from_iter(
                        row.iter().map(Option::as_deref),
                    ))
                    .map_err(connection_error)?;
            }
            appender.flush().map_err(connection_error)
        })
        .await
        .map_err(connection_error)?
    }

    /// Makes the table, in `main`, with each column cast to its type in `types`, in the
    /// columns' order, and drops its text.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if `types` doesn't give one type per column, or a value doesn't cast
    /// to its column's type.
    pub async fn finish(self, types: &[DuckDbType]) -> Result<(), DriverError> {
        if types.len() != self.columns.len() {
            return Err(connection_error(format!(
                "{} types for the {} columns of {}",
                types.len(),
                self.columns.len(),
                self.table
            )));
        }
        let loading = format!("{LOAD_SCHEMA}.{}", identifier(&self.table));
        let casts: Vec<String> = self
            .columns
            .iter()
            .zip(types)
            .map(|(column, column_type)| {
                let column = identifier(column);
                match column_type {
                    DuckDbType::Varchar => column,
                    other => format!("CAST({column} AS {other}) AS {column}"),
                }
            })
            .collect();
        let sql = format!(
            "CREATE TABLE main.{} AS SELECT {} FROM {loading}; DROP TABLE {loading}",
            identifier(&self.table),
            casts.join(", ")
        );
        tokio::task::spawn_blocking(move || {
            let connection = self.connection.lock().map_err(|_| poisoned())?;
            connection
                .execute_batch(&sql)
                .map_err(|error| statement_error(error, &sql))
        })
        .await
        .map_err(connection_error)?
    }
}

/// Rows asked of `EXPLAIN` at a time: it answers with one.
const EXPLAIN_PAGE_ROWS: usize = 10;

/// `custom_profiling_settings`' JSON: every metric the workspace keeps, switched on.
fn profiling_settings() -> String {
    let metrics: Vec<String> = [PEAK_MEMORY_METRIC, PEAK_TEMP_SIZE_METRIC]
        .into_iter()
        .chain(QUERY_METRICS)
        .chain(OPERATOR_METRICS)
        .map(|metric| format!("\"{metric}\": \"true\""))
        .collect();
    format!("{{{}}}", metrics.join(", "))
}

/// `"name"`, quoted as DuckDB quotes an identifier, so any column name works.
fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `'text'`, quoted as a SQL string.
fn string_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A size as DuckDB's settings take one: DuckDB reads no plain number of bytes.
fn kibibytes(bytes: u64) -> String {
    format!("{}KiB", bytes.div_ceil(1024).max(1))
}
