//! Reading DbRover's hand-editable config files on the blocking pool: `keymap.toml`,
//! `settings.toml`, and the customisation files beside them.

use std::error::Error;

use dbrover_core::describe_error;
use dbrover_store::{CustomFile, CustomFileKind, CustomFiles};

use crate::Event;

/// Runs file work on tokio's blocking pool, with any error described as the UI shows it.
pub(crate) async fn blocking<T, E>(
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, String>
where
    T: Send + 'static,
    E: Error + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(describe_error(&error)),
        Err(error) => Err(describe_error(&error)),
    }
}

/// Writes a new customisation file, then reads it back. A file that couldn't be written,
/// because one exists, is reported as it is, with why.
pub(crate) async fn create_custom(
    files: &CustomFiles,
    kind: CustomFileKind,
    text: String,
) -> Event {
    let task_files = files.clone();
    let created = blocking(move || task_files.create(kind, &text)).await;
    let task_files = files.clone();
    let read = tokio::task::spawn_blocking(move || task_files.read(kind)).await;
    let file = match read {
        Ok(file) => file,
        Err(error) => CustomFile::parse(kind, "").with_problem(describe_error(&error)),
    };
    let file = match created {
        Ok(()) => {
            tracing::info!("created {}", files.path(kind).display());
            file
        }
        Err(error) => {
            tracing::error!("could not create {}: {error}", kind.file_name());
            file.with_problem(format!("could not create {}: {error}", kind.file_name()))
        }
    };
    Event::CustomFileLoaded {
        path: files.path(kind),
        file,
    }
}

/// Reads `actions.toml`, `snippets.toml`, `theme.toml` and `exports.toml`, as one
/// [`Event::CustomFileLoaded`] each. What a file gets wrong is in its event, never fatal.
pub(crate) async fn load_custom(files: &CustomFiles) -> Vec<Event> {
    let mut events = Vec::new();
    for kind in CustomFileKind::ALL {
        let task_files = files.clone();
        match tokio::task::spawn_blocking(move || task_files.read(kind)).await {
            Ok(file) => {
                for problem in file.problems() {
                    tracing::warn!("{}: {problem}", kind.file_name());
                }
                events.push(Event::CustomFileLoaded {
                    path: files.path(kind),
                    file,
                });
            }
            Err(error) => tracing::error!(
                "could not read {}: {}",
                kind.file_name(),
                describe_error(&error)
            ),
        }
    }
    events
}
