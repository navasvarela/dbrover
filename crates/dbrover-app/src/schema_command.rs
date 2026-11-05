//! `dbrover schema snapshot`: a schema written as a snapshot file from the command line, for a
//! CI job that commits one after every migration (VC3, `docs/schema-compare.md`).

mod args;

use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use dbrover_core::{DataSource, describe_error};
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels, read_schema};
use dbrover_schema::write_snapshot;
use dbrover_store::{AppPaths, DataSourceFile, os_keychain};

pub use args::Request;

use self::args::{SnapshotArgs, USAGE};
use crate::{report_worker, version};

/// The exit code for anything that went wrong, the code `dbrover schema compare` uses for it
/// too, so a CI step reads both alike.
const FAILED: i32 = 2;

/// Does what `request` asks, and returns the process's exit code.
pub fn main(request: Request) -> i32 {
    match request {
        Request::Help => {
            println!("{}\n\n{USAGE}", version::describe());
            0
        }
        Request::Invalid(problem) => {
            eprintln!("{problem}\n\n{USAGE}");
            FAILED
        }
        Request::Snapshot(args) => match snapshot(&args) {
            Ok(()) => 0,
            Err(problem) => {
                eprintln!("{problem}");
                FAILED
            }
        },
    }
}

fn snapshot(args: &SnapshotArgs) -> Result<(), String> {
    let paths = report_worker::settings_paths(args.config_dir.as_deref())
        .map_err(|failure| format!("The settings folder can't be used: {}", failure.message))?;
    let data_source = find_data_source(&paths, &args.data_source)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start the runtime: {error}"))?;
    let schema = runtime.block_on(async {
        let secrets = os_keychain();
        let tunnels = Tunnels::new(paths.ssh_known_hosts_file());
        let commands =
            PasswordCommands::load(&paths.trusted_commands_file(), Arc::new(ProcessRunner)).await;
        let access = SourceAccess {
            secrets: &secrets,
            tunnels: &tunnels,
            commands: &commands,
        };
        read_schema(&data_source, &args.schema, &access).await
    })?;
    let text = write_snapshot(&schema);
    match &args.output {
        Some(path) => write_file(path, &text),
        None => io::stdout()
            .lock()
            .write_all(text.as_bytes())
            .map_err(|error| format!("could not write the snapshot: {error}")),
    }
}

/// The saved data source called `name`.
fn find_data_source(paths: &AppPaths, name: &str) -> Result<DataSource, String> {
    let data_sources = DataSourceFile::new(paths.data_sources_file())
        .load()
        .map_err(|error| describe_error(&error))?;
    data_sources
        .into_iter()
        .find(|data_source| data_source.name == name)
        .ok_or_else(|| format!("There is no data source named \u{201c}{name}\u{201d}."))
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    dbrover_engine::write_whole(path, text.as_bytes())
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}
