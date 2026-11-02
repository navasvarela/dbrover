//! Exporting data sources on one "machine" (an engine with its own folder and keychain) and
//! importing them on another.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{MemorySecrets, TestEngine};
use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, MysqlConfig, Password, Port, PostgresConfig,
    ProjectId, ServerConfig, SourceColor, SqlServerConfig, SqliteConfig, TlsMode,
};
use dbrover_engine::{Command, Event, PasswordUpdate};
use dbrover_store::{EntryStatus, ImportPreview, Project, Projects};

fn server(host: &str, port: Port, tls: TlsMode) -> ServerConfig {
    ServerConfig {
        host: host.to_owned(),
        port,
        database: "app".to_owned(),
        user: "juan".to_owned(),
        tls,
        ssh: None,
        password_command: None,
        cloud_sign_in: None,
    }
}

/// One data source of every kind, each with its own password.
fn every_kind(shared: &Path) -> Vec<(DataSource, Password)> {
    let configs = [
        (
            "production",
            ConnectionConfig::Postgres(PostgresConfig {
                server: server(
                    "db.internal",
                    5432.into(),
                    TlsMode::VerifyFull {
                        ca_file: Some(shared.join("internal-ca.pem")),
                    },
                ),
            }),
        ),
        (
            "shop",
            ConnectionConfig::Mysql(MysqlConfig {
                server: server(
                    "${SHOP_HOST}",
                    Port::Placeholder("${SHOP_PORT}".to_owned()),
                    TlsMode::Require,
                ),
            }),
        ),
        (
            "sales",
            ConnectionConfig::SqlServer(SqlServerConfig {
                server: server("sql.internal", 1433.into(), TlsMode::Disable),
            }),
        ),
        (
            "chinook",
            ConnectionConfig::Sqlite(SqliteConfig {
                path: shared.join("chinook.db"),
                read_only: true,
            }),
        ),
    ];
    configs
        .into_iter()
        .map(|(name, config)| {
            let data_source = DataSource {
                id: DataSourceId::generate(),
                name: name.to_owned(),
                color: Some(SourceColor::Teal),
                safety: dbrover_core::SafetyLevel::Normal,
                project: None,
                mcp_login: None,
                config,
            };
            let password = Password::new(format!("secret-of-{name}-{}", data_source.id));
            (data_source, password)
        })
        .collect()
}

struct Machine {
    engine: TestEngine,
    dir: tempfile::TempDir,
}

impl Machine {
    fn start(secrets: Arc<MemorySecrets>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), secrets);
        engine.expect_started(vec![]);
        Self { engine, dir }
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }

    fn save(&mut self, data_source: &DataSource, password: &Password) {
        self.engine.send(Command::SaveDataSource {
            data_source: data_source.clone(),
            password: PasswordUpdate::Set(password.clone()),
            ssh_secret: PasswordUpdate::Keep,
        });
        self.engine
            .expect(Event::DataSourceSaved(data_source.clone()));
    }

    fn create_project(&mut self, name: &str) -> Project {
        let id = ProjectId::generate();
        self.engine.send(Command::CreateProject {
            id,
            name: name.to_owned(),
        });
        match self.engine.next_event() {
            Event::ProjectSaved(project) if project.id == id => project,
            other => panic!("expected the project to be saved, got {other:?}"),
        }
    }

    fn move_to(&mut self, data_source: &mut DataSource, project: &Project) {
        data_source.project = Some(project.id);
        self.engine.send(Command::MoveToProject {
            data_source: data_source.id,
            project: Some(project.id),
        });
        self.engine
            .expect(Event::DataSourceSaved(data_source.clone()));
    }

    fn export(&mut self, path: &Path, data_sources: Vec<DataSourceId>, projects: Vec<ProjectId>) {
        self.engine.send(Command::ExportDataSources {
            path: path.to_owned(),
            data_sources: data_sources.clone(),
            projects,
        });
        self.engine.expect(Event::DataSourcesExported {
            path: path.to_owned(),
            result: Ok(data_sources.len()),
        });
    }

    fn preview(&mut self, path: &Path) -> ImportPreview {
        self.engine.send(Command::PreviewImport(path.to_owned()));
        match self.engine.next_event() {
            Event::ImportPreviewed {
                result: Ok(preview),
                ..
            } => *preview,
            other => panic!("expected a preview, got {other:?}"),
        }
    }
}

