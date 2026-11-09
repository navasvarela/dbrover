use dbrover_core::{ProjectId, describe_error};
use dbrover_store::{
    FileVersion, McpOverrides, McpPermissions, Project, ProjectFile, ProjectStoreError, Projects,
    Snapshot,
};
use tokio::task::JoinError;

use crate::Event;
use crate::outside_changes::{EntryChanges, Noticed, entry_changes};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProjectError {
    #[error("projects can't be changed until projects.toml is fixed: {0}")]
    Unavailable(String),
    #[error("there is no such project")]
    Unknown,
    #[error("a project's name can't be empty")]
    EmptyName,
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Store(#[from] ProjectStoreError),
    #[error("a background task failed")]
    Task(#[source] JoinError),
}

/// The projects the engine knows about, kept in sync with `projects.toml`. The same shape as
/// the data sources' `Registry`, for the same reasons: a file that can't be read is never
/// written over, and each change is applied on top of what the file holds when it is made.
pub(crate) struct ProjectRegistry {
    file: ProjectFile,
    state: State,
    /// What another program changed in the file, since the engine last asked.
    outside: Option<OutsideChanges>,
}

enum State {
    Loaded {
        projects: Projects,
        version: FileVersion,
    },
    Unavailable {
        reason: String,
    },
}

/// What another program changed in `projects.toml`.
#[derive(Debug)]
pub(crate) enum OutsideChanges {
    Entries {
        projects: EntryChanges<Project, ProjectId>,
        /// The defaults, if they changed.
        mcp_defaults: Option<McpPermissions>,
        noticed: Noticed,
    },
    /// The file couldn't be read before, and can now.
    Readable,
}

impl ProjectRegistry {
    pub(crate) async fn load(file: ProjectFile) -> Self {
        let state = match read(&file).await {
            Ok(snapshot) => State::Loaded {
                projects: snapshot.value,
                version: snapshot.version,
            },
            Err(error) => {
                let reason = describe_error(&error);
                tracing::error!("could not load projects: {reason}");
                State::Unavailable { reason }
            }
        };
        Self {
            file,
            state,
            outside: None,
        }
    }

    pub(crate) fn loaded_event(&self) -> Event {
        match &self.state {
            State::Loaded { projects, .. } => Event::ProjectsLoaded(projects.clone()),
            State::Unavailable { reason } => Event::ProjectsUnavailable {
                error: reason.clone(),
            },
        }
    }

    pub(crate) fn contains(&self, id: ProjectId) -> bool {
        self.projects()
            .is_ok_and(|projects| projects.get(id).is_some())
    }

    /// What another program changed in the file since the last call.
    pub(crate) fn take_outside_changes(&mut self) -> Option<OutsideChanges> {
        self.outside.take()
    }

    /// Reads the file again if another program changed it. A file that can't be read now is
    /// left alone, and so are the projects in memory.
    pub(crate) async fn look_again(&mut self) {
        let snapshot = match read(&self.file).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::error!(
                    "projects.toml changed on disk and can't be read: {}",
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
                    projects: snapshot.value,
                    version: snapshot.version,
                };
                self.outside = Some(OutsideChanges::Readable);
            }
        }
    }

    /// Adds a project called `name`, with a slug no other project has, and saves the file.
    pub(crate) async fn create(
        &mut self,
        id: ProjectId,
        name: &str,
    ) -> Result<Project, ProjectError> {
        let name = non_empty(name)?;
        self.update(|projects| {
            let project = Project::new(id, name.to_owned(), projects.unused_slug(name));
            projects.projects.push(project.clone());
            Ok(project)
        })
        .await
    }

    /// Renames the project and saves the file. The slug stays, so MCP clients configured with
    /// it keep working.
    pub(crate) async fn rename(
        &mut self,
        id: ProjectId,
        name: &str,
    ) -> Result<Project, ProjectError> {
        let name = non_empty(name)?;
        self.update(|projects| {
            let project = projects
                .projects
                .iter_mut()
                .find(|project| project.id == id)
                .ok_or(ProjectError::Unknown)?;
            name.clone_into(&mut project.name);
            Ok(project.clone())
        })
        .await
    }

    /// What `projects.toml` holds, or why it can't be used.
    pub(crate) fn projects(&self) -> Result<&Projects, ProjectError> {
        match &self.state {
            State::Loaded { projects, .. } => Ok(projects),
            State::Unavailable { reason } => Err(ProjectError::Unavailable(reason.clone())),
        }
    }

    /// Adds each project, or replaces the one with its id, and saves the file once.
    pub(crate) async fn import(&mut self, imported: Vec<Project>) -> Result<(), ProjectError> {
        self.update(|projects| {
            for project in imported {
                match projects.projects.iter_mut().find(|p| p.id == project.id) {
                    Some(existing) => *existing = project,
                    None => projects.projects.push(project),
                }
            }
            projects.check_unique().map_err(ProjectError::Conflict)
        })
        .await
    }

    /// Replaces the project's MCP settings and saves the file.
    pub(crate) async fn save_mcp(
        &mut self,
        id: ProjectId,
        mcp_enabled: bool,
        overrides: McpOverrides,
    ) -> Result<Project, ProjectError> {
        self.update(|projects| {
            let project = projects
                .projects
                .iter_mut()
                .find(|project| project.id == id)
                .ok_or(ProjectError::Unknown)?;
            project.mcp_enabled = mcp_enabled;
            project.mcp = overrides;
            Ok(project.clone())
        })
        .await
    }

    /// Replaces the MCP defaults and saves the file.
    pub(crate) async fn save_mcp_defaults(
        &mut self,
        defaults: McpPermissions,
    ) -> Result<McpPermissions, ProjectError> {
        self.update(|projects| {
            projects.mcp_defaults = defaults.clone();
            Ok(defaults)
        })
        .await
    }

    /// Removes the project and saves the file.
    pub(crate) async fn remove(&mut self, id: ProjectId) -> Result<(), ProjectError> {
        self.update(|projects| {
            let before = projects.projects.len();
            projects.projects.retain(|project| project.id != id);
            if projects.projects.len() == before {
                return Err(ProjectError::Unknown);
            }
            Ok(())
        })
        .await
    }

    /// Reads the file, takes in what another program changed, applies `change` on top, and
    /// writes the result. Memory changes only once the file is written.
    async fn update<T>(
        &mut self,
        change: impl FnOnce(&mut Projects) -> Result<T, ProjectError>,
    ) -> Result<T, ProjectError> {
        self.projects()?;
        let snapshot = read(&self.file).await?;
        self.take_in(snapshot.clone(), Noticed::Saving);
        let mut updated = snapshot.value.clone();
        let result = change(&mut updated)?;

        let file = self.file.clone();
        let to_write = updated.clone();
        let version = tokio::task::spawn_blocking(move || file.write_over(&snapshot, &to_write))
            .await
            .map_err(ProjectError::Task)??;
        if let Some(OutsideChanges::Entries {
            projects,
            mcp_defaults,
            ..
        }) = &mut self.outside
        {
            projects.as_of(&updated.projects, |project| project.id);
            if mcp_defaults.is_some() {
                *mcp_defaults = Some(updated.mcp_defaults.clone());
            }
        }
        self.state = State::Loaded {
            projects: updated,
            version,
        };
        Ok(result)
    }

    /// Makes `snapshot` what the engine knows, noting what changed if another program wrote
    /// it since the engine last read or wrote it.
    fn take_in(&mut self, snapshot: Snapshot<Projects>, noticed: Noticed) {
        let State::Loaded { projects, version } = &mut self.state else {
            return;
        };
        if *version == snapshot.version {
            return;
        }
        let changes = entry_changes(&projects.projects, &snapshot.value.projects, |p| p.id);
        let mcp_defaults = (projects.mcp_defaults != snapshot.value.mcp_defaults)
            .then(|| snapshot.value.mcp_defaults.clone());
        *projects = snapshot.value;
        *version = snapshot.version;
        if !changes.is_empty() || mcp_defaults.is_some() {
            self.outside = Some(OutsideChanges::Entries {
                projects: changes,
                mcp_defaults,
                noticed,
            });
        }
    }
}

async fn read(file: &ProjectFile) -> Result<Snapshot<Projects>, ProjectError> {
    let file = file.clone();
    tokio::task::spawn_blocking(move || file.read())
        .await
        .map_err(ProjectError::Task)?
        .map_err(ProjectError::from)
}

fn non_empty(name: &str) -> Result<&str, ProjectError> {
    match name.trim() {
        "" => Err(ProjectError::EmptyName),
        name => Ok(name),
    }
}
