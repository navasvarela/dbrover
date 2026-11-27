//! Dumping a database to a file with the engine's own program (W17, `docs/dump-restore.md`):
//! what to dump, in which format, with which options. The programs themselves, and the
//! arguments they are given, are `dbrover-dump`'s.

use std::fmt;
use std::num::NonZeroU8;
use std::path::PathBuf;

use crate::{ConnectionConfig, DataSourceId, ManagedService, ServerInfo, TableName};

/// An engine DbRover can dump and restore, which decides the programs and formats offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DumpEngine {
    Postgres,
    Mysql,
    MariaDb,
}

/// What `ServerInfo::product` says for a MariaDB server, as the MySQL driver reports it.
const MARIADB_PRODUCT: &str = "MariaDB";

impl DumpEngine {
    /// The engine a data source's server is, or `None` where W17 doesn't dump: other engines,
    /// and Redshift, which speaks PostgreSQL's protocol but whose catalog `pg_dump` can't read.
    pub fn of(config: &ConnectionConfig, server: &ServerInfo) -> Option<Self> {
        match config {
            ConnectionConfig::Postgres(_) if server.service == Some(ManagedService::Redshift) => {
                None
            }
            ConnectionConfig::Postgres(_) => Some(Self::Postgres),
            ConnectionConfig::Mysql(_) if server.product == MARIADB_PRODUCT => Some(Self::MariaDb),
            ConnectionConfig::Mysql(_) => Some(Self::Mysql),
            _ => None,
        }
    }

    /// The formats a dump of this engine can be written in, the default first.
    pub fn formats(self) -> &'static [DumpFormat] {
        match self {
            Self::Postgres => &[
                DumpFormat::Custom,
                DumpFormat::Directory,
                DumpFormat::Sql,
                DumpFormat::SqlGz,
            ],
            Self::Mysql | Self::MariaDb => &[DumpFormat::Sql, DumpFormat::SqlGz],
        }
    }

    /// Whether a dump can be limited to some schemas. MySQL's databases are its schemas, so a
    /// MySQL dump is of one database, or some of its tables.
    pub fn has_schemas(self) -> bool {
        matches!(self, Self::Postgres)
    }
}

impl fmt::Display for DumpEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Postgres => "PostgreSQL",
            Self::Mysql => "MySQL",
            Self::MariaDb => "MariaDB",
        })
    }
}

/// How a dump is written. Tar is left out: it is the custom format without its compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DumpFormat {
    /// PostgreSQL's custom archive: one compressed file, any table restorable on its own.
    Custom,
    /// PostgreSQL's directory archive: a folder, written by several jobs at once.
    Directory,
    /// Plain SQL, read back by `psql` or `mysql`.
    Sql,
    /// Plain SQL, gzipped by DbRover as it is written.
    SqlGz,
}

impl DumpFormat {
    /// The file name's ending, `None` for a folder.
    pub fn extension(self) -> Option<&'static str> {
        match self {
            Self::Custom => Some("dump"),
            Self::Directory => None,
            Self::Sql => Some("sql"),
            Self::SqlGz => Some("sql.gz"),
        }
    }

    /// Whether the dump is plain SQL, gzipped or not.
    pub fn is_sql(self) -> bool {
        matches!(self, Self::Sql | Self::SqlGz)
    }

    /// What the format is good for, as the dialog says it beside the choice.
    pub fn description(self) -> &'static str {
        match self {
            Self::Custom => "Custom (.dump): one compressed file, any table restorable on its own",
            Self::Directory => "Directory: a folder, dumped by several jobs at once",
            Self::Sql => "Plain SQL (.sql): readable, restored all at once",
            Self::SqlGz => "Plain SQL, gzipped (.sql.gz)",
        }
    }
}

/// Whether a dump holds the tables' definitions, their rows, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DumpContent {
    #[default]
    SchemaAndData,
    SchemaOnly,
    DataOnly,
}

/// What a dump covers, within its database.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DumpScope {
    /// Everything the program dumps of the database.
    #[default]
    Database,
    /// These schemas, with everything in them (PostgreSQL only).
    Schemas(Vec<String>),
    /// These tables, with what they need to be created.
    Tables(Vec<TableName>),
}

/// The options a dump is written with, beside its format and content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DumpOptions {
    pub content: DumpContent,
    /// Leaves out `ALTER … OWNER TO` (PostgreSQL's `--no-owner`).
    pub leave_out_owners: bool,
    /// Leaves out `GRANT` and `REVOKE` (PostgreSQL's `--no-privileges`).
    pub leave_out_privileges: bool,
    /// Connections dumping at once, for PostgreSQL's directory format. One by default: each is
    /// another connection reading a whole table from a server others may be using.
    pub jobs: NonZeroU8,
}

