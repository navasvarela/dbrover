//! Reading and writing ER diagram files on the blocking pool.

use dbrover_core::describe_error;
use dbrover_store::{DiagramFile, DiagramFiles, DiagramKey, DiagramStoreError, SavedDiagram};
use tokio::task::JoinError;

use crate::outside_changes::{DocumentVersions, Noticed, Saved, look_at, save_document};
use crate::{Document, Event};

/// Reads every diagram file, noting each one's version. Files that can't be read are logged
/// and left out, so the UI drops their tabs and the files stay untouched on disk.
pub(crate) async fn load(files: &DiagramFiles, documents: &mut DocumentVersions) -> Event {
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
        .map(|read| {
            documents.read(Document::Diagram(read.diagram.key.clone()), read.version);
            read.diagram
        })
        .collect();
    Event::DiagramsLoaded(diagrams)
}

/// Writes the diagram's file, unless another program changed it since: answered with
/// [`Event::DiagramChangedOnDisk`] then, and with [`Event::DiagramSaveFailed`] if the write
/// fails or the file can't be read.
pub(crate) async fn save(
    files: &DiagramFiles,
    documents: &mut DocumentVersions,
    key: DiagramKey,
    file: DiagramFile,
) -> Option<Event> {
    let (read_files, read_key) = (files.clone(), key.clone());
    let (write_files, write_key) = (files.clone(), key.clone());
    let saved = save_document(
        documents,
        Document::Diagram(key.clone()),
        move || read_files.read(&read_key),
        move |read| write_files.write_over(&write_key, read, &file),
    )
    .await;
    match saved {
        Ok(Saved::Written | Saved::Waiting | Saved::ChangedOnDisk(None)) => None,
        Ok(Saved::ChangedOnDisk(Some(file))) => Some(Event::DiagramChangedOnDisk {
            key,
            file,
            noticed: Noticed::Saving,
        }),
        Err(error) => {
            tracing::error!(diagram = %key.name, "could not save the diagram: {error}");
            Some(Event::DiagramSaveFailed { key, error })
        }
    }
}

/// [`Event::DiagramChangedOnDisk`] if another program changed the diagram's file since the
/// app last saw it.
pub(crate) async fn look_again(
    files: &DiagramFiles,
    documents: &mut DocumentVersions,
    key: DiagramKey,
) -> Option<Event> {
    let (files, read_key) = (files.clone(), key.clone());
    let file = look_at(documents, Document::Diagram(key.clone()), move || {
        files.read(&read_key)
    })
    .await
    .inspect_err(|error| tracing::error!(diagram = %key.name, "{error}"))
    .ok()??;
    Some(Event::DiagramChangedOnDisk {
        key,
        file: file?,
        noticed: Noticed::Looking,
    })
}

/// Removes the diagram's file. A leftover file only lists the diagram again, so a failure is
/// logged.
pub(crate) async fn delete(
    files: &DiagramFiles,
    documents: &mut DocumentVersions,
    key: DiagramKey,
) {
    documents.forget(&Document::Diagram(key.clone()));
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
