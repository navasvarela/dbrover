//! `dbrover report run --resume <run>` and `--from-scratch` (`docs/reports-at-scale.md`, RS5d):
//! the stopped run a run carries on from, found by its id in the history and its work folder's
//! manifest, and the report's stopped runs, which a run from scratch discards.

use std::path::{Path, PathBuf};

use dbrover_core::describe_error;
use dbrover_engine::{KeptRun, discard, kept_runs};
use dbrover_report::Manifest;
use dbrover_store::{AppPaths, HistoryFile, ReportName, ReportRun, RunId};

use super::history::listed_name;
use crate::reports::RunSettings;

/// How many of a report's newest runs are looked through for the one a run resumes: as many
/// as the history keeps by default and then some.
const RUNS_SEARCHED: usize = 200;

/// A stopped run to carry on from.
pub(super) struct Resuming {
    pub(super) id: RunId,
    /// Its kept work folder.
    pub(super) work: PathBuf,
    pub(super) manifest: Manifest,
}

/// The folder runs' work folders go in, as Settings › Reports has it.
pub(super) fn work_root(paths: &AppPaths, settings: &RunSettings) -> PathBuf {
    settings.work_root(&paths.report_runs_dir()).to_owned()
}

/// The run `id` of `report`, whose work folder in `work_root` was kept to resume.
///
/// # Errors
///
/// Why it can't be resumed, as a sentence: the report isn't in the reports folder, the
/// history keeps no such run, or the run kept no work folder, or no longer has it.
pub(super) fn find(
    paths: &AppPaths,
    report: &Path,
    id: RunId,
    work_root: &Path,
) -> Result<Resuming, String> {
    let name = listed_name(paths.report_files().dir(), report).ok_or_else(|| {
        format!(
            "{} isn't in the reports folder, so its runs aren't kept, and none can resume",
            report.display()
        )
    })?;
    let run = history_runs(paths, &name)?
        .into_iter()
        .find(|run| run.id == Some(id))
        .ok_or_else(|| {
            format!("{name} keeps no run {id}; dbrover report runs {name} lists the runs it keeps")
        })?;
    let folder = run
        .profile
        .kept
        .ok_or_else(|| format!("run {id} kept no work folder to resume from"))?;
    let work = work_root.join(&folder);
    let manifest = Manifest::read(&work)
        .map_err(|error| format!("run {id}'s work folder can't be read: {error}"))?
        .filter(|manifest| manifest.kept_at.is_some())
        .ok_or_else(|| {
            format!(
                "run {id}'s work folder is gone: a later run finished or resumed it, it expired, \
                 or it was discarded"
            )
        })?;
    Ok(Resuming { id, work, manifest })
}

/// The newest of `report`'s stopped runs that can resume, and the history's id for it.
pub(super) fn newest(
    paths: &AppPaths,
    report: &Path,
    work_root: &Path,
) -> Option<(RunId, KeptRun)> {
    let name = listed_name(paths.report_files().dir(), report)?;
    let kept = of_report(report, work_root);
    let runs = history_runs(paths, &name).ok()?;
    runs.iter().find_map(|run| {
        let folder = run.profile.kept.as_deref()?;
        let found = kept.iter().find(|kept| kept.name() == folder)?;
        Some((run.id?, found.clone()))
    })
}

/// Discards every stopped run of `report` kept in `work_root`.
pub(super) fn discard_all(report: &Path, work_root: &Path) {
    for run in of_report(report, work_root) {
        if let Err(error) = discard(&run.folder) {
            tracing::warn!("could not discard {}: {error}", run.folder.display());
        }
    }
}

/// The stopped runs of `report` kept in `work_root`.
fn of_report(report: &Path, work_root: &Path) -> Vec<KeptRun> {
    let report = report.canonicalize().unwrap_or_else(|_| report.to_owned());
    kept_runs(work_root)
        .into_iter()
        .filter(|run| run.manifest.run.report == report)
        .collect()
}

fn history_runs(paths: &AppPaths, name: &ReportName) -> Result<Vec<ReportRun>, String> {
    HistoryFile::new(paths.history_file())
        .open()
        .and_then(|history| history.runs(name, RUNS_SEARCHED))
        .map_err(|error| format!("the history can't be read: {}", describe_error(&error)))
}
