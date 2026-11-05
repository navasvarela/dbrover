//! `dbrover schema snapshot` and `dbrover schema compare`: a schema written as a snapshot file,
//! and two schemas compared, from the command line, for a CI job that commits a snapshot after
//! every migration and fails when a server has drifted from it (VC3, `docs/schema-compare.md`).

mod args;

use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use dbrover_core::{DataSource, describe_error};
use dbrover_engine::{
    PasswordCommands, ProcessRunner, SourceAccess, Tunnels, read_schema, read_snapshot_file,
};
use dbrover_schema::{Schema, Sides, compare, script, write_snapshot};
use dbrover_store::{AppPaths, DataSourceFile, os_keychain};

pub use args::Request;

use self::args::{CompareArgs, SideArg, SnapshotArgs, USAGE};
use crate::{report_worker, version};

/// The exit codes, as `diff`'s: the same, different, or trouble. A snapshot written is "the
/// same", and one that isn't is trouble, so a CI step reads both commands alike.
const SAME: i32 = 0;
const DIFFERENT: i32 = 1;
const FAILED: i32 = 2;

/// Does what `request` asks, and returns the process's exit code.
pub fn main(request: Request) -> i32 {
    let outcome = match request {
        Request::Help => {
            println!("{}\n\n{USAGE}", version::describe());
            return SAME;
        }
        Request::Invalid(problem) => {
            eprintln!("{problem}\n\n{USAGE}");
            return FAILED;
        }
        Request::Snapshot(args) => snapshot(&args).map(|()| SAME),
        Request::Compare(args) => compare_sides(&args),
    };
    outcome.unwrap_or_else(|problem| {
        eprintln!("{problem}");
        FAILED
    })
}

fn snapshot(args: &SnapshotArgs) -> Result<(), String> {
    let settings = Settings::load(args.config_dir.as_deref())?;
    let schema = block_on(settings.read(&args.data_source, &args.schema))?;
    let text = write_snapshot(&schema);
    match &args.output {
        Some(path) => dbrover_engine::write_whole(path, text.as_bytes())
            .map_err(|error| format!("could not write {}: {error}", path.display())),
        None => print(&text),
    }
}

/// Compares the two sides and prints the differences or the script; the exit code says
/// whether they differ.
fn compare_sides(args: &CompareArgs) -> Result<i32, String> {
    let is_live = |side: &SideArg| matches!(side, SideArg::Live { .. });
    let settings = if is_live(&args.left) || is_live(&args.right) {
        Some(Settings::load(args.config_dir.as_deref())?)
    } else {
        None
    };
    let (left, right) = block_on(async {
        let left = read_side(&args.left, settings.as_ref()).await?;
        let right = read_side(&args.right, settings.as_ref()).await?;
        Ok::<_, String>((left, right))
    })?;
    let comparison = compare(&left, &right).map_err(|error| error.to_string())?;
    if args.script {
        let sides = Sides {
            left: describe(&args.left),
            right: describe(&args.right),
        };
        let written = script(&left, &right, &sides).map_err(|error| error.to_string())?;
        if let Some(written) = written {
            print(&written.sql)?;
        }
    } else if comparison.is_empty() {
        print("The schemas are the same.\n")?;
    } else {
        print(&comparison.report())?;
    }
    Ok(if comparison.is_empty() {
        SAME
    } else {
        DIFFERENT
    })
}

async fn read_side(side: &SideArg, settings: Option<&Settings>) -> Result<Schema, String> {
    match (side, settings) {
        (SideArg::Snapshot(path), _) => read_snapshot_file(path),
        (
            SideArg::Live {
                data_source,
                schema,
            },
            Some(settings),
        ) => settings.read(data_source, schema).await,
        (SideArg::Live { .. }, None) => Err("the settings folder wasn't read".to_owned()),
    }
}

/// How the script's header names a side.
fn describe(side: &SideArg) -> String {
    match side {
        SideArg::Live {
            data_source,
            schema,
        } => format!("schema {schema} on {data_source}"),
        SideArg::Snapshot(path) => format!("snapshot {}", path.display()),
    }
}

/// The settings folder's paths and data sources, which a live side is read through.
struct Settings {
    paths: AppPaths,
    data_sources: Vec<DataSource>,
}

impl Settings {
    fn load(config_dir: Option<&Path>) -> Result<Self, String> {
        let paths = report_worker::settings_paths(config_dir)
            .map_err(|failure| format!("The settings folder can't be used: {}", failure.message))?;
        let data_sources = DataSourceFile::new(paths.data_sources_file())
            .load()
            .map_err(|error| describe_error(&error))?;
        Ok(Self {
            paths,
            data_sources,
        })
    }

    /// Connects to the data source called `name` as the app would, and reads `schema`.
    async fn read(&self, name: &str, schema: &str) -> Result<Schema, String> {
        let data_source = self
            .data_sources
            .iter()
            .find(|data_source| data_source.name == name)
            .ok_or_else(|| format!("There is no data source named \u{201c}{name}\u{201d}."))?;
        let secrets = os_keychain();
        let tunnels = Tunnels::new(self.paths.ssh_known_hosts_file());
        let commands =
            PasswordCommands::load(&self.paths.trusted_commands_file(), Arc::new(ProcessRunner))
                .await;
        let access = SourceAccess {
            secrets: &secrets,
            tunnels: &tunnels,
            commands: &commands,
        };
        read_schema(data_source, schema, &access).await
    }
}

fn block_on<T>(work: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start the runtime: {error}"))?
        .block_on(work)
}

fn print(text: &str) -> Result<(), String> {
    io::stdout()
        .lock()
        .write_all(text.as_bytes())
        .map_err(|error| format!("could not write to standard output: {error}"))
}
