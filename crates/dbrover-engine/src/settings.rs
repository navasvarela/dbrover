//! Reading and writing `settings.toml` on the blocking pool.

use dbrover_store::{HistorySettings, Settings, SettingsFile};

use crate::Document;
use crate::Event;
use crate::config_files::blocking;
use crate::outside_changes::{DocumentVersions, Noticed, Saved, look_at, save_document};

/// Reads the settings and reports them as [`Event::SettingsLoaded`], noting the file's
/// version. A broken file is reported, not replaced: the UI keeps the defaults and shows why.
pub(crate) async fn load(file: &SettingsFile, documents: &mut DocumentVersions) -> Event {
    let task_file = file.clone();
    let result = blocking(move || task_file.read()).await;
    let result = match result {
        Ok(snapshot) => {
            documents.read(Document::Settings, snapshot.version);
            Ok(snapshot.value)
        }
        Err(error) => {
            tracing::error!("could not load the settings: {error}");
            Err(error)
        }
    };
    Event::SettingsLoaded {
        path: file.path().to_owned(),
        result,
    }
}

/// Writes `settings`, unless another program changed the file since the app last saw it:
/// answered with [`Event::SettingsLoaded`] once written or if it fails, and with
/// [`Event::SettingsChangedOnDisk`] if it changed.
pub(crate) async fn save(
    file: &SettingsFile,
    documents: &mut DocumentVersions,
    settings: Settings,
) -> Option<Event> {
    let (read_file, write_file) = (file.clone(), file.clone());
    let written = settings.clone();
    let saved = save_document(
        documents,
        Document::Settings,
        move || read_file.read(),
        move |read| write_file.write_over(read, &written),
    )
    .await;
    let result = match saved {
        Ok(Saved::Written) => Ok(settings),
        Ok(Saved::Waiting) => return None,
        Ok(Saved::ChangedOnDisk(settings)) => {
            return Some(Event::SettingsChangedOnDisk {
                settings,
                noticed: Noticed::Saving,
            });
        }
        Err(error) => {
            tracing::error!("could not save the settings: {error}");
            Err(error)
        }
    };
    Some(Event::SettingsLoaded {
        path: file.path().to_owned(),
        result,
    })
}

/// [`Event::SettingsChangedOnDisk`] if another program changed `settings.toml` since the app
/// last saw it, or [`Event::SettingsLoaded`] with the error if it can't be read now.
pub(crate) async fn look_again(
    file: &SettingsFile,
    documents: &mut DocumentVersions,
) -> Option<Event> {
    let task_file = file.clone();
    match look_at(documents, Document::Settings, move || task_file.read()).await {
        Ok(settings) => Some(Event::SettingsChangedOnDisk {
            settings: settings?,
            noticed: Noticed::Looking,
        }),
        Err(error) => Some(Event::SettingsLoaded {
            path: file.path().to_owned(),
            result: Err(error),
        }),
    }
}

/// The history's retention in what [`load`] or [`save`] read; `None` if the file couldn't be
/// read, so a broken file never prunes by the defaults.
pub(crate) fn history_retention(loaded: &Event) -> Option<HistorySettings> {
    match loaded {
        Event::SettingsLoaded {
            result: Ok(settings),
            ..
        } => Some(settings.history),
        _ => None,
    }
}
