//! The arguments a dump program is run with, from a dump's request, the server it connects to,
//! and the files the engine has set aside for the run.

use std::path::{Path, PathBuf};

use dbrover_core::{DumpContent, DumpEngine, DumpFormat, DumpOptions, DumpScope, TableName};

use crate::connection::{ConnectionError, Target};
use crate::version::Version;

/// The files a run uses, chosen by the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFiles {
    /// The password file ([`crate::secret::password_file`]).
    pub secret: PathBuf,
    /// A `pg_dump --filter` file, for many schemas or tables.
    pub filter: PathBuf,
    /// Where the program writes the dump while it runs: the `.partial` name.
    pub output: PathBuf,
}

/// What a dump is of and how, as far as the program's arguments go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpSpec<'a> {
    pub engine: DumpEngine,
    /// The program's version, which decides the options it has.
    pub program: Version,
    /// The server's, for options only some servers need.
    pub server: Version,
    pub scope: &'a DumpScope,
    pub format: DumpFormat,
    pub options: DumpOptions,
}

/// Where the program's standard output goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stdout {
    /// The program writes the dump itself, to [`RunFiles::output`]; its stdout is only read.
    Unused,
    /// The program writes the dump to stdout, and the engine writes that to the file.
    Dump { gzip: bool },
}

/// A program's run, ready to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpCommand {
    /// Every argument, in order.
    pub arguments: Vec<String>,
    /// The arguments as **Copy command** shows them: without the ones naming the run's own
    /// secret and filter files, which are gone once it ends.
    pub shown: Vec<String>,
    /// Set on top of DbRover's environment, from which the engine first removes `PG*` and
    /// `MYSQL_*` variables.
    pub environment: Vec<(String, String)>,
    pub stdout: Stdout,
    /// The contents of [`RunFiles::filter`], when the run uses it.
    pub filter: Option<String>,
}

/// Why a dump can't be run as asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgumentError {
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error("{0} can't write or read a dump in this format")]
    Format(DumpEngine),
    #[error("{0} dumps and restores itself, with no program")]
    NoProgram(DumpEngine),
    #[error("this is a {file} dump, which can't be restored into {server}")]
    OtherEngine {
        file: DumpEngine,
        server: DumpEngine,
    },
    #[error("{0} has no schemas inside a database; choose the database or some of its tables")]
    NoSchemas(DumpEngine),
    #[error("\"{0}\" starts with a dash, which the program would read as an option")]
    LooksLikeAnOption(String),
    #[error(
        "{count} names don't fit on a command line for pg_dump {program}; pg_dump 17 or later \
         takes them in a file, or choose the whole database or its schemas"
    )]
    TooManyNames { count: usize, program: Version },
}

/// Names past which `pg_dump` 17 and later take them in a `--filter` file rather than on the
/// command line. Chosen, not measured: well under Windows' 32,767-character command line for
/// names of any usual length, and few enough that **Copy command** stays readable.
const NAMES_ON_THE_COMMAND_LINE: usize = 50;

/// What a command line may hold before Windows refuses it (32,767 characters), with room for
/// the program's path and the connection.
const COMMAND_LINE_ROOM: usize = 30_000;

/// `pg_dump` 17 added `--filter`.
const FILTER_SINCE: u32 = 17;

/// The arguments for a dump of `spec` from `target`, with the run's `files`.
///
/// # Errors
///
/// [`ArgumentError`] for a dump the program can't make as asked.
pub fn dump_command(
    spec: &DumpSpec<'_>,
    target: &Target,
    files: &RunFiles,
) -> Result<DumpCommand, ArgumentError> {
    if !spec.engine.formats().contains(&spec.format) {
        return Err(ArgumentError::Format(spec.engine));
    }
    match spec.engine {
        DumpEngine::Postgres => pg_dump(spec, target, files),
        DumpEngine::Mysql | DumpEngine::MariaDb => mysqldump(spec, target, files),
        DumpEngine::Sqlite | DumpEngine::DuckDb => Err(ArgumentError::NoProgram(spec.engine)),
    }
}

