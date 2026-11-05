//! Reading and writing report files on the blocking pool, for the explorer's **Reports** and
//! **New report**. What a file says is read by `dbrover-report` in the app; this only finds
//! the files and their text.

use std::time::SystemTime;

use dbrover_core::describe_error;
use dbrover_store::{ReportFiles, ReportName, ReportSaveError};

use crate::Event;
use crate::config_files::blocking;
use crate::protocol::{ReportSaveFailure, ReportText};

/// Every report in the folder, with its text or why it couldn't be read.
pub(crate) async fn list(files: ReportFiles) -> Event {
    let listed = blocking(move || {
        files.list().map(|names| {
            names
                .into_iter()
                .map(|name| {
                    let read = files.read_with_time(&name);
                    let modified = read.as_ref().ok().and_then(|contents| contents.modified);
                    ReportText {
                        path: files.path(&name),
                        text: read
                            .map(|contents| contents.text)
                            .map_err(|error| error.to_string()),
                        name,
                        modified,
                    }
                })
                .collect()
        })
    })
    .await;
    if let Err(error) = &listed {
        tracing::error!("could not list the reports: {error}");
    }
    Event::ReportsListed(listed)
}

/// Writes `text` over the report `name`, unless it changed on disk since `read_at`.
pub(crate) async fn save(
    files: ReportFiles,
    name: ReportName,
    text: String,
    read_at: Option<SystemTime>,
) -> Event {
    let saving = name.clone();
    let saved = tokio::task::spawn_blocking(move || files.save(&saving, &text, read_at)).await;
    let result = match saved {
        Ok(Ok(modified)) => {
            tracing::info!("saved the report {name}");
            Ok(modified)
        }
        Ok(Err(ReportSaveError::ChangedOnDisk)) => {
            tracing::info!("did not save the report {name}: it changed on disk");
            Err(ReportSaveFailure::ChangedOnDisk)
        }
        Ok(Err(error)) => Err(failed(&name, &error)),
        Err(error) => Err(failed(&name, &error)),
    };
    Event::ReportSaved { name, result }
}

fn failed(name: &ReportName, error: &(dyn std::error::Error + 'static)) -> ReportSaveFailure {
    let described = describe_error(error);
    tracing::error!("could not save the report {name}: {described}");
    ReportSaveFailure::Failed(described)
}

/// Writes `text` as a new report named after `name`, under a name no report has.
pub(crate) async fn create(files: ReportFiles, name: ReportName, text: String) -> Event {
    let created = blocking(move || files.create(&name, &text)).await;
    match &created {
        Ok(name) => tracing::info!("created the report {name}"),
        Err(error) => tracing::error!("could not create a report: {error}"),
    }
    Event::ReportCreated(created)
}

/// Renames the report `from` to `to`. Its runs are moved by the history, once this succeeds.
pub(crate) async fn rename(files: ReportFiles, from: &ReportName, to: &ReportName) -> Event {
    let (old, new) = (from.clone(), to.clone());
    let renamed = blocking(move || files.rename(&old, &new).map(|()| new)).await;
    match &renamed {
        Ok(to) => tracing::info!("renamed the report {from} to {to}"),
        Err(error) => tracing::error!("could not rename the report {from}: {error}"),
    }
    Event::ReportRenamed {
        from: from.clone(),
        result: renamed,
    }
}
