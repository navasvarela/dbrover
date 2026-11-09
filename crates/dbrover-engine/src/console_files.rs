//! Reading and writing query console files on the blocking pool.

use dbrover_core::describe_error;
use dbrover_store::{ConsoleFiles, ConsoleKey, ConsoleStoreError, SavedConsole};
use tokio::task::JoinError;

use crate::outside_changes::{DocumentVersions, Noticed, Saved, look_at, save_document};
use crate::{Document, Event};

#[derive(Debug, thiserror::Error)]
enum ConsoleFileError {
    #[error(transparent)]
    Store(#[from] ConsoleStoreError),
    #[error("a background task failed")]
    Task(#[source] JoinError),
}

/// Reads every console file, noting each one's version. Files that can't be read are logged
/// and left out, so the UI drops their tabs and the files stay untouched on disk.
pub(crate) async fn load(files: &ConsoleFiles, documents: &mut DocumentVersions) -> Event {
    let files = files.clone();
    let loaded = tokio::task::spawn_blocking(move || files.load_all())
        .await
        .map_err(ConsoleFileError::Task)
        .and_then(|result| result.map_err(ConsoleFileError::from));
    let consoles: Vec<SavedConsole> = match loaded {
        Ok(results) => results
            .into_iter()
            .filter_map(|result| {
                result
                    .inspect_err(|error| {
                        tracing::error!("could not load a console: {}", describe_error(error));
                    })
                    .ok()
            })
            .map(|read| {
                documents.read(Document::Console(read.console.key.clone()), read.version);
                read.console
            })
            .collect(),
        Err(error) => {
            tracing::error!("could not load consoles: {}", describe_error(&error));
            Vec::new()
        }
    };
    Event::ConsolesLoaded(consoles)
}

/// Writes the console's file, unless another program changed it since: answered with
/// [`Event::ConsoleChangedOnDisk`] then, and with [`Event::ConsoleSaveFailed`] if the write
/// fails.
pub(crate) async fn save(
    files: &ConsoleFiles,
    documents: &mut DocumentVersions,
    key: ConsoleKey,
    text: String,
) -> Option<Event> {
    let (read_files, read_key) = (files.clone(), key.clone());
    let (write_files, write_key) = (files.clone(), key.clone());
    let saved = save_document(
        documents,
        Document::Console(key.clone()),
        move || read_files.read(&read_key),
        move |read| write_files.write_over(&write_key, read, &text),
    )
    .await;
    match saved {
        Ok(Saved::Written | Saved::Waiting) => None,
        Ok(Saved::ChangedOnDisk(text)) => Some(Event::ConsoleChangedOnDisk {
            key,
            text: text.unwrap_or_default(),
            noticed: Noticed::Saving,
        }),
        Err(error) => {
            tracing::error!(console = %key.name, "could not save the console: {error}");
            Some(Event::ConsoleSaveFailed { key, error })
        }
    }
}

/// [`Event::ConsoleChangedOnDisk`] if another program changed the console's file since the
/// app last saw it.
pub(crate) async fn look_again(
    files: &ConsoleFiles,
    documents: &mut DocumentVersions,
    key: ConsoleKey,
) -> Option<Event> {
    let (files, read_key) = (files.clone(), key.clone());
    let text = look_at(documents, Document::Console(key.clone()), move || {
        files.read(&read_key)
    })
    .await
    .inspect_err(|error| tracing::error!(console = %key.name, "{error}"))
    .ok()??;
    Some(Event::ConsoleChangedOnDisk {
        key,
        text: text?,
        noticed: Noticed::Looking,
    })
}

/// Removes the console's file. A leftover file is harmless, so a failure is only logged.
pub(crate) async fn delete(
    files: &ConsoleFiles,
    documents: &mut DocumentVersions,
    key: ConsoleKey,
) {
    documents.forget(&Document::Console(key.clone()));
    let files = files.clone();
    let task_key = key.clone();
    let result = tokio::task::spawn_blocking(move || files.delete(&task_key)).await;
    if let Some(error) = flatten(result) {
        tracing::warn!(console = %key.name, "could not remove the console file: {error}");
    }
}

/// The description of whatever went wrong, if anything did.
fn flatten(result: Result<Result<(), ConsoleStoreError>, JoinError>) -> Option<String> {
    match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(describe_error(&error)),
        Err(error) => Some(describe_error(&error)),
    }
}