fn pg_dump(
    spec: &DumpSpec<'_>,
    target: &Target,
    files: &RunFiles,
) -> Result<DumpCommand, ArgumentError> {
    let mut arguments = vec![
        format!("--dbname={}", target.conninfo(spec.program)?),
        // Never wait for a password on a terminal a GUI app doesn't have.
        "--no-password".to_owned(),
        "--verbose".to_owned(),
    ];
    let format = match spec.format {
        DumpFormat::Custom => "custom",
        DumpFormat::Directory => "directory",
        DumpFormat::Sql | DumpFormat::SqlGz => "plain",
        DumpFormat::DatabaseFile | DumpFormat::Parquet | DumpFormat::Csv => {
            return Err(ArgumentError::Format(spec.engine));
        }
    };
    arguments.push(format!("--format={format}"));
    let stdout = match spec.format {
        DumpFormat::Custom | DumpFormat::Directory => {
            arguments.push(format!("--file={}", files.output.display()));
            Stdout::Unused
        }
        DumpFormat::Sql => Stdout::Dump { gzip: false },
        DumpFormat::SqlGz => Stdout::Dump { gzip: true },
        DumpFormat::DatabaseFile | DumpFormat::Parquet | DumpFormat::Csv => {
            return Err(ArgumentError::Format(spec.engine));
        }
    };
    if spec.format == DumpFormat::Directory && spec.options.jobs.get() > 1 {
        arguments.push(format!("--jobs={}", spec.options.jobs));
    }
    arguments.extend(content_flag(spec.options.content).map(str::to_owned));
    if spec.options.leave_out_owners {
        arguments.push("--no-owner".to_owned());
    }
    if spec.options.leave_out_privileges {
        arguments.push("--no-privileges".to_owned());
    }
    let mut shown = arguments.clone();
    let names = scope_patterns(spec.scope);
    let mut filter = None;
    if !names.is_empty() {
        let use_filter =
            names.len() > NAMES_ON_THE_COMMAND_LINE && spec.program.major >= FILTER_SINCE;
        if use_filter {
            filter = Some(filter_file(&names));
            arguments.push(format!("--filter={}", files.filter.display()));
            shown.push(format!("--filter=<{} names>", names.len()));
        } else {
            let options: Vec<String> = names.iter().map(Pattern::option).collect();
            let length: usize = options.iter().map(String::len).sum();
            if length > COMMAND_LINE_ROOM {
                return Err(ArgumentError::TooManyNames {
                    count: names.len(),
                    program: spec.program,
                });
            }
            arguments.extend(options.iter().cloned());
            shown.extend(options);
        }
        // A table dropped since it was picked fails the dump rather than being left out.
        arguments.push("--strict-names".to_owned());
        shown.push("--strict-names".to_owned());
    }
    Ok(DumpCommand {
        arguments,
        shown,
        environment: vec![(
            "PGPASSFILE".to_owned(),
            files.secret.to_string_lossy().into_owned(),
        )],
        stdout,
        filter,
    })
}

fn content_flag(content: DumpContent) -> Option<&'static str> {
    match content {
        DumpContent::SchemaAndData => None,
        DumpContent::SchemaOnly => Some("--schema-only"),
        DumpContent::DataOnly => Some("--data-only"),
    }
}

/// A schema or table named in a `pg_dump` pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    Schema(String),
    Table(String),
}

impl Pattern {
    fn option(&self) -> String {
        match self {
            Self::Schema(pattern) => format!("--schema={pattern}"),
            Self::Table(pattern) => format!("--table={pattern}"),
        }
    }

    fn filter_line(&self) -> String {
        match self {
            Self::Schema(pattern) => format!("include schema {pattern}"),
            Self::Table(pattern) => format!("include table {pattern}"),
        }
    }
}

fn scope_patterns(scope: &DumpScope) -> Vec<Pattern> {
    match scope {
        DumpScope::Database => Vec::new(),
        DumpScope::Schemas(schemas) => schemas
            .iter()
            .map(|schema| Pattern::Schema(quoted(schema)))
            .collect(),
        DumpScope::Tables(tables) => tables
            .iter()
            .map(|table| Pattern::Table(table_pattern(table)))
            .collect(),
    }
}

