use dbrover_core::{DataSource, DataSourceId, ProjectId, SourceColor};
use dbrover_engine::{Event, SessionState};
use dbrover_store::{McpPermissions, Project};

use crate::catalog_tree::CatalogTree;

/// What the UI knows about the world, built only from engine events.
#[derive(Debug, Default)]
pub struct AppState {
    pub engine_status: EngineStatus,
    pub data_sources: DataSources,
    pub projects: ProjectList,
    /// What projects allow MCP clients when they don't say otherwise.
    pub mcp_defaults: McpPermissions,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum EngineStatus {
    #[default]
    Starting,
    Running,
    Stopped,
}

impl EngineStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum DataSources {
    #[default]
    Loading,
    Unavailable(String),
    /// Sorted by name, case-insensitively.
    Loaded(Vec<DataSourceEntry>),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum ProjectList {
    #[default]
    Loading,
    /// `projects.toml` can't be read; every data source shows ungrouped until it is fixed.
    Unavailable(String),
    /// Sorted by name, case-insensitively.
    Loaded(Vec<Project>),
}

impl ProjectList {
    pub fn projects(&self) -> &[Project] {
        match self {
            Self::Loaded(projects) => projects,
            Self::Loading | Self::Unavailable(_) => &[],
        }
    }

    pub fn get(&self, id: ProjectId) -> Option<&Project> {
        self.projects().iter().find(|project| project.id == id)
    }

    fn set(&mut self, mut projects: Vec<Project>) {
        sort_projects(&mut projects);
        *self = Self::Loaded(projects);
    }

    fn upsert(&mut self, project: &Project) {
        let Self::Loaded(projects) = self else {
            return;
        };
        match projects.iter_mut().find(|p| p.id == project.id) {
            Some(existing) => *existing = project.clone(),
            None => projects.push(project.clone()),
        }
        sort_projects(projects);
    }

    fn remove(&mut self, id: ProjectId) {
        if let Self::Loaded(projects) = self {
            projects.retain(|project| project.id != id);
        }
    }
}

fn sort_projects(projects: &mut [Project]) {
    projects.sort_by_key(|project| project.name.to_lowercase());
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSourceEntry {
    pub data_source: DataSource,
    pub session: SessionState,
    pub catalog: CatalogTree,
}

impl DataSourceEntry {
    fn new(data_source: DataSource) -> Self {
        Self {
            data_source,
            session: SessionState::Disconnected,
            catalog: CatalogTree::default(),
        }
    }

    fn set_session(&mut self, state: &SessionState) {
        // Everything loaded came from the connection that just went away, and the next
        // connection may see a different catalog. A lost connection keeps it: the explorer
        // reconnects on its next request, which may be loading a node of it right now.
        if matches!(state, SessionState::Disconnected | SessionState::Failed(_)) {
            self.catalog.reset();
        }
        self.session = state.clone();
    }
}

impl AppState {
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Started => self.engine_status = EngineStatus::Running,
            Event::DataSourcesLoaded(data_sources) => {
                let mut entries: Vec<_> = data_sources
                    .iter()
                    .cloned()
                    .map(DataSourceEntry::new)
                    .collect();
                sort_by_name(&mut entries);
                self.data_sources = DataSources::Loaded(entries);
            }
            Event::DataSourcesUnavailable { error } => {
                self.data_sources = DataSources::Unavailable(error.clone());
            }
            Event::DataSourceSaved(data_source) => self.data_sources.upsert(data_source),
            Event::DataSourceDeleted(id) => self.data_sources.remove(*id),
            Event::ProjectsLoaded(projects) => {
                self.projects.set(projects.projects.clone());
                self.mcp_defaults = projects.mcp_defaults.clone();
            }
            Event::ProjectsUnavailable { error } => {
                self.projects = ProjectList::Unavailable(error.clone());
            }
            Event::ProjectSaved(project) => self.projects.upsert(project),
            Event::ProjectDeleted(id) => self.projects.remove(*id),
            Event::McpDefaultsSaved(defaults) => self.mcp_defaults = defaults.clone(),
            Event::SessionChanged { id, state } => {
                if let Some(entry) = self.data_sources.entry_mut(*id) {
                    entry.set_session(state);
                }
            }
            Event::ChildrenLoaded {
                id,
                path,
                query,
                result,
            } => {
                if let Some(catalog) = self.catalog_mut(*id) {
                    catalog.loaded(path, query, result);
                }
            }
            // The dialogs, the definition window and the consoles track these themselves.
            Event::DataSourceSaveFailed { .. }
            | Event::ProjectSaveFailed { .. }
            | Event::McpDefaultsSaveFailed { .. }
            | Event::ConsolesLoaded(_)
            | Event::DiagramsLoaded(_)
            | Event::DiagramSaveFailed { .. }
            | Event::KeymapLoaded { .. }
            | Event::CustomFileLoaded { .. }
            | Event::TrustedCommandsLoaded(_)
            | Event::ProgramFinished { .. }
            | Event::SettingsLoaded { .. }
            | Event::LatestRelease(_)
            | Event::UpdateProgress { .. }
            | Event::UpdateInstalled(_)
            | Event::HistoryFound { .. }
            | Event::HistoryExported { .. }
            | Event::ImportProgress { .. }
            | Event::RowsImported { .. }
            | Event::ExportProgress { .. }
            | Event::ExportFinished { .. }
            | Event::TableDescribed { .. }
            | Event::TablesDescribed { .. }
            | Event::EditsApplied { .. }
            | Event::RoleDescribed { .. }
            | Event::DdlApplied { .. }
            | Event::DdlLoaded { .. }
            | Event::TableLoaded { .. }
            | Event::TransactionChanged { .. }
            | Event::TransactionEnded { .. }
            | Event::ConsoleSaveFailed { .. }
            | Event::SqlFileRead { .. }
            | Event::SqlFileSaved { .. }
            | Event::SqlFileChanged { .. }
            | Event::ConnectionTested { .. }
            | Event::PasswordNeeded { .. }
            | Event::HostKeyUnknown { .. }
            | Event::PasswordCommandUntrusted { .. }
            | Event::HostKeyTrusted { .. }
            | Event::DataSourcesExported { .. }
            | Event::ImportPreviewed { .. }
            | Event::ImportApplied(_)
            | Event::PasswordProvided { .. }
            | Event::DefinitionLoaded { .. }
            | Event::DatabaseFileCreated { .. }
            | Event::ConsoleSessionChanged { .. }
            | Event::StatementsValidated { .. }
            | Event::StatementStarted { .. }
            | Event::RowsFetched { .. }
            | Event::StatementFinished { .. }
            | Event::RunFinished { .. } => {}
        }
    }

