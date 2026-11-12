//! Work folders kept so their runs can resume (`docs/reports-at-scale.md`, RS5c): listed from
//! the folder runs' work folders go in, each by its manifest, and removed when they expire,
//! when together they hold more than Settings › Reports allows, when a later run of their
//! report finishes, or on **Discard**.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dbrover_report::Manifest;
use jiff::Timestamp;

use crate::report_disk::folder_size;

/// How long a kept work folder stays: a week, long enough to resume after a weekend, short
/// enough that source rows don't stay on disk for long unasked.
pub const KEPT_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What every kept work folder may hold together, when Settings › Reports doesn't say: room
/// for a couple of runs at their `max_disk`'s default of 20 GiB.
pub const DEFAULT_KEPT_RUNS_TOTAL: u64 = 50 << 30;

/// A work folder kept so its run can resume.
#[derive(Debug, Clone)]
pub struct KeptRun {
    pub folder: PathBuf,
    pub manifest: Manifest,
    /// When it was kept.
    pub kept_at: Timestamp,
}

impl KeptRun {
    /// The folder's name, which the history keeps (`RunProfile::kept`).
    pub fn name(&self) -> String {
        self.folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// The kept work folders in `root`, oldest first. A folder whose manifest doesn't say it was
/// kept belongs to a run going on, or one a crash left, and isn't listed.
pub fn kept_runs(root: &Path) -> Vec<KeptRun> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut kept: Vec<KeptRun> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|folder| {
            let manifest = Manifest::read(&folder)
                .inspect_err(|error| {
                    tracing::debug!("{} has no manifest to read: {error}", folder.display());
                })
                .ok()
                .flatten()?;
            let kept_at = manifest.kept_at()?;
            Some(KeptRun {
                folder,
                manifest,
                kept_at,
            })
        })
        .collect();
    kept.sort_by_key(|run| run.kept_at);
    kept
}

/// Removes a kept work folder.
///
/// # Errors
///
/// When it can't be removed.
pub fn discard(folder: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(folder) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => {
            tracing::info!("discarded {}", folder.display());
            Ok(())
        }
    }
}

/// Removes the kept work folders in `root` that expired by `now`, then the oldest ones while
/// all of them hold more than `total`. Returns the folders removed.
pub fn sweep(root: &Path, total: u64, now: Timestamp) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let mut left = Vec::new();
    for run in kept_runs(root) {
        let expired = now
            .duration_since(run.kept_at)
            .try_into()
            .is_ok_and(|age: Duration| age >= KEPT_FOR);
        if expired {
            removed.push(run.folder);
        } else {
            let size = folder_size(&run.folder);
            left.push((run.folder, size));
        }
    }
    let mut held: u64 = left.iter().map(|(_, size)| size).sum();
    for (folder, size) in left {
        if held <= total {
            break;
        }
        held -= size;
        removed.push(folder);
    }
    removed.retain(|folder| {
        discard(folder)
            .inspect_err(|error| {
                tracing::warn!("could not discard {}: {error}", folder.display());
            })
            .is_ok()
    });
    removed
}

/// Removes the kept work folders in `root` of runs of `report` but `keep`'s: a run of it that
/// finished supersedes them.
pub fn discard_superseded(root: &Path, report: &Path, keep: Option<&Path>) {
    for run in kept_runs(root) {
        let superseded = run.manifest.run.report == report && Some(run.folder.as_path()) != keep;
        if superseded && let Err(error) = discard(&run.folder) {
            tracing::warn!("could not discard {}: {error}", run.folder.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use dbrover_report::{Manifest, RangeKept, RunIdentity, TypedValues};

    use super::*;

    fn keep(root: &Path, name: &str, report: &str, kept_at: Option<&str>, bytes: usize) -> PathBuf {
        let folder = root.join(name);
        let mut manifest = Manifest::start(RunIdentity {
            report: PathBuf::from(report),
            version: "0123".to_owned(),
            values: TypedValues::new(),
            dbrover: "0.33.0".to_owned(),
            data_sources: Vec::new(),
        });
        manifest.ranges.push(RangeKept {
            number: 1,
            from: "0".to_owned(),
            to: "1".to_owned(),
            read: 1,
            rows: 1,
            bytes: 1,
            files: Vec::new(),
        });
        manifest.kept_at = kept_at.map(str::to_owned);
        manifest.write(&folder).unwrap();
        std::fs::write(folder.join("rows"), vec![b'x'; bytes]).unwrap();
        folder
    }

    fn names(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn lists_kept_folders_oldest_first_and_not_running_ones() {
        let root = tempfile::tempdir().unwrap();
        keep(root.path(), "b", "/r.toml", Some("2026-11-10T00:00:00Z"), 1);
        keep(root.path(), "a", "/r.toml", Some("2026-11-11T00:00:00Z"), 1);
        keep(root.path(), "running", "/r.toml", None, 1);
        std::fs::create_dir(root.path().join("no-manifest")).unwrap();
        let listed: Vec<String> = kept_runs(root.path()).iter().map(KeptRun::name).collect();
        assert_eq!(listed, ["b", "a"]);
        assert!(kept_runs(&root.path().join("missing")).is_empty());
    }

    #[test]
    fn sweeps_expired_folders_then_the_oldest_past_the_total() {
        let root = tempfile::tempdir().unwrap();
        keep(
            root.path(),
            "expired",
            "/r.toml",
            Some("2026-11-01T00:00:00Z"),
            10,
        );
        keep(
            root.path(),
            "old",
            "/r.toml",
            Some("2026-11-10T00:00:00Z"),
            3_000,
        );
        keep(
            root.path(),
            "new",
            "/r.toml",
            Some("2026-11-11T00:00:00Z"),
            3_000,
        );
        keep(root.path(), "running", "/r.toml", None, 50_000);
        let now: Timestamp = "2026-11-12T00:00:00Z".parse().unwrap();

        let removed = sweep(root.path(), 100_000, now);
        assert_eq!(removed, [root.path().join("expired")]);
        assert_eq!(names(root.path()), ["new", "old", "running"]);

        let removed = sweep(root.path(), 4_000, now);
        assert_eq!(removed, [root.path().join("old")], "the oldest goes first");
        assert_eq!(names(root.path()), ["new", "running"]);
    }

    #[test]
    fn discards_the_folders_a_finished_run_supersedes() {
        let root = tempfile::tempdir().unwrap();
        let mine = keep(
            root.path(),
            "mine",
            "/r.toml",
            Some("2026-11-10T00:00:00Z"),
            1,
        );
        keep(
            root.path(),
            "older",
            "/r.toml",
            Some("2026-11-09T00:00:00Z"),
            1,
        );
        keep(
            root.path(),
            "other",
            "/s.toml",
            Some("2026-11-09T00:00:00Z"),
            1,
        );
        discard_superseded(root.path(), Path::new("/r.toml"), Some(&mine));
        assert_eq!(names(root.path()), ["mine", "other"]);
        discard(&mine).unwrap();
        discard(&mine).unwrap();
        assert_eq!(names(root.path()), ["other"]);
    }
}
