//! A report run's disk (`docs/reports-at-scale.md`, RS3b): its budget, from `max_disk` and the
//! free space of its work folder's disk, and the bytes its folders hold.

use std::io;
use std::path::Path;

use dbrover_report::{ByteSize, DiskBudget};

/// What a run whose work folder is in `work_root` may hold on disk: `max_disk`, or less when
/// that disk has less free.
pub fn disk_budget(work_root: &Path, max_disk: ByteSize) -> DiskBudget {
    DiskBudget {
        max_disk,
        free: free_space(work_root).map(ByteSize),
    }
}

/// The bytes free to an unprivileged process on `path`'s disk; `None` where the system doesn't
/// say (Windows) or the call fails.
fn free_space(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let stats = rustix::fs::statvfs(path)
            .inspect_err(|error| {
                tracing::warn!("could not read {}'s free space: {error}", path.display());
            })
            .ok()?;
        Some(stats.f_bavail.saturating_mul(stats.f_frsize))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// The bytes of every file under `folder`, which may not exist: a folder that a run hasn't
/// made yet, or has removed, holds nothing.
pub(crate) fn folder_size(folder: &Path) -> u64 {
    match size_of(folder) {
        Ok(size) => size,
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => {
            tracing::debug!("could not measure {}: {error}", folder.display());
            0
        }
    }
}

fn size_of(path: &Path) -> io::Result<u64> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total = 0;
    for entry in std::fs::read_dir(path)? {
        // A file DuckDB removes between the listing and its size held nothing by then.
        total += match size_of(&entry?.path()) {
            Ok(size) => size,
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_up_every_file_in_a_folder_and_its_folders() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("a"), [0; 100]).unwrap();
        std::fs::create_dir(folder.path().join("spill")).unwrap();
        std::fs::write(folder.path().join("spill").join("b"), [0; 23]).unwrap();
        assert_eq!(folder_size(folder.path()), 123);
        assert_eq!(folder_size(&folder.path().join("gone")), 0);
    }

    #[test]
    fn reads_the_free_space_where_the_system_says() {
        let folder = tempfile::tempdir().unwrap();
        let budget = disk_budget(folder.path(), ByteSize(1));
        assert_eq!(budget.free.is_some(), cfg!(unix), "{budget:?}");
        assert_eq!(budget.bytes(), 1);
    }
}
