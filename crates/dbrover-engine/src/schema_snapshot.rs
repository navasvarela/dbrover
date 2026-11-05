//! Schemas outside a session: read headless for `dbrover schema snapshot` and `dbrover schema
//! compare`, and read from snapshot files (VC3).

use std::path::{Path, PathBuf};

use dbrover_core::{DataSource, describe_error};
use dbrover_language::QueryLanguage;
use dbrover_schema::{Schema, read_snapshot};

use crate::{Event, SourceAccess, open_data_source};

/// Reads the snapshot file at `path` on the blocking pool. Answered with
/// [`Event::SchemaSnapshotRead`].
pub(crate) async fn read_file(path: PathBuf) -> Event {
    let task_path = path.clone();
    let result = tokio::task::spawn_blocking(move || read_snapshot_file(&task_path))
        .await
        .map_err(|error| describe_error(&error))
        .and_then(|read| read);
    if let Err(error) = &result {
        tracing::warn!("could not read the snapshot {}: {error}", path.display());
    }
    Event::SchemaSnapshotRead { path, result }
}

/// The schema in the snapshot file at `path`.
///
/// # Errors
///
/// Why the file can't be read, or isn't a snapshot, as a sentence naming it.
pub fn read_snapshot_file(path: &Path) -> Result<Schema, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    read_snapshot(&text).map_err(|error| format!("{}: {error}", path.display()))
}

/// Connects to `data_source` as the app would, reads `schema`, and disconnects.
///
/// # Errors
///
/// Why it couldn't, as a sentence: a data source that doesn't speak SQL or can't compare its
/// schemas, a connection that fails, or a schema that isn't there.
pub async fn read_schema(
    data_source: &DataSource,
    schema: &str,
    access: &SourceAccess<'_>,
) -> Result<Schema, String> {
    let dialect = QueryLanguage::from(data_source.config.driver())
        .dialect()
        .ok_or_else(|| {
            format!(
                "{} doesn't speak SQL, so its schemas can't be compared.",
                data_source.name
            )
        })?;
    let mut connection =
        open_data_source(data_source, access.secrets, access.tunnels, access.commands)
            .await
            .map_err(|error| {
                format!(
                    "could not connect to {}: {}",
                    data_source.name,
                    describe_error(&error)
                )
            })?;
    let described = connection.describe_schema(schema).await;
    if let Err(error) = connection.close().await {
        tracing::warn!(
            "could not close the connection to {}: {}",
            data_source.name,
            describe_error(&error)
        );
    }
    let described = described
        .map_err(|error| describe_error(&error))?
        .ok_or_else(|| {
            format!(
                "There is no schema \u{201c}{schema}\u{201d} on {}.",
                data_source.name
            )
        })?;
    Ok(Schema::from_description(described, dialect))
}
