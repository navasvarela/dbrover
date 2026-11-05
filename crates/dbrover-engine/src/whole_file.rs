//! Writing a file the user picked so that a failure never leaves half of it.

use std::path::{Path, PathBuf};
use std::{fs, io};

/// Writes `bytes` beside `path` first, then renames them into place once whole, so a failed
/// write leaves any earlier file as it was.
///
/// # Errors
///
/// The first I/O error, after removing the partial file.
pub fn write_whole(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    fs::write(&partial, bytes)?;
    fs::rename(&partial, path).inspect_err(|_| {
        let _ = fs::remove_file(&partial);
    })
}