impl Default for DumpOptions {
    fn default() -> Self {
        Self {
            content: DumpContent::default(),
            leave_out_owners: false,
            leave_out_privileges: false,
            jobs: NonZeroU8::MIN,
        }
    }
}

/// One dump to run: a database of a data source, written to `file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpRequest {
    pub data_source: DataSourceId,
    pub database: String,
    pub scope: DumpScope,
    pub format: DumpFormat,
    pub options: DumpOptions,
    /// The file, or the folder for [`DumpFormat::Directory`], once the dump has ended well.
    /// While it runs it is written beside it, under a `.partial` name.
    pub file: PathBuf,
}

/// Where a restore writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreTarget {
    /// A database DbRover creates first, so nothing that was there can be lost.
    NewDatabase(String),
    /// A database that exists, written into.
    Existing(String),
}

impl RestoreTarget {
    pub fn database(&self) -> &str {
        match self {
            Self::NewDatabase(name) | Self::Existing(name) => name,
        }
    }
}

/// The options a restore runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreOptions {
    /// Leaves out `ALTER … OWNER TO`, `GRANT` and `REVOKE` (and MySQL's `DEFINER`): the
    /// common case is a server without the dump's roles, where they fail.
    pub leave_out_owners: bool,
    /// One transaction, so a restore that fails or is cancelled leaves nothing (PostgreSQL;
    /// not with more than one job).
    pub all_or_nothing: bool,
    /// Drops each object before creating it (PostgreSQL's `--clean --if-exists`), only into an
    /// existing database.
    pub drop_first: bool,
    /// Connections restoring at once, for PostgreSQL's archives.
    pub jobs: NonZeroU8,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        Self {
            leave_out_owners: true,
            all_or_nothing: true,
            drop_first: false,
            jobs: NonZeroU8::MIN,
        }
    }
}

/// One restore to run: a dump file into a database of a data source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRequest {
    pub data_source: DataSourceId,
    pub file: PathBuf,
    pub target: RestoreTarget,
    /// Some of an archive's tables only; `None` for everything in the file.
    pub tables: Option<Vec<TableName>>,
    pub options: RestoreOptions,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MysqlConfig, Port, PostgresConfig, ServerConfig, SqliteConfig, TlsMode};

    fn server() -> ServerConfig {
        ServerConfig {
            host: "db".to_owned(),
            port: Port::Number(5432),
            database: "shop".to_owned(),
            user: "ada".to_owned(),
            tls: TlsMode::default(),
            ssh: None,
            password_command: None,
            cloud_sign_in: None,
        }
    }

    #[test]
    fn knows_which_servers_it_dumps() {
        let postgres = ConnectionConfig::Postgres(PostgresConfig { server: server() });
        let mysql = ConnectionConfig::Mysql(MysqlConfig { server: server() });
        let sqlite = ConnectionConfig::Sqlite(SqliteConfig {
            path: "shop.db".into(),
            read_only: false,
        });
        let redshift =
            ServerInfo::new("Redshift", "1.0.0").on_service(Some(ManagedService::Redshift));
        let cases = [
            (
                &postgres,
                ServerInfo::new("PostgreSQL", "17.4"),
                Some(DumpEngine::Postgres),
            ),
            (&postgres, redshift, None),
            (
                &mysql,
                ServerInfo::new("MySQL", "8.4.3"),
                Some(DumpEngine::Mysql),
            ),
            (
                &mysql,
                ServerInfo::new("MariaDB", "11.4.4"),
                Some(DumpEngine::MariaDb),
            ),
            (&sqlite, ServerInfo::new("SQLite", "3.46.0"), None),
        ];
        for (config, info, expected) in cases {
            assert_eq!(DumpEngine::of(config, &info), expected, "{info}");
        }
    }

    #[test]
    fn mysql_dumps_are_plain_sql() {
        assert_eq!(DumpEngine::Postgres.formats()[0], DumpFormat::Custom);
        assert!(
            DumpEngine::MariaDb
                .formats()
                .iter()
                .all(|format| format.is_sql())
        );
        assert_eq!(DumpFormat::SqlGz.extension(), Some("sql.gz"));
        assert_eq!(DumpFormat::Directory.extension(), None);
    }
}
