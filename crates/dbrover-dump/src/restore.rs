//! The arguments a restore program is run with: `pg_restore` for PostgreSQL's archives, which
//! reads the file itself, and `psql` for plain SQL, which DbRover sends on its standard input,
//! gunzipped and filtered on the way.

use std::path::Path;

use dbrover_core::{DumpEngine, RestoreOptions, TableName};

use crate::arguments::{ArgumentError, RunFiles};
use crate::connection::Target;
use crate::dump_file::DumpFileKind;
use crate::version::Version;

/// What a restore is, as far as the program's arguments go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreSpec<'a> {
    /// The engine of the server restored into.
    pub engine: DumpEngine,
    pub program: Version,
    pub kind: DumpFileKind,
    pub file: &'a Path,
    /// Some of an archive's tables only.
    pub tables: Option<&'a [TableName]>,
    pub options: RestoreOptions,
    /// Whether the database existed before: only then are objects dropped first.
    pub existing: bool,
}

/// How the dump reaches the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// The program reads the file it is given.
    Named,
    /// DbRover sends the file on the program's standard input, gunzipped when `gzip`, and
    /// without owners and privileges when `leave_out_owners`.
    Stdin { gzip: bool, leave_out_owners: bool },
}

/// A restore program's run, ready to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreCommand {
    pub arguments: Vec<String>,
    /// As **Copy command** shows them.
    pub shown: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub input: Input,
}

/// The arguments for restoring `spec` into `target`.
///
/// # Errors
///
/// [`ArgumentError`] for a file another engine wrote, or a restore the program can't do.
pub fn restore_command(
    spec: &RestoreSpec<'_>,
    target: &Target,
    files: &RunFiles,
) -> Result<RestoreCommand, ArgumentError> {
    if !spec.kind.restores_into(spec.engine) {
        return Err(ArgumentError::OtherEngine {
            file: spec.kind.engine().unwrap_or(spec.engine),
            server: spec.engine,
        });
    }
    match (spec.engine, spec.kind) {
        (DumpEngine::Postgres, DumpFileKind::Custom | DumpFileKind::Directory) => {
            pg_restore(spec, target, files)
        }
        (DumpEngine::Postgres, DumpFileKind::Sql { gzip, .. }) => psql(spec, target, files, gzip),
        (DumpEngine::Mysql | DumpEngine::MariaDb, _) => Err(ArgumentError::Format(spec.engine)),
    }
}

fn connection(spec: &RestoreSpec<'_>, target: &Target) -> Result<Vec<String>, ArgumentError> {
    Ok(vec![
        format!("--dbname={}", target.conninfo(spec.program)?),
        // Never wait for a password on a terminal a GUI app doesn't have.
        "--no-password".to_owned(),
    ])
}

fn pg_restore(
    spec: &RestoreSpec<'_>,
    target: &Target,
    files: &RunFiles,
) -> Result<RestoreCommand, ArgumentError> {
    let mut arguments = connection(spec, target)?;
    // pg_restore otherwise goes on past an error and reports them all at the end.
    arguments.extend(["--verbose", "--exit-on-error"].map(str::to_owned));
    let jobs = spec.options.jobs.get();
    if jobs > 1 {
        arguments.push(format!("--jobs={jobs}"));
    } else if spec.options.all_or_nothing {
        arguments.push("--single-transaction".to_owned());
    }
    if spec.options.leave_out_owners {
        arguments.extend(["--no-owner", "--no-privileges"].map(str::to_owned));
    }
    if spec.options.drop_first && spec.existing {
        arguments.extend(["--clean", "--if-exists"].map(str::to_owned));
    }
    if let Some(tables) = spec.tables {
        arguments.extend(table_options(tables));
    }
    arguments.push(spec.file.to_string_lossy().into_owned());
    Ok(RestoreCommand {
        shown: arguments.clone(),
        arguments,
        environment: password_file(files),
        input: Input::Named,
    })
}

/// `pg_restore --table` takes a table's name alone, not a pattern; `--schema` beside it
/// narrows every table to that schema, so it is given only when they share one.
fn table_options(tables: &[TableName]) -> Vec<String> {
    let mut options = Vec::new();
    if let Some(first) = tables.first()
        && tables.iter().all(|table| table.schema == first.schema)
    {
        options.push(format!("--schema={}", first.schema));
    }
    options.extend(tables.iter().map(|table| format!("--table={}", table.name)));
    options
}

