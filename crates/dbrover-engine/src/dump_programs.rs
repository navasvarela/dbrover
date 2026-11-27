//! Finding the engines' dump programs on this machine and reading their versions (W17,
//! `docs/dump-restore.md`). Where to look is `dbrover-dump`'s; running each copy's `--version`
//! is here.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use dbrover_dump::{FoundProgram, Program, ProgramVersion, RealFolders, SearchContext};

/// How long a program has to print its version. Chosen, not measured: `--version` answers at
/// once, and a copy that hangs (a wrapper waiting on something) is left out rather than
/// holding a dump up.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// The paths set by hand in **Settings › Dumps**, which win over every copy found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramPaths(pub BTreeMap<Program, PathBuf>);

/// What each copy's `--version` said, kept until its file changes, so a dump doesn't start
/// every copy on the machine again. Cloning shares it.
#[derive(Clone, Default)]
pub(crate) struct ProgramVersions(Arc<Mutex<HashMap<PathBuf, Known>>>);

#[derive(Clone, Copy)]
struct Known {
    modified: Option<SystemTime>,
    version: Option<ProgramVersion>,
}

impl ProgramVersions {
    /// Every copy of `programs` that runs and says its version: the path set in Settings for a
    /// program alone when there is one, otherwise every copy `PATH` and the installers'
    /// folders hold.
    pub(crate) async fn find(
        &self,
        programs: &[Program],
        paths: &ProgramPaths,
    ) -> Vec<FoundProgram> {
        let mut found = Vec::new();
        for &program in programs {
            let candidates = match paths.0.get(&program) {
                Some(path) => vec![path.clone()],
                None => search(program).await,
            };
            for path in candidates {
                if let Some(version) = self.version(&path).await {
                    found.push(FoundProgram {
                        program,
                        path,
                        version,
                    });
                }
            }
        }
        found
    }

    async fn version(&self, path: &Path) -> Option<ProgramVersion> {
        let modified = std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok();
        let cached = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            .copied();
        if let Some(known) = cached.filter(|known| known.modified == modified) {
            return known.version;
        }
        let version = read_version(path).await;
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_owned(), Known { modified, version });
        version
    }
}

async fn search(program: Program) -> Vec<PathBuf> {
    let context = SearchContext::current();
    tokio::task::spawn_blocking(move || dbrover_dump::find(program, &context, &RealFolders))
        .await
        .unwrap_or_default()
}

/// What `path --version` says, or `None` for a copy that doesn't run, hangs or prints
/// something else.
async fn read_version(path: &Path) -> Option<ProgramVersion> {
    let mut command = tokio::process::Command::new(path);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(crate::programs::CREATE_NO_WINDOW);
    let output = tokio::time::timeout(VERSION_TIMEOUT, command.output()).await;
    match output {
        Ok(Ok(output)) if output.status.success() => {
            let version = ProgramVersion::parse(&String::from_utf8_lossy(&output.stdout));
            if version.is_none() {
                tracing::info!("{} printed no version DbRover reads", path.display());
            }
            version
        }
        Ok(Ok(output)) => {
            tracing::info!("{} --version failed: {}", path.display(), output.status);
            None
        }
        Ok(Err(error)) => {
            tracing::info!("could not run {}: {error}", path.display());
            None
        }
        Err(_) => {
            tracing::info!(
                "{} --version took longer than {VERSION_TIMEOUT:?}",
                path.display()
            );
            None
        }
    }
}
