use dbrover_core::{DataSource, DataSourceId, ProjectId, describe_error};
use dbrover_store::{DataSourceFile, FileVersion, Snapshot, StoreError};
use tokio::task::JoinError;

use crate::Event;
use crate::outside_changes::{EntryChanges, Noticed, entry_changes};

#[derive(Debug, thiserror::Error)]
pub(crate) enum RegistryError {
    #[error("data sources can't be changed until data_sources.toml is fixed: {0}")]
    Unavailable(String),
    #[error("there is no such data source")]
    Unknown,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("a background task failed")]
    Task(#[source] JoinError),
}

/// The data sources the engine knows about, kept in sync with `data_sources.toml`.
///
/// Every change is applied to what the file holds when it is made, read again just before:
/// another program's change to the file (a `git pull`) is taken in first, and the one entry
/// being changed goes on top of it, so the other entries survive.
pub(crate) struct Registry {
    file: DataSourceFile,
    state: State,
    /// What another program changed in the file, since the engine last asked.
    outside: Option<OutsideChanges>,
}

enum State {
    Loaded {
        data_sources: Vec<DataSource>,
        version: FileVersion,
    },
    /// The file exists but couldn't be read. Every change is refused, so the file is never
    /// overwritten with an empty list and the user's data sources stay recoverable.
    Unavailable { reason: String },
}

/// Data sources another program changed in `data_sources.toml`.
#[derive(Debug)]
pub(crate) enum OutsideChanges {
    Entries {
        changes: EntryChanges<DataSource, DataSourceId>,
        noticed: Noticed,
    },
    /// The file couldn't be read before, and can now: every data source is new to the app.
    Readable,
}

impl Registry {
    pub(crate) async fn load(file: DataSourceFile) -> Self {
        let state = match read(&file).await {
            Ok(snapshot) => State::Loaded {
                data_sources: snapshot.value,
                version: snapshot.version,
            },
            Err(error) => {
                let reason = describe_error(&error);
                tracing::error!("could not load data sources: {reason}");
                State::Unavailable { reason }
            }
        };
        Self {
            file,
            state,
            outside: None,
        }
    }

    /// The event that tells the UI what was loaded.
    pub(crate) fn loaded_event(&self) -> Event {
        match &self.state {
            State::Loaded { data_sources, .. } => Event::DataSourcesLoaded(data_sources.clone()),
            State::Unavailable { reason } => Event::DataSourcesUnavailable {
                error: reason.clone(),
            },
        }
    }

    pub(crate) fn get(&self, id: DataSourceId) -> Option<&DataSource> {
        match &self.state {
            State::Loaded { data_sources, .. } => data_sources.iter().find(|ds| ds.id == id),
            State::Unavailable { .. } => None,
        }
    }

    /// Every data source, or why `data_sources.toml` can't be used.
    pub(crate) fn data_sources(&self) -> Result<&[DataSource], RegistryError> {
        match &self.state {
            State::Loaded { data_sources, .. } => Ok(data_sources),
            State::Unavailable { reason } => Err(RegistryError::Unavailable(reason.clone())),
        }
    }

    pub(crate) fn ensure_available(&self) -> Result<(), RegistryError> {
        self.data_sources().map(|_| ())
    }

    /// What another program changed in the file since the last call, found by a save or by
    /// [`look_again`](Self::look_again).
    pub(crate) fn take_outside_changes(&mut self) -> Option<OutsideChanges> {
        self.outside.take()
    }

