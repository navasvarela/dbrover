//! Reading, writing and watching the `.sql` files consoles are bound to, on the blocking pool.

use std::io;
use std::path::PathBuf;

use dbrover_core::describe_error;
use dbrover_store::{ConsoleKey, SqlFileContents, SqlFileError};
use tokio::task::JoinError;

use crate::Event;
use crate::events::EventSink;

/// A file a console shows, and when it was last modified as far as the console knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedFile {
    pub path: PathBuf,
    pub modified: Option<std::time::SystemTime>,
}

#[derive(Debug, thiserror::Error)]
enum TaskError {
    #[error(transparent)]
    File(#[from] SqlFileError),
    #[error("a background task failed")]
    Task(#[source] JoinError),
}

fn flatten<T>(result: Result<Result<T, SqlFileError>, JoinError>) -> Result<T, String> {
    result
        .map_err(TaskError::Task)
        .and_then(|result| result.map_err(TaskError::from))
        .map_err(|error| describe_error(&error))
}

/// Reads the file, answering with [`Event::SqlFileRead`].
pub(crate) async fn read(path: PathBuf, events: &EventSink) {
    let task_path = path.clone();
    let result = flatten(
        tokio::task::spawn_blocking(move || dbrover_store::read_sql_file(&task_path)).await,
    );
    if let Err(error) = &result {
        tracing::warn!("could not open {}: {error}", path.display());
    }
    events.send(Event::SqlFileRead { path, result });
}

/// Writes the console's text to the file, answering with [`Event::SqlFileSaved`].
pub(crate) async fn save(console: ConsoleKey, path: PathBuf, text: String) -> Event {
    let task_path = path.clone();
    let result = flatten(
        tokio::task::spawn_blocking(move || dbrover_store::write_sql_file(&task_path, &text)).await,
    );
    match &result {
        Ok(_) => tracing::info!("saved {}", path.display()),
        Err(error) => tracing::error!("could not save {}: {error}", path.display()),
    }
    Event::SqlFileSaved {
        console,
        path,
        result,
    }
}

/// Creates an empty file, answering with [`Event::SqlFileCreated`].
pub(crate) async fn create(path: PathBuf) -> Event {
    let task_path = path.clone();
    let result = flatten(
        tokio::task::spawn_blocking(move || dbrover_store::create_sql_file(&task_path)).await,
    );
    match &result {
        Ok(()) => tracing::info!("created {}", path.display()),
        Err(error) => tracing::warn!("could not create {}: {error}", path.display()),
    }
    Event::SqlFileCreated { path, result }
}

/// Answers with an [`Event::SqlFileChanged`] for each file whose modification time isn't the
/// one given, with what it holds now, or `None` once it is gone. A file that can't be read is
/// only logged: it is looked at again at the next check.
pub(crate) async fn check(files: Vec<WatchedFile>, events: &EventSink) {
    let changed = tokio::task::spawn_blocking(move || {
        files
            .into_iter()
            .filter_map(|file| changed(file).transpose())
            .collect::<Vec<_>>()
    })
    .await;
    let Ok(changed) = changed else {
        tracing::warn!("could not look at the open .sql files");
        return;
    };
    for change in changed {
        match change {
            Ok((path, contents)) => events.send(Event::SqlFileChanged { path, contents }),
            Err(error) => tracing::warn!("{}", describe_error(&error)),
        }
    }
}

/// The file's contents if it changed since `file.modified`, `Some((path, None))` if it is
/// gone, and `None` if it is as the console knows it.
fn changed(file: WatchedFile) -> Result<Option<(PathBuf, Option<SqlFileContents>)>, SqlFileError> {
    match dbrover_store::modified(&file.path) {
        Ok(modified) if modified == file.modified => Ok(None),
        Ok(_) => {
            let contents = dbrover_store::read_sql_file(&file.path)?;
            Ok(Some((file.path, Some(contents))))
        }
        // Already reported as gone: the console's last known time is `None` from then on.
        Err(error) if error.kind() == io::ErrorKind::NotFound && file.modified.is_none() => {
            Ok(None)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Some((file.path, None))),
        Err(source) => Err(SqlFileError::Read {
            path: file.path,
            source,
        }),
    }
}
