//! Reading and writing ER diagram files on the blocking pool.

use dbrover_core::describe_error;
use dbrover_store::{DiagramFile, DiagramFiles, DiagramKey, DiagramStoreError, SavedDiagram};
use tokio::task::JoinError;

use crate::Event;

/// Reads every diagram file. Files that can't be read are logged and left out, so the UI
/// drops their tabs and the files stay untouched on disk.
pub(crate) async fn load(files: &DiagramFiles) -> Event {
    let files = files.clone();
    let loaded = match tokio::task::spawn_blocking(move || files.load_all()).await {
        Ok(Ok(results)) => results,
        Ok(Err(error)) => {
            tracing::error!("could not load diagrams: {}", describe_error(&error));
            Vec::new()
        }
        Err(error) => {
            tracing::error!("could not load diagrams: {}", describe_error(&error));
            Vec::new()
        }
    };
    let diagrams: Vec<SavedDiagram> = loaded
        .into_iter()
        .filter_map(|result| {
            result
                .inspect_err(|error| {
                    tracing::error!("could not load a diagram: {}", describe_error(error));
                })
                .ok()
        })
        .collect();
    Event::DiagramsLoaded(diagrams)
}

/// Writes the diagram's file, answering with [`Event::DiagramSaveFailed`] if that fails.
pub(crate) async fn save(
    files: &DiagramFiles,
    key: DiagramKey,
    file: DiagramFile,
) -> Option<Event> {
    let files = files.clone();
    let task_key = key.clone();
    let result = tokio::task::spawn_blocking(move || files.save(&task_key, &file)).await;
    let error = flatten(result)?;
    tracing::error!(diagram = %key.name, "could not save the diagram: {error}");
    Some(Event::DiagramSaveFailed { key, error })
}

/// Removes the diagram's file. A leftover file only lists the diagram again, so a failure is
/// logged.
pub(crate) async fn delete(files: &DiagramFiles, key: DiagramKey) {
    let files = files.clone();
    let task_key = key.clone();
    let result = tokio::task::spawn_blocking(move || files.delete(&task_key)).await;
    if let Some(error) = flatten(result) {
        tracing::warn!(diagram = %key.name, "could not remove the diagram file: {error}");
    }
}

/// The description of whatever went wrong, if anything did.
fn flatten(result: Result<Result<(), DiagramStoreError>, JoinError>) -> Option<String> {
    match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(describe_error(&error)),
        Err(error) => Some(describe_error(&error)),
    }
}