/// A machine with every kind of data source saved with a password, two of them in a
/// project, and their export in a shared folder next to the SQLite database.
fn exporting_machine(shared: &Path) -> (Machine, Vec<(DataSource, Password)>, Project) {
    let secrets = Arc::new(MemorySecrets::default());
    let mut machine = Machine::start(Arc::clone(&secrets));
    let mut saved = every_kind(shared);
    for (data_source, password) in &saved {
        machine.save(data_source, password);
    }
    let project = machine.create_project("Analytics");
    for (data_source, _) in saved.iter_mut().take(2) {
        machine.move_to(data_source, &project);
    }
    for (data_source, password) in &saved {
        assert_eq!(secrets.database(data_source.id).as_ref(), Some(password));
    }
    (machine, saved, project)
}

#[test]
fn an_export_never_holds_a_saved_password() {
    let shared = tempfile::tempdir().unwrap();
    let (mut machine, saved, project) = exporting_machine(shared.path());
    let path = shared.path().join("dbrover.toml");

    machine.export(
        &path,
        saved.iter().map(|(ds, _)| ds.id).collect(),
        vec![project.id],
    );

    let text = std::fs::read_to_string(&path).unwrap();
    for (data_source, password) in &saved {
        assert!(text.contains(&data_source.id.to_string()), "{text}");
        assert!(
            !text.contains(password.expose()),
            "the export holds {}'s password:\n{text}",
            data_source.name
        );
    }
    let mut settings = text.lines().filter(|line| !line.starts_with('#'));
    assert!(
        settings.all(|line| !line.to_lowercase().contains("password")),
        "{text}"
    );
}

#[test]
fn round_trips_through_an_export_and_reimporting_changes_nothing() {
    let shared = tempfile::tempdir().unwrap();
    let (mut exporter, saved, project) = exporting_machine(shared.path());
    let path: PathBuf = shared.path().join("dbrover.toml");
    // Only the project is chosen: its data sources come with it, and the rest stay behind.
    let in_project: Vec<DataSource> = saved
        .iter()
        .map(|(ds, _)| ds.clone())
        .filter(|ds| ds.project == Some(project.id))
        .collect();
    exporter.export(
        &path,
        in_project.iter().map(|ds| ds.id).collect(),
        vec![project.id],
    );

    let mut importer = Machine::start(Arc::default());
    let preview = importer.preview(&path);
    assert_eq!(preview.projects.len(), 1);
    assert!(
        preview
            .projects
            .iter()
            .map(|entry| &entry.status)
            .chain(preview.data_sources.iter().map(|entry| &entry.status))
            .all(|status| *status == EntryStatus::New)
    );
    importer.engine.send(Command::ApplyImport(
        preview.plan(&preview.default_choices()),
    ));
    importer.engine.expect(Event::ProjectSaved(project.clone()));
    for data_source in &in_project {
        importer
            .engine
            .expect(Event::DataSourceSaved(data_source.clone()));
    }
    importer.engine.expect(Event::ImportApplied(Ok(())));

    let files_after_import = (
        importer.file("data_sources.toml"),
        importer.file("projects.toml"),
    );
    let again = importer.preview(&path);
    assert!(
        again
            .projects
            .iter()
            .map(|entry| &entry.status)
            .chain(again.data_sources.iter().map(|entry| &entry.status))
            .all(|status| *status == EntryStatus::Unchanged)
    );
    let plan = again.plan(&again.default_choices());
    assert!(plan.is_empty(), "{plan:?}");
    importer.engine.send(Command::ApplyImport(plan));
    importer.engine.expect(Event::ImportApplied(Ok(())));
    assert_eq!(
        (
            importer.file("data_sources.toml"),
            importer.file("projects.toml"),
        ),
        files_after_import
    );

    importer.engine.shut_down();
    let mut restarted = TestEngine::start(
        &importer.dir.path().join("data_sources.toml"),
        Arc::default(),
    );
    restarted.expect_started_with_projects(
        Projects {
            projects: vec![project],
            ..Projects::default()
        },
        in_project,
    );
}

#[test]
fn says_why_an_unreadable_file_cannot_be_imported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-an-export.toml");
    std::fs::write(&path, "format_version = 99\n").unwrap();
    let mut machine = Machine::start(Arc::default());

    machine.engine.send(Command::PreviewImport(path.clone()));

    match machine.engine.next_event() {
        Event::ImportPreviewed {
            path: previewed,
            result: Err(error),
        } if previewed == path => assert!(error.contains("newer DbRover"), "{error}"),
        other => panic!("expected the import to be refused, got {other:?}"),
    }
}