    /// Reads the file again if another program changed it. A file that can't be read now is
    /// left alone, and so are the data sources in memory; saves are refused until it is fixed.
    pub(crate) async fn look_again(&mut self) {
        let snapshot = match read(&self.file).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::error!(
                    "data_sources.toml changed on disk and can't be read: {}",
                    describe_error(&error)
                );
                return;
            }
        };
        match &self.state {
            State::Loaded { version, .. } if *version == snapshot.version => {}
            State::Loaded { .. } => self.take_in(snapshot, Noticed::Looking),
            State::Unavailable { .. } => {
                self.state = State::Loaded {
                    data_sources: snapshot.value,
                    version: snapshot.version,
                };
                self.outside = Some(OutsideChanges::Readable);
            }
        }
    }

    /// Adds the data source, or replaces the one with the same id, and saves the file.
    pub(crate) async fn upsert(&mut self, data_source: DataSource) -> Result<(), RegistryError> {
        self.update(|data_sources| upsert(data_sources, data_source))
            .await
    }

    /// Adds each data source, or replaces the one with its id, and saves the file once.
    pub(crate) async fn upsert_all(
        &mut self,
        imported: Vec<DataSource>,
    ) -> Result<(), RegistryError> {
        self.update(|data_sources| {
            for data_source in imported {
                upsert(data_sources, data_source);
            }
        })
        .await
    }

    /// Removes the data source and saves the file. Returns `false` if it wasn't there.
    pub(crate) async fn remove(&mut self, id: DataSourceId) -> Result<bool, RegistryError> {
        self.update(|data_sources| {
            let before = data_sources.len();
            data_sources.retain(|ds| ds.id != id);
            data_sources.len() != before
        })
        .await
    }

    /// Puts the data source in `project` (`None` for no project) and saves the file.
    pub(crate) async fn move_to_project(
        &mut self,
        id: DataSourceId,
        project: Option<ProjectId>,
    ) -> Result<DataSource, RegistryError> {
        let moved = self
            .update(|data_sources| {
                let data_source = data_sources.iter_mut().find(|ds| ds.id == id)?;
                data_source.project = project;
                Some(data_source.clone())
            })
            .await?;
        moved.ok_or(RegistryError::Unknown)
    }

    /// Moves every data source in `project` to no project, saves the file, and returns the
    /// data sources it moved.
    pub(crate) async fn leave_project(
        &mut self,
        project: ProjectId,
    ) -> Result<Vec<DataSource>, RegistryError> {
        self.update(|data_sources| {
            data_sources
                .iter_mut()
                .filter(|ds| ds.project == Some(project))
                .map(|data_source| {
                    data_source.project = None;
                    data_source.clone()
                })
                .collect()
        })
        .await
    }

    /// Reads the file, takes in what another program changed, applies `change` on top, and
    /// writes the result. Memory changes only once the file is written, so the two never
    /// disagree after a failed write.
    async fn update<R>(
        &mut self,
        change: impl FnOnce(&mut Vec<DataSource>) -> R,
    ) -> Result<R, RegistryError> {
        self.ensure_available()?;
        let snapshot = read(&self.file).await?;
        self.take_in(snapshot.clone(), Noticed::Saving);
        let mut updated = snapshot.value.clone();
        let result = change(&mut updated);

        let file = self.file.clone();
        let to_write = updated.clone();
        let version = tokio::task::spawn_blocking(move || file.write_over(&snapshot, &to_write))
            .await
            .map_err(RegistryError::Task)??;
        if let Some(OutsideChanges::Entries { changes, .. }) = &mut self.outside {
            changes.as_of(&updated, |ds| ds.id);
        }
        self.state = State::Loaded {
            data_sources: updated,
            version,
        };
        Ok(result)
    }

    /// Makes `snapshot` what the engine knows, noting what changed if another program wrote
    /// it since the engine last read or wrote it.
    fn take_in(&mut self, snapshot: Snapshot<Vec<DataSource>>, noticed: Noticed) {
        let State::Loaded {
            data_sources,
            version,
        } = &mut self.state
        else {
            return;
        };
        if *version == snapshot.version {
            return;
        }
        let changes = entry_changes(data_sources, &snapshot.value, |ds| ds.id);
        *data_sources = snapshot.value;
        *version = snapshot.version;
        if !changes.is_empty() {
            self.outside = Some(OutsideChanges::Entries { changes, noticed });
        }
    }
}

fn upsert(data_sources: &mut Vec<DataSource>, data_source: DataSource) {
    match data_sources.iter_mut().find(|ds| ds.id == data_source.id) {
        Some(existing) => *existing = data_source,
        None => data_sources.push(data_source),
    }
}

async fn read(file: &DataSourceFile) -> Result<Snapshot<Vec<DataSource>>, RegistryError> {
    let file = file.clone();
    tokio::task::spawn_blocking(move || file.read())
        .await
        .map_err(RegistryError::Task)?
        .map_err(RegistryError::from)
}