/// `"schema"."table"`: inside double quotes, a `pg_dump` pattern's `*`, `?`, `.` and capitals
/// are matched as themselves, and a `"` is written twice, as psql's patterns read them.
fn table_pattern(table: &TableName) -> String {
    format!("{}.{}", quoted(&table.schema), quoted(&table.name))
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn filter_file(names: &[Pattern]) -> String {
    names.iter().map(|name| name.filter_line() + "\n").collect()
}

fn mysqldump(
    spec: &DumpSpec<'_>,
    target: &Target,
    files: &RunFiles,
) -> Result<DumpCommand, ArgumentError> {
    let tables = match spec.scope {
        DumpScope::Database => Vec::new(),
        DumpScope::Tables(tables) => tables.iter().map(|table| table.name.clone()).collect(),
        DumpScope::Schemas(_) => return Err(ArgumentError::NoSchemas(spec.engine)),
    };
    let positional: Vec<String> = std::iter::once(target.database.clone())
        .chain(tables)
        .collect();
    if let Some(dashed) = positional.iter().find(|name| name.starts_with('-')) {
        return Err(ArgumentError::LooksLikeAnOption(dashed.clone()));
    }
    let mut shown = target.mysql_options(spec.engine);
    shown.extend(
        [
            // A consistent read of InnoDB tables, without locking them.
            "--single-transaction",
            "--hex-blob",
            "--default-character-set=utf8mb4",
            // Tablespaces are the server's, not a database's, and MySQL 8 asks for the
            // server-wide PROCESS privilege to dump them, which a database's own login lacks.
            "--no-tablespaces",
            "--verbose",
        ]
        .map(str::to_owned),
    );
    match spec.options.content {
        DumpContent::SchemaAndData => shown.extend(definitions()),
        DumpContent::SchemaOnly => {
            shown.extend(definitions());
            shown.push("--no-data".to_owned());
        }
        DumpContent::DataOnly => {
            shown.extend(["--no-create-info", "--skip-triggers"].map(str::to_owned));
        }
    }
    if spec.engine == DumpEngine::Mysql {
        // A dump of a server with GTIDs on (RDS among them) restores on one without, by a
        // login without SUPER.
        shown.push("--set-gtid-purged=OFF".to_owned());
        // MySQL 8's mysqldump reads information_schema.COLUMN_STATISTICS, which 5.7 lacks.
        if spec.program.major >= 8 && spec.server.major < 8 {
            shown.push("--column-statistics=0".to_owned());
        }
    }
    shown.extend(positional);
    // `--defaults-file` must come first, or the clients refuse it.
    let arguments = std::iter::once(format!("--defaults-file={}", files.secret.display()))
        .chain(shown.iter().cloned())
        .collect();
    Ok(DumpCommand {
        arguments,
        shown,
        environment: Vec::new(),
        stdout: Stdout::Dump {
            gzip: spec.format == DumpFormat::SqlGz,
        },
        filter: None,
    })
}

fn definitions() -> [String; 3] {
    ["--routines", "--triggers", "--events"].map(str::to_owned)
}

/// A command as a POSIX shell would read it, for **Copy command** and the log: each argument
/// that isn't plain in single quotes, `'` written `'\''`.
pub fn command_line(program: &Path, arguments: &[String]) -> String {
    std::iter::once(program.to_string_lossy().into_owned())
        .chain(arguments.iter().cloned())
        .map(|argument| shell_quoted(&argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quoted(argument: &str) -> String {
    let plain = !argument.is_empty()
        && argument
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if plain {
        argument.to_owned()
    } else {
        format!("'{}'", argument.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU8;

    use dbrover_core::TlsMode;

    use super::*;

    fn target() -> Target {
        Target {
            host: "db".to_owned(),
            port: 5432,
            user: "ada".to_owned(),
            database: "shop".to_owned(),
            tls: TlsMode::Prefer,
            tunnel_port: None,
        }
    }

    fn files() -> RunFiles {
        RunFiles {
            secret: "/run/dbrover/secret".into(),
            filter: "/run/dbrover/filter".into(),
            output: "/dumps/shop.dump.partial".into(),
        }
    }

    fn spec(engine: DumpEngine, scope: &DumpScope, format: DumpFormat) -> DumpSpec<'_> {
        DumpSpec {
            engine,
            program: Version::new(17, 2, 0),
            server: Version::new(17, 4, 0),
            scope,
            format,
            options: DumpOptions::default(),
        }
    }

    const CONNINFO: &str = "--dbname=host=db port=5432 user=ada dbname=shop sslmode=prefer \
                            application_name='DbRover dump'";

    #[test]
    fn dumps_a_whole_postgres_database_in_custom_format() {
        let command = dump_command(
            &spec(
                DumpEngine::Postgres,
                &DumpScope::Database,
                DumpFormat::Custom,
            ),
            &target(),
            &files(),
        )
        .unwrap();
        assert_eq!(
            command.arguments,
            [
                CONNINFO,
                "--no-password",
                "--verbose",
                "--format=custom",
                "--file=/dumps/shop.dump.partial"
            ]
        );
        assert_eq!(command.shown, command.arguments);
        assert_eq!(
            command.environment,
            [("PGPASSFILE".to_owned(), "/run/dbrover/secret".to_owned())]
        );
        assert_eq!(command.stdout, Stdout::Unused);
    }

    #[test]
    fn quotes_the_tables_it_names_and_insists_they_exist() {
        let scope = DumpScope::Tables(vec![
            TableName {
                schema: "public".to_owned(),
                name: "Order \"items\"*".to_owned(),
            },
            TableName {
                schema: "a.b".to_owned(),
                name: "c".to_owned(),
            },
        ]);
        let mut schema_only = spec(DumpEngine::Postgres, &scope, DumpFormat::SqlGz);
        schema_only.options.content = DumpContent::SchemaOnly;
        schema_only.options.leave_out_owners = true;
        let command = dump_command(&schema_only, &target(), &files()).unwrap();
        assert_eq!(
            command.arguments[3..],
            [
                "--format=plain",
                "--schema-only",
                "--no-owner",
                "--table=\"public\".\"Order \"\"items\"\"*\"",
                "--table=\"a.b\".\"c\"",
                "--strict-names"
            ]
        );
        assert_eq!(command.stdout, Stdout::Dump { gzip: true });
    }

    #[test]
    fn many_names_go_in_a_filter_file_on_pg_dump_17() {
        let tables: Vec<TableName> = (0..60)
            .map(|n| TableName {
                schema: "public".to_owned(),
                name: format!("t{n}"),
            })
            .collect();
        let scope = DumpScope::Tables(tables);
        let command = dump_command(
            &spec(DumpEngine::Postgres, &scope, DumpFormat::Custom),
            &target(),
            &files(),
        )
        .unwrap();
        assert!(
            command
                .arguments
                .contains(&"--filter=/run/dbrover/filter".to_owned())
        );
        assert!(command.shown.contains(&"--filter=<60 names>".to_owned()));
        let filter = command.filter.unwrap();
        assert!(
            filter.starts_with("include table \"public\".\"t0\"\n"),
            "{filter}"
        );
        assert_eq!(filter.lines().count(), 60);

        let mut older = spec(DumpEngine::Postgres, &scope, DumpFormat::Custom);
        older.program = Version::new(16, 4, 0);
        let on_the_line = dump_command(&older, &target(), &files()).unwrap();
        assert!(on_the_line.filter.is_none());
        assert!(
            on_the_line
                .arguments
                .contains(&"--table=\"public\".\"t59\"".to_owned())
        );
    }

    #[test]
    fn directory_dumps_take_jobs() {
        let mut directory = spec(
            DumpEngine::Postgres,
            &DumpScope::Database,
            DumpFormat::Directory,
        );
        directory.options.jobs = NonZeroU8::new(4).unwrap();
        let command = dump_command(&directory, &target(), &files()).unwrap();
        assert!(command.arguments.contains(&"--format=directory".to_owned()));
        assert!(command.arguments.contains(&"--jobs=4".to_owned()));
    }

    #[test]
    fn dumps_a_mysql_database_with_its_password_file_first() {
        let mut mysql_target = target();
        mysql_target.port = 3306;
        let command = dump_command(
            &spec(DumpEngine::Mysql, &DumpScope::Database, DumpFormat::Sql),
            &mysql_target,
            &files(),
        )
        .unwrap();
        assert_eq!(command.arguments[0], "--defaults-file=/run/dbrover/secret");
        assert_eq!(command.arguments[1..], command.shown);
        assert_eq!(
            command.shown,
            [
                "--host=db",
                "--port=3306",
                "--user=ada",
                "--protocol=TCP",
                "--ssl-mode=PREFERRED",
                "--single-transaction",
                "--hex-blob",
                "--default-character-set=utf8mb4",
                "--no-tablespaces",
                "--verbose",
                "--routines",
                "--triggers",
                "--events",
                "--set-gtid-purged=OFF",
                "shop"
            ]
        );
        assert!(command.environment.is_empty());
        assert_eq!(command.stdout, Stdout::Dump { gzip: false });
    }

    #[test]
    fn dumps_some_mariadb_tables_data_only() {
        let scope = DumpScope::Tables(vec![TableName {
            schema: "shop".to_owned(),
            name: "orders".to_owned(),
        }]);
        let mut data_only = spec(DumpEngine::MariaDb, &scope, DumpFormat::SqlGz);
        data_only.options.content = DumpContent::DataOnly;
        let command = dump_command(&data_only, &target(), &files()).unwrap();
        let tail = &command.shown[command.shown.len() - 5..];
        assert_eq!(
            tail,
            [
                "--verbose",
                "--no-create-info",
                "--skip-triggers",
                "shop",
                "orders"
            ]
        );
        assert!(
            !command
                .shown
                .iter()
                .any(|argument| argument.starts_with("--set-gtid"))
        );
        assert!(!command.shown.contains(&"--routines".to_owned()));
        assert_eq!(command.stdout, Stdout::Dump { gzip: true });
    }

    #[test]
    fn refuses_what_a_mysql_client_cant_do() {
        let schemas = DumpScope::Schemas(vec!["shop".to_owned()]);
        assert_eq!(
            dump_command(
                &spec(DumpEngine::Mysql, &schemas, DumpFormat::Sql),
                &target(),
                &files()
            ),
            Err(ArgumentError::NoSchemas(DumpEngine::Mysql))
        );
        assert_eq!(
            dump_command(
                &spec(DumpEngine::Mysql, &DumpScope::Database, DumpFormat::Custom),
                &target(),
                &files()
            ),
            Err(ArgumentError::Format(DumpEngine::Mysql))
        );
        let dashed = DumpScope::Tables(vec![TableName {
            schema: "shop".to_owned(),
            name: "-x".to_owned(),
        }]);
        assert_eq!(
            dump_command(
                &spec(DumpEngine::Mysql, &dashed, DumpFormat::Sql),
                &target(),
                &files()
            ),
            Err(ArgumentError::LooksLikeAnOption("-x".to_owned()))
        );
    }

    #[test]
    fn adds_column_statistics_only_for_mysql_8_against_5_7() {
        let mut old_server = spec(DumpEngine::Mysql, &DumpScope::Database, DumpFormat::Sql);
        old_server.program = Version::new(8, 4, 3);
        old_server.server = Version::new(5, 7, 44);
        let command = dump_command(&old_server, &target(), &files()).unwrap();
        assert!(command.shown.contains(&"--column-statistics=0".to_owned()));
    }

    #[test]
    fn shows_a_command_a_shell_reads_back() {
        assert_eq!(
            command_line(
                Path::new("/opt/homebrew/opt/libpq/bin/pg_dump"),
                &[
                    "--dbname=host=db application_name='DbRover dump'".to_owned(),
                    "--format=custom".to_owned(),
                    String::new()
                ]
            ),
            r"/opt/homebrew/opt/libpq/bin/pg_dump '--dbname=host=db application_name='\''DbRover dump'\''' --format=custom ''"
        );
    }
}