    pub fn data_source(&self, id: DataSourceId) -> Option<&DataSourceEntry> {
        self.data_sources
            .entries()
            .iter()
            .find(|e| e.data_source.id == id)
    }

    pub fn catalog_mut(&mut self, id: DataSourceId) -> Option<&mut CatalogTree> {
        self.data_sources
            .entry_mut(id)
            .map(|entry| &mut entry.catalog)
    }

    pub fn connected_count(&self) -> usize {
        self.data_sources
            .entries()
            .iter()
            .filter(|entry| matches!(entry.session, SessionState::Connected(_)))
            .count()
    }
}

impl DataSources {
    pub fn entries(&self) -> &[DataSourceEntry] {
        match self {
            Self::Loaded(entries) => entries,
            Self::Loading | Self::Unavailable(_) => &[],
        }
    }

    pub fn color_of(&self, id: DataSourceId) -> Option<SourceColor> {
        self.entries()
            .iter()
            .find(|entry| entry.data_source.id == id)?
            .data_source
            .color
    }

    fn entry_mut(&mut self, id: DataSourceId) -> Option<&mut DataSourceEntry> {
        match self {
            Self::Loaded(entries) => entries.iter_mut().find(|e| e.data_source.id == id),
            Self::Loading | Self::Unavailable(_) => None,
        }
    }

    /// Updates the saved data source, keeping its session state, or adds it.
    fn upsert(&mut self, data_source: &DataSource) {
        let Self::Loaded(entries) = self else {
            return;
        };
        match entries
            .iter_mut()
            .find(|e| e.data_source.id == data_source.id)
        {
            Some(entry) => entry.data_source = data_source.clone(),
            None => entries.push(DataSourceEntry::new(data_source.clone())),
        }
        sort_by_name(entries);
    }

    fn remove(&mut self, id: DataSourceId) {
        if let Self::Loaded(entries) = self {
            entries.retain(|entry| entry.data_source.id != id);
        }
    }
}

fn sort_by_name(entries: &mut [DataSourceEntry]) {
    entries.sort_by_key(|entry| entry.data_source.name.to_lowercase());
}

#[cfg(test)]
mod tests {
    use dbrover_core::{ConnectionConfig, ServerInfo, SqliteConfig};

    use super::*;

