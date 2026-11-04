//! Reading and writing report files on the blocking pool, for the explorer's **Reports** and
//! **New report**. What a file says is read by `dbrover-report` in the app; this only finds
//! the files and their text.

use dbrover_store::{ReportFiles, ReportName};

use crate::Event;
use crate::config_files::blocking;
use crate::protocol::ReportText;

/// Every report in the folder, with its text or why it couldn't be read.
pub(crate) async fn list(files: ReportFiles) -> Event {
    let listed = blocking(move || {
        files.list().map(|names| {
            names
                .into_iter()
                .map(|name| {
                    let text = files.read(&name).map_err(|error| error.to_string());
                    ReportText {
                        path: files.path(&name),
                        name,
                        text,
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
