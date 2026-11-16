//! A DuckDB database of one report run's own (`docs/reporting.md`, "RP2 — Several sources"):
//! on a file in the run's work folder (`docs/reports-at-scale.md`, RS3b), or in memory, with its
//! memory, threads and spilling bounded, the run's sources loaded into its tables, DuckDB files
//! attached to it read-only, and then locked down for the query that combines them.
//!
//! A run goes through it in this order:
//!
//! 1. [`Workspace::open`] makes the database and sets its limits;
//! 2. [`Workspace::attach`] attaches each DuckDB file, and [`Workspace::loader`] loads each
//!    other source: into a table of the types known before its first row, a page of text at a
//!    time, which DuckDB casts as it appends; then each other column is given the type its
//!    values chose (`docs/reports-at-scale.md`, RS3c);
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
use std::time::SystemTime;

use dbrover_core::{
    ByteCount, Cell, Connection, DriverError, DuckDbConfig, DuckDbDatabase, LakeFormat, LakePath,
    ObjectStorage, Row,
};
use duckdb::arrow::array::{ArrayRef, StringArray};
use duckdb::arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use duckdb::arrow::record_batch::RecordBatch;

use crate::errors::statement_error;
use crate::lake::{BucketSignIn, DuckDbExtension, LakeView};
use crate::parquet::ParquetCompression;
use crate::{DuckDbConnection, connection_error, poisoned};

/// The schema a source's table is made in, which the combine query reads unqualified.
pub(crate) const MAIN_SCHEMA: &str = "main";

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

/// DuckDB's name for its log of HTTP requests, which is the only kind a workspace keeps.
const HTTP_LOG_TYPE: &str = "HTTP";

/// What goes before a query to have DuckDB plan it and write the plan as JSON, which
/// `dbrover-plan` reads.
const EXPLAIN_JSON: &str = "EXPLAIN (FORMAT JSON) ";

/// The column of `EXPLAIN`'s answer holding the plan, after `explain_key`.
const EXPLAIN_VALUE_COLUMN: usize = 1;

/// Where a workspace keeps its tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceStore {
    /// In memory, for a workspace whose tables stay small, such as one that explains a report
    /// over empty tables.
    Memory,
    /// In a new database file at this path, which must not exist yet: tables larger than
    /// `memory_limit` are written out to it as any database's pages are, not spilled. Whoever
    /// opened the workspace removes the file after [`Workspace::close`].
    File(PathBuf),
    /// In the database file at this path that a stopped run kept (RS5e), opened as it was left:
    /// the tables its finished sources loaded are there, and DuckDB replays what its log holds.
    Kept(PathBuf),
}

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

/// What a handoff's files became: the table's rows, and each column with the type name DuckDB
/// gave it, which is the files' own rather than one the run's loading chose (RS7f).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HandoffLoaded {
    pub rows: u64,
    /// Each column's name and DuckDB type name, in the table's order.
    pub columns: Vec<(String, String)>,
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

/// What a bucket answered a workspace's requests with (RS7e).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BucketTraffic {
    pub requests: u64,
    /// The bytes the answers carried, as their `Content-Length` says.
    pub bytes: u64,
}

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

/// What a locked workspace may still reach ([`Workspace::lock`]): nothing, by default.
///
/// The two are one list to DuckDB (`allowed_directories`), and both are only bounds once
/// external access is off: with it on, the list holds nothing back
/// (`docs/adr/0008-a-locked-workspace-reads-a-named-bucket.md`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Allowed<'a> {
    /// Folders it may write files in, and read them back from: the run's staging folder, for
    /// the Parquet files DuckDB writes itself (RS3d), and the work folder's parts of a
    /// partitioned run (RS5b).
    pub folders: &'a [&'a Path],
    /// Prefixes in a bucket it may read under, each ending in `/`: one per lake the report
    /// named, as [`LakeView::prefix`] gives it (RS7).
    pub lakes: &'a [&'a str],
}