    fn data_source(name: &str) -> DataSource {
        DataSource {
            id: DataSourceId::generate(),
            name: name.to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Sqlite(SqliteConfig {
                path: format!("/data/{name}.db").into(),
                read_only: false,
            }),
        }
    }

    fn names(state: &AppState) -> Vec<&str> {
        state
            .data_sources
            .entries()
            .iter()
            .map(|entry| entry.data_source.name.as_str())
            .collect()
    }

    #[test]
    fn engine_is_running_once_it_reports_started() {
        let mut state = AppState::default();
        assert_eq!(state.engine_status, EngineStatus::Starting);

        state.apply(&Event::Started);

        assert_eq!(state.engine_status, EngineStatus::Running);
    }

    #[test]
    fn keeps_data_sources_sorted_through_saves_and_deletes() {
        let mut state = AppState::default();
        let beta = data_source("beta");
        state.apply(&Event::DataSourcesLoaded(vec![
            beta.clone(),
            data_source("Alpha"),
        ]));
        assert_eq!(names(&state), ["Alpha", "beta"]);

        state.apply(&Event::DataSourceSaved(data_source("gamma")));
        state.apply(&Event::DataSourceDeleted(beta.id));

        assert_eq!(names(&state), ["Alpha", "gamma"]);
    }

    #[test]
    fn keeps_projects_sorted_through_saves_renames_and_deletes() {
        let project = |name: &str| {
            Project::new(
                ProjectId::generate(),
                name.to_owned(),
                dbrover_store::ProjectSlug::from_name(name),
            )
        };
        let project_names = |state: &AppState| -> Vec<String> {
            state
                .projects
                .projects()
                .iter()
                .map(|project| project.name.clone())
                .collect()
        };
        let mut state = AppState::default();
        let mut shop = project("shop");
        state.apply(&Event::ProjectsLoaded(dbrover_store::Projects {
            mcp_defaults: dbrover_store::McpPermissions::default(),
            projects: vec![shop.clone(), project("Analytics")],
        }));
        assert_eq!(project_names(&state), ["Analytics", "shop"]);

        shop.name = "Archive".to_owned();
        state.apply(&Event::ProjectSaved(shop.clone()));
        state.apply(&Event::ProjectSaved(project("Billing")));
        assert_eq!(project_names(&state), ["Analytics", "Archive", "Billing"]);

        state.apply(&Event::ProjectDeleted(shop.id));
        assert_eq!(project_names(&state), ["Analytics", "Billing"]);
        assert!(state.projects.get(shop.id).is_none());
    }

    #[test]
    fn renaming_keeps_the_session() {
        let mut state = AppState::default();
        let mut renamed = data_source("old");
        state.apply(&Event::DataSourcesLoaded(vec![renamed.clone()]));
        let connected = SessionState::Connected(ServerInfo {
            product: "SQLite".to_owned(),
            version: "3.50.0".to_owned(),
        });
        state.apply(&Event::SessionChanged {
            id: renamed.id,
            state: connected.clone(),
        });

        renamed.name = "new".to_owned();
        state.apply(&Event::DataSourceSaved(renamed.clone()));

        let entry = state.data_source(renamed.id).unwrap();
        assert_eq!(entry.data_source.name, "new");
        assert_eq!(entry.session, connected);
        assert_eq!(state.connected_count(), 1);
    }

    #[test]
    fn failing_to_connect_forgets_the_loaded_catalog_but_losing_it_does_not() {
        let mut state = AppState::default();
        let source = data_source("app");
        state.apply(&Event::DataSourcesLoaded(vec![source.clone()]));
        let root = dbrover_core::CatalogPath::root();
        let catalog = state.catalog_mut(source.id).unwrap();
        assert!(catalog.expand(&root));

        state.apply(&Event::SessionChanged {
            id: source.id,
            state: SessionState::Connecting,
        });
        assert!(
            state
                .data_source(source.id)
                .unwrap()
                .catalog
                .is_expanded(&root)
        );
        // A lost connection keeps the tree: the explorer reconnects on its next request,
        // which may be loading a node of it.
        state.apply(&Event::SessionChanged {
            id: source.id,
            state: SessionState::Lost("the connection was lost".to_owned()),
        });
        assert!(
            state
                .data_source(source.id)
                .unwrap()
                .catalog
                .is_expanded(&root)
        );
        state.apply(&Event::SessionChanged {
            id: source.id,
            state: SessionState::Failed("timed out".to_owned()),
        });

        assert!(
            !state
                .data_source(source.id)
                .unwrap()
                .catalog
                .is_expanded(&root)
        );
    }
}
