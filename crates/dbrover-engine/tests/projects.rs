//! Projects through the engine: creating, renaming and deleting them, and moving data sources
//! between them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{MemorySecrets, TestEngine, sqlite_data_source};
use dbrover_core::{DataSource, DataSourceId, Password, ProjectId};
use dbrover_engine::{Command, Event, PasswordUpdate};
use dbrover_store::{
    DataSourceFile, KeymapBindings, McpOverrides, McpPermissions, Project, ProjectFile,
    SchemaScope, Settings,
};

fn save(engine: &mut TestEngine, data_source: &DataSource) {
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: PasswordUpdate::Keep,
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
}

fn expect_project_saved(engine: &mut TestEngine) -> Project {
    match engine.next_event() {
        Event::ProjectSaved(project) => project,
        other => panic!("expected a saved project, got {other:?}"),
    }
}

fn create(engine: &mut TestEngine, name: &str) -> Project {
    let id = ProjectId::generate();
    engine.send(Command::CreateProject {
        id,
        name: name.to_owned(),
    });
    let project = expect_project_saved(engine);
    assert_eq!(project.id, id);
    project
}

fn move_to(engine: &mut TestEngine, data_source: &mut DataSource, project: Option<ProjectId>) {
    engine.send(Command::MoveToProject {
        data_source: data_source.id,
        project,
    });
    data_source.project = project;
    engine.expect(Event::DataSourceSaved(data_source.clone()));
}

fn projects_file(dir: &Path) -> ProjectFile {
    ProjectFile::new(dir.join("projects.toml"))
}

#[test]
fn groups_data_sources_into_projects_that_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data_sources.toml");
    let mut engine = TestEngine::start(&file, Arc::default());
    engine.expect_started(vec![]);
    let mut orders = sqlite_data_source(dir.path().join("orders.db"));
    let mut events = sqlite_data_source(dir.path().join("events.db"));
    save(&mut engine, &orders);
    save(&mut engine, &events);

    let shop = create(&mut engine, "Shop");
    let analytics = create(&mut engine, "Analytics");
    assert_eq!(shop.slug.as_str(), "shop");
    assert!(!shop.mcp_enabled, "a new project is invisible to MCP");
    assert_eq!(create(&mut engine, "shop").slug.as_str(), "shop-2");
    move_to(&mut engine, &mut orders, Some(shop.id));
    move_to(&mut engine, &mut events, Some(analytics.id));

    engine.send(Command::RenameProject {
        id: shop.id,
        name: " Web shop ".to_owned(),
    });
    let renamed = expect_project_saved(&mut engine);
    assert_eq!(renamed.name, "Web shop");
    assert_eq!(
        renamed.slug, shop.slug,
        "renaming keeps the slug clients use"
    );
    engine.shut_down();

    let saved = projects_file(dir.path()).load().unwrap();
    assert_eq!(saved.get(shop.id), Some(&renamed));
    let mut restarted = TestEngine::start(&file, Arc::default());
    restarted.expect_started_with_projects(saved, vec![orders, events]);
}

#[test]
fn deleting_a_project_moves_its_data_sources_to_no_project() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data_sources.toml");
    let mut engine = TestEngine::start(&file, Arc::default());
    engine.expect_started(vec![]);
    let mut inside = sqlite_data_source(dir.path().join("inside.db"));
    let outside = sqlite_data_source(dir.path().join("outside.db"));
    save(&mut engine, &inside);
    save(&mut engine, &outside);
    let project = create(&mut engine, "Doomed");
    move_to(&mut engine, &mut inside, Some(project.id));

    engine.send(Command::DeleteProject(project.id));

    inside.project = None;
    engine.expect(Event::DataSourceSaved(inside.clone()));
    engine.expect(Event::ProjectDeleted(project.id));
    assert_eq!(
        DataSourceFile::new(&file).load().unwrap(),
        vec![inside, outside]
    );
    assert!(
        projects_file(dir.path())
            .load()
            .unwrap()
            .projects
            .is_empty()
    );
    engine.shut_down();
}