fn psql(
    spec: &RestoreSpec<'_>,
    target: &Target,
    files: &RunFiles,
    gzip: bool,
) -> Result<RestoreCommand, ArgumentError> {
    let mut arguments = connection(spec, target)?;
    arguments.extend(
        [
            // The user's ~/.psqlrc could change what the dump's statements do.
            "--no-psqlrc",
            "--set=ON_ERROR_STOP=1",
            "--quiet",
        ]
        .map(str::to_owned),
    );
    if spec.options.all_or_nothing {
        arguments.push("--single-transaction".to_owned());
    }
    Ok(RestoreCommand {
        shown: arguments.clone(),
        arguments,
        environment: password_file(files),
        input: Input::Stdin {
            gzip,
            leave_out_owners: spec.options.leave_out_owners,
        },
    })
}

fn password_file(files: &RunFiles) -> Vec<(String, String)> {
    vec![(
        "PGPASSFILE".to_owned(),
        files.secret.to_string_lossy().into_owned(),
    )]
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
            database: "shop_restored".to_owned(),
            tls: TlsMode::Disable,
            tunnel_port: None,
        }
    }

    fn files() -> RunFiles {
        RunFiles {
            secret: "/run/secret".into(),
            filter: "/run/filter".into(),
            output: "/unused".into(),
        }
    }

    fn spec(kind: DumpFileKind) -> RestoreSpec<'static> {
        RestoreSpec {
            engine: DumpEngine::Postgres,
            program: Version::new(17, 2, 0),
            kind,
            file: Path::new("/dumps/shop.dump"),
            tables: None,
            options: RestoreOptions::default(),
            existing: false,
        }
    }

    const CONNINFO: &str = "--dbname=host=db port=5432 user=ada dbname=shop_restored \
                            sslmode=disable application_name='DbRover dump'";

    #[test]
    fn restores_an_archive_all_or_nothing_without_owners() {
        let command = restore_command(&spec(DumpFileKind::Custom), &target(), &files()).unwrap();
        assert_eq!(
            command.arguments,
            [
                CONNINFO,
                "--no-password",
                "--verbose",
                "--exit-on-error",
                "--single-transaction",
                "--no-owner",
                "--no-privileges",
                "/dumps/shop.dump"
            ]
        );
        assert_eq!(command.input, Input::Named);
        assert_eq!(command.environment[0].0, "PGPASSFILE");
    }

    #[test]
    fn jobs_replace_the_single_transaction_and_drops_need_an_existing_database() {
        let mut parallel = spec(DumpFileKind::Directory);
        parallel.options.jobs = NonZeroU8::new(4).unwrap();
        parallel.options.drop_first = true;
        let command = restore_command(&parallel, &target(), &files()).unwrap();
        assert!(command.arguments.contains(&"--jobs=4".to_owned()));
        assert!(
            !command
                .arguments
                .contains(&"--single-transaction".to_owned())
        );
        assert!(
            !command.arguments.contains(&"--clean".to_owned()),
            "a new database"
        );
        parallel.existing = true;
        let command = restore_command(&parallel, &target(), &files()).unwrap();
        assert!(command.arguments.contains(&"--clean".to_owned()));
        assert!(command.arguments.contains(&"--if-exists".to_owned()));
    }

    #[test]
    fn restores_chosen_tables() {
        let tables = [
            TableName {
                schema: "public".to_owned(),
                name: "Order Items".to_owned(),
            },
            TableName {
                schema: "public".to_owned(),
                name: "customers".to_owned(),
            },
        ];
        let mut chosen = spec(DumpFileKind::Custom);
        chosen.tables = Some(&tables);
        let command = restore_command(&chosen, &target(), &files()).unwrap();
        let end = &command.arguments[command.arguments.len() - 4..];
        assert_eq!(
            end,
            [
                "--schema=public",
                "--table=Order Items",
                "--table=customers",
                "/dumps/shop.dump"
            ]
        );
    }

    #[test]
    fn sends_plain_sql_to_psql() {
        let command = restore_command(
            &spec(DumpFileKind::Sql {
                engine: Some(DumpEngine::Postgres),
                gzip: true,
            }),
            &target(),
            &files(),
        )
        .unwrap();
        assert_eq!(
            command.arguments[2..],
            [
                "--no-psqlrc",
                "--set=ON_ERROR_STOP=1",
                "--quiet",
                "--single-transaction"
            ]
        );
        assert_eq!(
            command.input,
            Input::Stdin {
                gzip: true,
                leave_out_owners: true
            }
        );
    }

    #[test]
    fn refuses_another_engines_dump() {
        let mysql_dump = DumpFileKind::Sql {
            engine: Some(DumpEngine::Mysql),
            gzip: false,
        };
        assert_eq!(
            restore_command(&spec(mysql_dump), &target(), &files()),
            Err(ArgumentError::OtherEngine {
                file: DumpEngine::Mysql,
                server: DumpEngine::Postgres
            })
        );
    }
}