impl<'a> Allowed<'a> {
    /// What it may write files in, and nothing else.
    pub fn folders(folders: &'a [&'a Path]) -> Self {
        Self {
            folders,
            lakes: &[],
        }
    }

    /// Every allowance as a DuckDB literal, folders with the trailing separator DuckDB wants
    /// on a directory.
    fn directories(&self) -> Vec<String> {
        let folders = self.folders.iter().map(|folder| {
            let mut folder = folder.to_string_lossy().into_owned();
            if !folder.ends_with(std::path::MAIN_SEPARATOR) {
                folder.push(std::path::MAIN_SEPARATOR);
            }
            string_literal(&folder)
        });
        folders
            .chain(self.lakes.iter().map(|prefix| string_literal(prefix)))
            .collect()
    }
}

/// A DuckDB database for one run.
pub struct Workspace {
    connection: DuckDbConnection,
}

impl Workspace {
    /// Makes a database in `store` under `limits`, with extensions neither downloaded nor
    /// loaded on their own, and its time zone UTC, so times with a zone read the same on
    /// every machine.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if the file exists already or can't be made, or DuckDB refuses a limit,
    /// such as a temporary folder it can't make.
    pub async fn open(
        store: &WorkspaceStore,
        limits: &WorkspaceLimits,
    ) -> Result<Self, DriverError> {
        std::fs::create_dir_all(&limits.temp_directory).map_err(connection_error)?;
        let database = match store {
            WorkspaceStore::Memory => DuckDbDatabase::InMemory,
            WorkspaceStore::File(path) => {
                crate::create_database(path).await?;
                DuckDbDatabase::File {
                    path: path.clone(),
                    read_only: false,
                }
            }
            WorkspaceStore::Kept(path) => DuckDbDatabase::File {
                path: path.clone(),
                read_only: false,
            },
        };
        let connection = crate::connect(&DuckDbConfig::new(database)).await?;
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
            // The file is removed once the run ends, so writing its last changes into it as
            // the database closes would only cost time.
            "PRAGMA disable_checkpoint_on_shutdown".to_owned(),
            "SET autoload_known_extensions = false".to_owned(),
            // Measures without writing anywhere, so it needs no file once external access is
            // off, and only what `profile` reads, which DuckDB measures as it runs anyway.
            "PRAGMA enable_profiling = 'no_output'".to_owned(),
            format!("SET custom_profiling_settings = '{}'", profiling_settings()),
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

    /// Starts loading the table `table`, with `columns`: those of a known type in it, which
    /// DuckDB casts each value to as it is appended, and the others as text until
    /// [`TableLoader::finish`] gives them theirs.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if the table can't be made, as with two columns of one name.
    pub async fn loader(
        &self,
        table: &str,
        columns: &[LoadColumn],
    ) -> Result<TableLoader, DriverError> {
        let types: Vec<DuckDbType> = columns
            .iter()
            .map(|column| column.known.unwrap_or(DuckDbType::Varchar))
            .collect();
        let definitions: Vec<String> = columns
            .iter()
            .zip(&types)
            .map(|(column, column_type)| format!("{} {column_type}", identifier(&column.name)))
            .collect();
        let sql = format!(
            "CREATE TABLE {MAIN_SCHEMA}.{} ({})",
            identifier(table),
            definitions.join(", ")
        );
        let own = self
            .connection
            .run_blocking(move |connection| {
                connection
                    .execute_batch(&sql)
                    .map_err(|error| statement_error(error, &sql))?;
                connection.try_clone().map_err(connection_error)
            })
            .await?;
        let fields: Vec<Field> = columns
            .iter()
            .map(|column| Field::new(&column.name, DataType::Utf8, true))
            .collect();
        Ok(TableLoader {
            connection: Arc::new(Mutex::new(own)),
            table: table.to_owned(),
            columns: columns.iter().map(|column| column.name.clone()).collect(),
            types,
            text: Arc::new(Schema::new(fields)),
        })
    }

    /// Makes the table `table` from the files an engine wrote for a handoff, and answers the
    /// rows it holds (`docs/reports-at-scale.md`, RS7f).
    ///
    /// The table's columns and types are the files' own, as a columnar source's are its
    /// engine's (ADR 0007): nothing is cast on the way in, because what a warehouse wrote to
    /// Parquet already says what each column is. A handoff is loaded where every other source
    /// is, before [`Workspace::lock`], so its bucket needs no prefix in `allowed_directories`.
    ///
    /// Call it after [`Workspace::load_lake_extensions`] and
    /// [`Workspace::sign_in_to_bucket`] for the handoff's bucket, which DuckDB needs to read
    /// the files at all.
    ///
    /// # Errors
    ///
    /// [`DriverError`] when the files can't be read — a bucket the secret doesn't reach, a
    /// shard the engine wrote half of — or when they hold no columns.
    pub async fn load_handoff(
        &self,
        table: &str,
        files: &[String],
        format: LakeFormat,
    ) -> Result<HandoffLoaded, DriverError> {
        let from = crate::handoff::files_clause(files, format)
            .map_err(|problem| connection_error(format!("the handoff can't be read: {problem}")))?;
        let name = format!("{MAIN_SCHEMA}.{}", identifier(table));
        let sql = format!("CREATE OR REPLACE TABLE {name} AS SELECT * FROM {from}");
        let count = format!("SELECT count(*) FROM {name}");
        let describe = format!("DESCRIBE {name}");
        self.connection
            .run_blocking(move |connection| {
                connection
                    .execute_batch(&sql)
                    .map_err(|error| statement_error(error, &sql))?;
                let rows = connection
                    .query_row(&count, [], |row| row.get::<_, i64>(0))
                    .map_err(|error| statement_error(error, &count))?;
                let mut statement = connection
                    .prepare(&describe)
                    .map_err(|error| statement_error(error, &describe))?;
                let columns = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                    .map_err(|error| statement_error(error, &describe))?;
                Ok(HandoffLoaded {
                    rows: rows.try_into().unwrap_or_default(),
                    columns,
                })
            })
            .await
    }

    /// Drops the table `table`, if there is one: a source's table that a stopped run left half
    /// loaded, which a run resuming it loads again (RS5e).
    ///
    /// # Errors
    ///
    /// [`DriverError`] if DuckDB refuses.
    pub async fn drop_table(&self, table: &str) -> Result<(), DriverError> {
        let sql = format!("DROP TABLE IF EXISTS {MAIN_SCHEMA}.{}", identifier(table));
        self.connection
            .run_blocking(move |connection| {
                connection
                    .execute_batch(&sql)
                    .map_err(|error| statement_error(error, &sql))
            })
            .await
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

    /// Loads the extensions a lake in `storage`, read as `format`, needs: the provider's file
    /// system, and the table format's reader where it is one
    /// (`docs/reports-at-scale.md`, RS7).
    ///
    /// Call it first of all, before [`Workspace::sign_in_to_bucket`], which DuckDB refuses
    /// until the provider's extension holds its secret's type, and before
    /// [`Workspace::lock`], which loads none.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] when an extension can't be downloaded or loaded, which is
    /// what an offline machine gets.
    pub async fn load_lake_extensions(
        &self,
        storage: &ObjectStorage,
        format: LakeFormat,
    ) -> Result<(), DriverError> {
        for extension in DuckDbExtension::of(storage, format) {
            let name = extension.name();
            let sql = format!("INSTALL {name}; LOAD {name}");
            self.connection
                .run_blocking(move |connection| {
                    connection
                        .execute_batch(&sql)
                        .map_err(|error| crate::errors::extension_error(error, name))
                })
                .await?;
        }
        Ok(())
    }

    /// Makes the view the combine query reads a lake through, which reads it in place rather
    /// than loading it (`docs/reports-at-scale.md`, RS7).
    ///
    /// Call it after [`Workspace::load_lake_extensions`] and
    /// [`Workspace::sign_in_to_bucket`], since making the view reads the files' schema, and
    /// before [`Workspace::lock`], whose [`Allowed`] must hold the lake's
    /// [`LakeView::prefix`]: a locked database reads no prefix it wasn't given.
    ///
    /// # Errors
    ///
    /// [`DriverError`] when DuckDB refuses the view, as for a path whose files it can't read.
    pub async fn read_lake(&self, lake: &LakeView) -> Result<(), DriverError> {
        let sql = lake.statement();
        self.connection
            .run_blocking(move |connection| {
                connection
                    .execute_batch(&sql)
                    .map_err(|error| statement_error(error, &sql))
            })
            .await
    }

    /// Starts DuckDB's own log of the requests it makes to a bucket, and nothing else, so a
    /// run can say what its lakes' scans read (RS7e).
    ///
    /// Call it before [`Workspace::lock`], which freezes the settings. The log is kept in
    /// DuckDB's memory, one row a request, and goes with the workspace.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if DuckDB refuses a setting.
    pub async fn log_bucket_requests(&self) -> Result<(), DriverError> {
        let sql = format!(
            "SET logging_mode = 'ENABLE_SELECTED'; SET enabled_log_types = '{HTTP_LOG_TYPE}'; \
             SET logging_level = 'trace'; SET enable_logging = true"
        );
        self.connection
            .run_blocking(move |connection| {
                connection
                    .execute_batch(&sql)
                    .map_err(|error| statement_error(error, &sql))
            })
            .await
    }

    /// What the bucket's requests for the keys under `key_prefix` have moved since
    /// [`Workspace::log_bucket_requests`]: how many requests, and the bytes their answers
    /// carried, as each answer's `Content-Length` says.
    ///
    /// The key's prefix is matched inside the request's address, because that address is the
    /// provider's own (`https://bucket.s3.region.amazonaws.com/sales/…`, or an endpoint's
    /// `http://host/bucket/sales/…`) rather than the `s3://` one the lake is written as. Two
    /// lakes whose paths nest (`sales/` and `sales/2026/`) each count the inner one's
    /// requests.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if the log can't be read, which is what a DuckDB without the logging
    /// tables would give.
    pub async fn bucket_requests(
        &mut self,
        key_prefix: &str,
    ) -> Result<BucketTraffic, DriverError> {
        let sql = format!(
            "SELECT count(*)::VARCHAR, \
             coalesce(sum(TRY_CAST(response.headers['Content-Length'] AS BIGINT)), 0)::VARCHAR \
             FROM duckdb_logs_parsed('{HTTP_LOG_TYPE}') WHERE request.url LIKE {}",
            string_literal(&format!("%{}%", like_pattern(key_prefix)))
        );
        let mut cursor = self.connection.execute(&sql).await?;
        let mut rows = Vec::new();
        while !cursor.is_exhausted() {
            rows.extend(cursor.next_page(1).await?);
        }
        cursor.finish().await?;
        let number = |index: usize| {
            rows.first()
                .and_then(|row: &Row| row.get(index))
                .and_then(Option::as_deref)
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default()
        };
        Ok(BucketTraffic {
            requests: number(0),
            bytes: number(1),
        })
    }

    /// The bytes a lake's files hold, read from their Parquet footers, which is the most a
    /// query over it can read: DuckDB's filters, row-group statistics and Hive partitions make
    /// the read smaller, never larger (RS7).
    ///
    /// `None` for a lake DuckDB has no footers to read, which is a CSV or JSON one. Call it
    /// before [`Workspace::lock`], with the bucket signed in to; it reads each file's footer
    /// and no rows.
    ///
    /// # Errors
    ///
    /// [`DriverError`] when the files can't be read, as for credentials the store refuses.
    pub async fn lake_bytes(
        &mut self,
        path: &LakePath,
        format: LakeFormat,
    ) -> Result<Option<ByteCount>, DriverError> {
        let Some(glob) = LakeView::footer_glob(path, format) else {
            return Ok(None);
        };
        // A lake of no files at all is no bytes, not an error: the combine's own read says so.
        let sql = format!(
            "SELECT coalesce(sum(total_compressed_size), 0)::VARCHAR FROM parquet_metadata({})",
            string_literal(&glob)
        );
        let mut cursor = self.connection.execute(&sql).await?;
        let mut rows = Vec::new();
        while !cursor.is_exhausted() {
            rows.extend(cursor.next_page(1).await?);
        }
        cursor.finish().await?;
        let bytes = rows
            .first()
            .and_then(|row| row.first())
            .and_then(Option::as_deref)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| connection_error(format!("could not read the footers of {glob}")))?;
        Ok(Some(ByteCount(bytes)))
    }

    /// Hands DuckDB the bucket's credentials, from the data source's C2 sign-in, if it holds
    /// none that is still fresh.
    ///
    /// Call it before [`Workspace::lock`] for the first secret, and again while the combine
    /// runs to replace credentials that are about to expire: a locked database still takes a
    /// secret, which is the one setting the lock leaves open. The statement holds the
    /// credentials, so it is DbRover's own and reaches neither the history nor the log.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] when signing in to the bucket fails, or DuckDB refuses the
    /// secret, as it does before the bucket's extension is loaded.
    pub async fn sign_in_to_bucket(&self, sign_in: &mut BucketSignIn) -> Result<(), DriverError> {
        let Some(statement) = sign_in
            .due_secret(SystemTime::now())
            .await
            .map_err(connection_error)?
        else {
            return Ok(());
        };
        self.connection
            .run_blocking(move |connection| {
                connection
                    .execute_batch(statement.expose())
                    .map_err(crate::errors::secret_error)
            })
            .await?;
        sign_in.made();
        Ok(())
    }

    /// Cuts the database off from everything outside it, files and extensions included, and
    /// freezes its settings, so the query that runs next reads only what was loaded,
    /// attached and `allowed`, within the limits it was opened with.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if DuckDB refuses a setting.
    pub async fn lock(&self, allowed: &Allowed<'_>) -> Result<(), DriverError> {
        // DuckDB takes `allowed_directories` only while external access is on, and it bounds
        // a bucket's prefixes only once external access is off (ADR 0008).
        let allowed = allowed.directories();
        let allowed = if allowed.is_empty() {
            String::new()
        } else {
            format!("SET allowed_directories = [{}]; ", allowed.join(", "))
        };
        let sql =
            format!("{allowed}SET enable_external_access = false; SET lock_configuration = true");
        self.connection
            .run_blocking(move |connection| {
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

/// A column of a table about to be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadColumn {
    pub name: String,
    /// Its type, when known before its first value: declared in the report's file, or a type
    /// its driver states exactly. `None` loads it as text, for its values to choose.
    pub known: Option<DuckDbType>,
}

impl LoadColumn {
    /// A column whose values choose its type.
    pub fn text(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            known: None,
        }
    }
}

/// A table being loaded, on a connection of its own, so tables load at the same time.
pub struct TableLoader {
    connection: Arc<Mutex<duckdb::Connection>>,
    table: String,
    columns: Vec<String>,
    /// Each column's type in the table now.
    types: Vec<DuckDbType>,
    /// The page's shape as appended: every column text, which DuckDB casts to the table's.
    text: SchemaRef,
}

impl TableLoader {
    /// Appends `rows`, each value text, to the table, as one batch: DuckDB casts each value to
    /// its column's type.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if a row hasn't as many values as the table has columns, a value doesn't
    /// cast to its column's type, or DuckDB runs out of memory.
    pub async fn append(&mut self, rows: Vec<Row>) -> Result<(), DriverError> {
        if rows.is_empty() {
            return Ok(());
        }
        let batch = text_batch(&self.text, &rows)?;
        let connection = Arc::clone(&self.connection);
        let table = self.table.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection.lock().map_err(|_| poisoned())?;
            let mut appender = connection
                .appender_to_db(&table, MAIN_SCHEMA)
                .map_err(connection_error)?;
            match appender.append_record_batch(batch) {
                Ok(()) => appender.flush().map_err(connection_error),
                // DuckDB says only that the batch's text didn't cast to some column's type,
                // and appended none of it. Row by row, its error names the value.
                Err(error) => {
                    tracing::debug!("appending a page as one batch failed: {error}");
                    drop(appender);
                    append_rows(&connection, &table, &rows)
                }
            }
        })
        .await
        .map_err(connection_error)?
    }

    /// Appends `batch`, an engine's own Arrow batch of the table's columns in their order, as
    /// it is (ADR 0007): DuckDB casts a column whose type isn't its table column's, as text
    /// into a column still loading as text.
    ///
    /// # Errors
    ///
    /// [`DriverError`] if the batch hasn't as many columns as the table, a value doesn't cast
    /// to its column's type, or DuckDB runs out of memory.
    pub async fn append_batch(&mut self, batch: RecordBatch) -> Result<(), DriverError> {
        if batch.num_columns() != self.columns.len() {
            return Err(connection_error(format!(
                "a batch of {} columns for a table of {}",
                batch.num_columns(),
                self.columns.len()
            )));
        }
        if batch.num_rows() == 0 {
            return Ok(());
        }
        let connection = Arc::clone(&self.connection);
        let table = self.table.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection.lock().map_err(|_| poisoned())?;
            let mut appender = connection
                .appender_to_db(&table, MAIN_SCHEMA)
                .map_err(connection_error)?;
            appender
                .append_record_batch(batch)
                .map_err(connection_error)?;
            appender.flush().map_err(connection_error)
        })
        .await
        .map_err(connection_error)?
    }

    /// Gives each column its type in `types`, in the columns' order: a column loaded as text
    /// whose values chose another type is rewritten, alone; the rest of the table stays as it
    /// was loaded.
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
        let table = format!("{MAIN_SCHEMA}.{}", identifier(&self.table));
        let changes: Vec<String> = self
            .columns
            .iter()
            .zip(self.types.iter().zip(types))
            .filter(|(_, (loaded, wanted))| loaded != wanted)
            .map(|(column, (_, wanted))| {
                let column = identifier(column);
                format!("ALTER TABLE {table} ALTER {column} TYPE {wanted} USING CAST({column} AS {wanted})")
            })
            .collect();
        if changes.is_empty() {
            return Ok(());
        }
        let sql = changes.join("; ");
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