#[test]
fn refuses_unknown_projects_and_empty_names() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data_sources.toml");
    let mut engine = TestEngine::start(&file, Arc::default());
    engine.expect_started(vec![]);
    let data_source = sqlite_data_source(dir.path().join("app.db"));
    save(&mut engine, &data_source);
    let unknown = ProjectId::generate();

    engine.send(Command::MoveToProject {
        data_source: data_source.id,
        project: Some(unknown),
    });
    assert!(matches!(
        engine.next_event(),
        Event::DataSourceSaveFailed { id, .. } if id == data_source.id
    ));
    for command in [
        Command::CreateProject {
            id: unknown,
            name: "  ".to_owned(),
        },
        Command::RenameProject {
            id: unknown,
            name: "Renamed".to_owned(),
        },
        Command::DeleteProject(unknown),
    ] {
        engine.send(command);
        assert!(matches!(
            engine.next_event(),
            Event::ProjectSaveFailed { id, .. } if id == unknown
        ));
    }
    assert_eq!(
        DataSourceFile::new(&file).load().unwrap(),
        vec![data_source]
    );
    engine.shut_down();
}

#[test]
fn never_overwrites_an_unreadable_projects_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data_sources.toml");
    let broken = "[[project]]\nname = 3\n";
    std::fs::write(dir.path().join("projects.toml"), broken).unwrap();
    let mut engine = TestEngine::start(&file, Arc::default());
    engine.expect(Event::Started);
    engine.expect_keymap(Ok(KeymapBindings::new()));
    engine.expect_settings(Ok(Settings::default()));
    engine.expect_no_custom_files();
    engine.expect_nothing_trusted();
    assert!(matches!(
        engine.next_event(),
        Event::ProjectsUnavailable { .. }
    ));
    engine.expect(Event::DataSourcesLoaded(vec![]));
    engine.expect(Event::ConsolesLoaded(vec![]));
    engine.expect(Event::DiagramsLoaded(vec![]));

    let id = ProjectId::generate();
    engine.send(Command::CreateProject {
        id,
        name: "Shop".to_owned(),
    });
    assert!(matches!(
        engine.next_event(),
        Event::ProjectSaveFailed { id: failed, error } if failed == id && error.contains("projects.toml")
    ));
    // Data sources don't depend on the projects file.
    save(&mut engine, &sqlite_data_source(dir.path().join("app.db")));
    engine.shut_down();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("projects.toml")).unwrap(),
        broken
    );
}

#[test]
fn saves_mcp_settings_defaults_and_an_mcp_logins_password() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data_sources.toml");
    let secrets = Arc::new(MemorySecrets::default());
    let mut engine = TestEngine::start(&file, Arc::clone(&secrets));
    engine.expect_started(vec![]);
    let project = create(&mut engine, "Analytics");
    let overrides = McpOverrides {
        write: Some(true),
        schemas: Some(SchemaScope::Only(vec!["reports".to_owned()])),
        ..McpOverrides::default()
    };

    engine.send(Command::SaveProjectMcp {
        id: project.id,
        mcp_enabled: true,
        overrides: overrides.clone(),
    });

    let saved = expect_project_saved(&mut engine);
    assert!(saved.mcp_enabled);
    assert_eq!(saved.mcp, overrides);
    assert_eq!(
        projects_file(dir.path()).load().unwrap().projects,
        vec![saved.clone()]
    );
    assert_eq!(saved.slug, project.slug, "the slug never changes");

    let defaults = McpPermissions {
        schema: true,
        ..McpPermissions::default()
    };
    engine.send(Command::SaveMcpDefaults(defaults.clone()));
    engine.expect(Event::McpDefaultsSaved(defaults.clone()));
    let file = projects_file(dir.path()).load().unwrap();
    assert_eq!(file.mcp_defaults, defaults);
    assert_eq!(file.projects, vec![saved], "the projects are untouched");

    let id = DataSourceId::generate();
    engine.send(Command::SaveMcpPassword {
        id,
        password: Some(Password::new("for the assistant")),
    });
    engine.send(Command::SaveMcpPassword { id, password: None });
    engine.send(Command::SaveMcpPassword {
        id,
        password: Some(Password::new("again")),
    });
    engine.engine.shutdown().unwrap();
    assert_eq!(secrets.mcp(id), Some(Password::new("again")));
    assert_eq!(
        secrets.database(id),
        None,
        "the data source's own entry is untouched"
    );
}
