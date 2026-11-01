//! Writing pictures, such as ER diagrams, on the blocking pool.

use std::path::{Path, PathBuf};
use std::{fs, io};

use dbrover_core::describe_error;

use crate::{Event, Picture};

/// Writes a picture to `path`: beside it first, renamed into place once whole, so
/// a failed export never leaves half a file. Answered with [`Event::PictureExported`].
pub(crate) async fn export(path: PathBuf, picture: Picture) -> Event {
    let task_path = path.clone();
    let result = tokio::task::spawn_blocking(move || write_picture(&task_path, picture))
        .await
        .map_err(|error| describe_error(&error))
        .and_then(|written| written);
    if let Err(error) = &result {
        tracing::warn!(
            "could not export the diagram to {}: {error}",
            path.display()
        );
    }
    Event::PictureExported { path, result }
}

fn write_picture(path: &Path, picture: Picture) -> Result<(), String> {
    let bytes = match picture {
        Picture::Svg(svg) => svg.into_bytes(),
        Picture::Png { svg, scale, fonts } => {
            dbrover_picture::png(&svg, scale, &fonts).map_err(|error| describe_error(&error))?
        }
    };
    write_beside_then_rename(path, &bytes).map_err(|error| describe_error(&error))
}

fn write_beside_then_rename(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    fs::write(&partial, bytes)?;
    fs::rename(&partial, path).inspect_err(|_| {
        let _ = fs::remove_file(&partial);
    })
}
