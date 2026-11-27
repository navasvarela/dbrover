//! `dbrover dump run <saved dump>`: a saved dump run once without the app, for cron, launchd
//! or Task Scheduler on a machine that must dump with DbRover closed (W17h,
//! `docs/dump-restore.md`). The same run the app makes, recorded in the same history and
//! schedule state, the files past its `keep` removed.

use std::path::PathBuf;

use dbrover_core::ByteSize;
use dbrover_engine::{HeadlessDump, run_saved_dump};
use dbrover_store::{DataSourceFile, HistoryFile, SettingsFile, os_keychain};

use crate::{report_worker, version};

/// The exit codes: the dump was written, or it wasn't.
const DONE: i32 = 0;
const FAILED: i32 = 1;

pub const USAGE: &str = "\
Usage: dbrover dump run <saved dump> [--config-dir <folder>]

Runs a dump saved in DbRover, by its id or its name, once, as its schedule runs it while
DbRover is open: written to its folder under a new name, recorded in the history, the files
past its keep removed. Its password must be in the keychain, or come from its password
command or cloud sign-in: there is nobody to ask. Exits 0 once the dump is written, 1
otherwise, with the reason on standard error.";

/// What `dbrover dump` was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Run {
        dump: String,
        config_dir: Option<PathBuf>,
    },
    Help,
    Invalid(String),
}

impl Request {
    /// Reads the arguments after `dump`.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Self {
        let mut args = args.into_iter();
        match args.next().as_deref() {
            Some("run") => {}
            Some("--help" | "-h") | None => return Self::Help,
            Some(other) => return Self::Invalid(format!("dbrover dump has no {other:?}")),
        }
        let mut dump = None;
        let mut config_dir = None;
        while let Some(arg) = args.next() {
            match version::parse_config_dir(&arg, &mut args) {
                Some(Ok(folder)) => config_dir = Some(folder),
                Some(Err(problem)) => return Self::Invalid(problem),
                None if arg == "--help" || arg == "-h" => return Self::Help,
                None if arg.starts_with('-') => {
                    return Self::Invalid(format!("dbrover dump run has no option {arg}"));
                }
                None if dump.is_some() => {
                    return Self::Invalid("dbrover dump run takes one saved dump".to_owned());
                }
                None => dump = Some(arg),
            }
        }
        match dump {
            Some(dump) => Self::Run { dump, config_dir },
            None => Self::Invalid("dbrover dump run needs a saved dump's id or name".to_owned()),
        }
    }
}

/// Does what `request` asks, and returns the process's exit code.
pub fn main(request: Request) -> i32 {
    let (dump, config_dir) = match request {
        Request::Help => {
            println!("{}\n\n{USAGE}", version::describe());
            return DONE;
        }
        Request::Invalid(problem) => {
            eprintln!("{problem}\n\n{USAGE}");
            return FAILED;
        }
        Request::Run { dump, config_dir } => (dump, config_dir),
    };
    let paths = match report_worker::settings_paths(config_dir.as_deref()) {
        Ok(paths) => paths,
        Err(failure) => {
            eprintln!("The settings folder can't be used: {}", failure.message);
            return FAILED;
        }
    };
    let headless = HeadlessDump {
        data_sources: DataSourceFile::new(paths.data_sources_file()),
        settings: SettingsFile::new(paths.settings_file()),
        trusted_commands: paths.trusted_commands_file(),
        known_hosts: paths.ssh_known_hosts_file(),
        history: HistoryFile::new(paths.history_file()),
        secrets: os_keychain(),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(run_saved_dump(&dump, headless)),
        Err(error) => Err(format!("could not start the runtime: {error}")),
    };
    match result {
        Ok(done) => {
            println!(
                "Dumped to {} ({}) in {:.1} s.",
                done.file.display(),
                ByteSize(done.bytes),
                done.elapsed.as_secs_f64()
            );
            DONE
        }
        Err(problem) => {
            eprintln!("{problem}");
            FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Request {
        Request::parse(args.iter().map(|&arg| arg.to_owned()))
    }

    #[test]
    fn reads_a_saved_dump_and_a_settings_folder() {
        assert_eq!(
            parse(&["run", "nightly-shop", "--config-dir", "/srv/dbrover"]),
            Request::Run {
                dump: "nightly-shop".to_owned(),
                config_dir: Some("/srv/dbrover".into()),
            }
        );
        assert_eq!(parse(&[]), Request::Help);
        assert!(matches!(parse(&["run"]), Request::Invalid(_)));
        assert!(matches!(parse(&["run", "a", "b"]), Request::Invalid(_)));
        assert!(matches!(
            parse(&["run", "a", "--force"]),
            Request::Invalid(_)
        ));
        assert!(matches!(parse(&["start", "a"]), Request::Invalid(_)));
    }
}