/// Appends `rows` one at a time, for the error DuckDB gives of the first value that doesn't
/// cast to its column's type.
fn append_rows(
    connection: &duckdb::Connection,
    table: &str,
    rows: &[Row],
) -> Result<(), DriverError> {
    let mut appender = connection
        .appender_to_db(table, MAIN_SCHEMA)
        .map_err(connection_error)?;
    for row in rows {
        appender
            .append_row(duckdb::appender_params_from_iter(
                row.iter().map(Option::as_deref),
            ))
            .map_err(connection_error)?;
    }
    appender.flush().map_err(connection_error)
}

/// `rows` as one batch of text columns in `schema`'s shape.
fn text_batch(schema: &SchemaRef, rows: &[Row]) -> Result<RecordBatch, DriverError> {
    let width = schema.fields().len();
    if let Some(row) = rows.iter().find(|row| row.len() != width) {
        return Err(connection_error(format!(
            "a row of {} values for a table of {width} columns",
            row.len()
        )));
    }
    let columns: Vec<ArrayRef> = (0..width)
        .map(|index| {
            let values: StringArray = rows.iter().map(|row| row[index].as_deref()).collect();
            Arc::new(values) as ArrayRef
        })
        .collect();
    RecordBatch::try_new(Arc::clone(schema), columns).map_err(connection_error)
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

/// The statement that writes what `query` returns to a Parquet file at `file`, compressed
/// with `compression`, run in a workspace locked to the file's folder; it answers with the
/// rows it wrote.
pub fn copy_to_parquet(query: &str, file: &Path, compression: ParquetCompression) -> String {
    // The query goes on lines of its own, so a comment closing it can't swallow the bracket;
    // a statement's closing semicolon would end the COPY early.
    let query = query.trim_end().trim_end_matches(';');
    format!(
        "COPY (\n{query}\n) TO {} (FORMAT parquet, COMPRESSION {})",
        string_literal(&file.to_string_lossy()),
        compression.as_sql()
    )
}

/// The query that reads back every row of the Parquet file at `file`.
pub fn read_parquet(file: &Path) -> String {
    format!(
        "SELECT * FROM read_parquet({})",
        string_literal(&file.to_string_lossy())
    )
}

/// The query that reads back every row of the Parquet files `files`, one after another in their
/// order: DuckDB keeps the order it reads rows in while `preserve_insertion_order` is on, its
/// default.
pub fn read_parquet_files(files: &[PathBuf]) -> String {
    let files: Vec<String> = files
        .iter()
        .map(|file| string_literal(&file.to_string_lossy()))
        .collect();
    format!("SELECT * FROM read_parquet([{}])", files.join(", "))
}

/// `"name"`, quoted as DuckDB quotes an identifier, so any column name works.
pub(crate) fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `text` with `LIKE`'s own wildcards escaped, so a key's prefix matches itself; `\\` is the
/// escape character `LIKE` takes by default.
fn like_pattern(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// `'text'`, quoted as a SQL string.
pub(crate) fn string_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A size as DuckDB's settings take one: DuckDB reads no plain number of bytes.
fn kibibytes(bytes: u64) -> String {
    format!("{}KiB", bytes.div_ceil(1024).max(1))
}
