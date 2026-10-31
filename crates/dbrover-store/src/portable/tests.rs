use std::path::PathBuf;

use dbrover_core::{
    ClickHouseConfig, ConnectionConfig, DataSourceId, DuckDbConfig, DuckDbDatabase, MysqlConfig,
    PasswordCommand, Port, PostgresConfig, ProjectId, ServerConfig, SourceColor, SqlServerConfig,
    SqliteConfig, SshConfig, SshLogin, TlsMode, Variables,
};

use super::*;
use crate::DataSourceFile;
use crate::projects::{McpOverrides, ProjectSlug};

/// No variables set, and no home folder unless the test gives one.
struct NoVariables;

impl Variables for NoVariables {
    fn var(&self, _: &str) -> Option<String> {
        None
    }

    fn home(&self) -> Option<PathBuf> {
        None
    }
}

fn project(name: &str, slug: &str) -> Project {
    Project::new(
        ProjectId::generate(),
        name.to_owned(),
        ProjectSlug::try_from(slug.to_owned()).unwrap(),
    )
}

fn server(host: &str, port: u16, tls: TlsMode) -> ServerConfig {
    ServerConfig {
        host: host.to_owned(),
        port: port.into(),
        database: "app".to_owned(),
        user: "juan".to_owned(),
        tls,
        ssh: None,
        password_command: None,
    }
}

fn data_source(name: &str, project: Option<ProjectId>, config: ConnectionConfig) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: name.to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project,
        mcp_login: None,
        config,
    }
}

/// One data source of every kind, with every optional setting used somewhere.
fn every_kind(home: &Path, beside: &Path, project: ProjectId) -> Vec<DataSource> {
    vec![
        DataSource {
            color: Some(SourceColor::Red),
            mcp_login: Some(dbrover_core::McpLogin {
                user: "assistant".to_owned(),
            }),
            ..data_source(
                "production",
                Some(project),
                ConnectionConfig::Postgres(PostgresConfig {
                    server: server(
                        "db.internal",
                        5432,
                        TlsMode::VerifyFull {
                            ca_file: Some(home.join("certs/internal-ca.pem")),
                        },
                    ),
                }),
            )
        },
        data_source(
            "shop",
            Some(project),
            ConnectionConfig::Mysql(MysqlConfig {
                server: ServerConfig {
                    port: Port::Placeholder("${SHOP_PORT}".to_owned()),
                    ssh: Some(SshConfig {
                        host: "jump.internal".to_owned(),
                        port: 22.into(),
                        user: "juan".to_owned(),
                        login: SshLogin::Agent,
                    }),
                    ..server("${SHOP_HOST}", 0, TlsMode::Require)
                },
            }),
        ),
        data_source(
            "sales",
            None,
            ConnectionConfig::SqlServer(SqlServerConfig {
                server: ServerConfig {
                    ssh: Some(SshConfig {
                        host: "bastion.internal".to_owned(),
                        port: Port::Placeholder("${BASTION_PORT}".to_owned()),
                        user: "deploy".to_owned(),
                        login: SshLogin::KeyFile {
                            key_file: home.join(".ssh/id_ed25519"),
                        },
                    }),
                    password_command: Some(
                        PasswordCommand::parse(&[
                            "vault".to_owned(),
                            "read".to_owned(),
                            "-field=password".to_owned(),
                            "secret/{host}/{user}".to_owned(),
                        ])
                        .unwrap(),
                    ),
                    ..server("sql.internal", 1433, TlsMode::Disable)
                },
            }),
        ),
        data_source(
            "chinook",
            None,
            ConnectionConfig::Sqlite(SqliteConfig {
                path: beside.join("data/chinook.db"),
                read_only: true,
            }),
        ),
        data_source(
            "scratch",
            None,
            ConnectionConfig::Sqlite(SqliteConfig {
                path: "/srv/scratch.db".into(),
                read_only: false,
            }),
        ),
        data_source(
            "events",
            None,
            ConnectionConfig::ClickHouse(ClickHouseConfig {
                server: ServerConfig {
                    ssh: Some(SshConfig {
                        host: "ch-gateway.internal".to_owned(),
                        port: 2222.into(),
                        user: "analyst".to_owned(),
                        login: SshLogin::Password,
                    }),
                    ..server("ch.internal", 8443, TlsMode::Require)
                },
                read_only: false,
            }),
        ),
        data_source(
            "lake",
            Some(project),
            ConnectionConfig::DuckDb(DuckDbConfig {
                database: DuckDbDatabase::File {
                    path: beside.join("data/lake.duckdb"),
                    read_only: true,
                },
            }),
        ),
        data_source(
            "scratchpad",
            None,
            ConnectionConfig::DuckDb(DuckDbConfig {
                database: DuckDbDatabase::InMemory,
            }),
        ),
    ]
}

#[test]
fn writes_portable_paths_and_reads_them_back_for_another_home_and_folder() {
    let dir = tempfile::tempdir().unwrap();
    let exported_to = dir.path().join("team/dbrover.toml");
    let analytics = Project {
        mcp_enabled: true,
        mcp: McpOverrides {
            write: Some(true),
            ..McpOverrides::default()
        },
        ..project("Analytics", "analytics")
    };
    let analytics_id = analytics.id;
    let data_sources = every_kind(
        Path::new("/home/juan"),
        &dir.path().join("team"),
        analytics_id,
    );

    write_export(
        &exported_to,
        &data_sources,
        std::slice::from_ref(&analytics),
        Some(Path::new("/home/juan")),
    )
    .unwrap();
    let text = fs::read_to_string(&exported_to).unwrap();
    assert!(
        text.starts_with("# DbRover data sources, exported"),
        "{text}"
    );
    assert!(text.contains("format_version = 1"), "{text}");
    assert!(
        text.contains(r#"ca_file = "~/certs/internal-ca.pem""#),
        "{text}"
    );
    assert!(text.contains(r#"path = "data/chinook.db""#), "{text}");
    assert!(text.contains(r#"path = "data/lake.duckdb""#), "{text}");
    assert!(text.contains(r#"database = "in-memory""#), "{text}");
    assert!(text.contains(r#"path = "/srv/scratch.db""#), "{text}");
    assert!(text.contains(r#"host = "${SHOP_HOST}""#), "{text}");
    assert!(text.contains(r#"key_file = "~/.ssh/id_ed25519""#), "{text}");
    assert!(text.contains(r#"login = "agent""#), "{text}");
    assert!(text.contains(r#"login = "password""#), "{text}");
    assert!(
        text.contains(
            r#"password_command = ["vault", "read", "-field=password", "secret/{host}/{user}"]"#
        ),
        "{text}"
    );

    let moved_to = dir.path().join("downloads/dbrover.toml");
    fs::create_dir_all(moved_to.parent().unwrap()).unwrap();
    fs::copy(&exported_to, &moved_to).unwrap();
    let incoming = read_export(&moved_to, Some(Path::new("/Users/ana"))).unwrap();

    assert_eq!(incoming.projects, vec![analytics]);
    let expected: Vec<DataSource> = every_kind(
        Path::new("/Users/ana"),
        &dir.path().join("downloads"),
        analytics_id,
    )
    .into_iter()
    .zip(&data_sources)
    .map(|(expected, original)| DataSource {
        id: original.id,
        ..expected
    })
    .collect();
    assert_eq!(incoming.data_sources, expected);
}

#[test]
fn its_data_source_tables_are_in_the_data_sources_files_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("export.toml");
    let elsewhere = Path::new("/elsewhere");
    let data_sources = every_kind(elsewhere, elsewhere, ProjectId::generate());

    write_export(&path, &data_sources, &[], None).unwrap();

    assert_eq!(DataSourceFile::new(&path).load().unwrap(), data_sources);
}

#[test]
fn refuses_files_from_a_newer_dbrover_and_repeated_ids() {
    let id = DataSourceId::generate();
    let entry = format!(
        "[[data_source]]\nid = \"{id}\"\nname = \"a\"\ndriver = \"sqlite\"\npath = \"/a.db\"\n"
    );
    let cases = [
        (
            format!("format_version = 2\n{entry}"),
            "was written by a newer DbRover",
        ),
        (
            format!("format_version = 1\n{entry}{entry}"),
            "two data sources have the id",
        ),
        (
            "[[data_source]]\nname = 3\n".to_owned(),
            "is not a DbRover data sources export",
        ),
    ];
    for (text, expected) in cases {
        let error = parse(Path::new("/x/export.toml"), &text, None).unwrap_err();
        assert!(
            dbrover_core::describe_error(&error).contains(expected),
            "{error:?}"
        );
    }
}

/// The local state, a file built from it, and the preview of importing that file.
struct Machine {
    data_sources: Vec<DataSource>,
    projects: Projects,
}

impl Machine {
    fn preview(&self, incoming: Incoming) -> ImportPreview {
        ImportPreview::new(incoming, &self.data_sources, &self.projects, &NoVariables)
    }

    fn apply(&mut self, plan: &ImportPlan) {
        for project in &plan.projects {
            match self
                .projects
                .projects
                .iter_mut()
                .find(|p| p.id == project.id)
            {
                Some(existing) => *existing = project.clone(),
                None => self.projects.projects.push(project.clone()),
            }
        }
        for data_source in &plan.data_sources {
            match self
                .data_sources
                .iter_mut()
                .find(|ds| ds.id == data_source.id)
            {
                Some(existing) => *existing = data_source.clone(),
                None => self.data_sources.push(data_source.clone()),
            }
        }
    }
}

fn incoming(projects: Vec<Project>, data_sources: Vec<DataSource>) -> Incoming {
    Incoming {
        file: "/x/export.toml".into(),
        projects,
        data_sources,
    }
}

fn statuses(preview: &ImportPreview) -> Vec<&EntryStatus> {
    preview
        .projects
        .iter()
        .map(|entry| &entry.status)
        .chain(preview.data_sources.iter().map(|entry| &entry.status))
        .collect()
}

#[test]
fn imports_everything_new_then_finds_it_all_unchanged() {
    let analytics = project("Analytics", "analytics");
    let file = incoming(
        vec![analytics.clone()],
        every_kind(Path::new("/h"), Path::new("/b"), analytics.id),
    );
    let mut machine = Machine {
        data_sources: Vec::new(),
        projects: Projects::default(),
    };

    let first = machine.preview(file.clone());
    assert!(statuses(&first).iter().all(|s| **s == EntryStatus::New));
    let plan = first.plan(&first.default_choices());
    assert_eq!(plan.projects, file.projects);
    assert_eq!(plan.data_sources, file.data_sources);
    machine.apply(&plan);

    let again = machine.preview(file);
    assert!(
        statuses(&again)
            .iter()
            .all(|s| **s == EntryStatus::Unchanged)
    );
    assert!(again.plan(&again.default_choices()).is_empty());
    let import_all = ImportChoices {
        projects: vec![ImportChoice::Import; again.projects.len()],
        data_sources: vec![ImportChoice::Import; again.data_sources.len()],
    };
    assert!(again.plan(&import_all).is_empty());
}

#[test]
fn shows_what_differs_and_updates_or_copies_as_chosen() {
    let local = data_source(
        "shop",
        None,
        ConnectionConfig::Postgres(PostgresConfig {
            server: server("localhost", 5432, TlsMode::Prefer),
        }),
    );
    let changed = DataSource {
        color: Some(SourceColor::Green),
        config: ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                password_command: Some(
                    PasswordCommand::parse(&["printf".to_owned(), "a b".to_owned()]).unwrap(),
                ),
                ..server("db.internal", 6432, TlsMode::Prefer)
            },
        }),
        ..local.clone()
    };
    let machine = Machine {
        data_sources: vec![local.clone()],
        projects: Projects::default(),
    };

    let preview = machine.preview(incoming(vec![], vec![changed.clone()]));

    let difference = |setting: &'static str, current: &str, incoming: &str| Difference {
        setting,
        current: current.to_owned(),
        incoming: incoming.to_owned(),
    };
    assert_eq!(
        preview.data_sources[0].status,
        EntryStatus::Differs(vec![
            difference("Colour", "none", "Green"),
            difference("Host", "localhost", "db.internal"),
            difference("Port", "5432", "6432"),
            difference("Password", "from the keychain", r#"from ["printf", "a b"]"#),
        ])
    );
    let choose = |choice| ImportChoices {
        projects: vec![],
        data_sources: vec![choice],
    };
    assert_eq!(
        preview.plan(&choose(ImportChoice::Import)).data_sources,
        vec![changed.clone()]
    );
    assert!(preview.plan(&choose(ImportChoice::Skip)).is_empty());
    let copy = preview.plan(&choose(ImportChoice::Copy)).data_sources;
    let [copy] = copy.as_slice() else {
        panic!("expected one copy, got {copy:?}");
    };
    assert_ne!(copy.id, local.id);
    assert_eq!(copy.name, "shop (copy)");
    assert_eq!(copy.config, changed.config);
}

#[test]
fn matches_projects_by_id_then_by_slug_keeping_this_machines_id_and_slug() {
    let local = project("Shop", "shop");
    let same_slug = Project {
        mcp_enabled: true,
        ..project("Shop EU", "shop")
    };
    let member = data_source(
        "orders",
        Some(same_slug.id),
        ConnectionConfig::Sqlite(SqliteConfig {
            path: "/srv/orders.db".into(),
            read_only: false,
        }),
    );
    let machine = Machine {
        data_sources: Vec::new(),
        projects: Projects {
            projects: vec![local.clone()],
            ..Projects::default()
        },
    };

    let preview = machine.preview(incoming(vec![same_slug.clone()], vec![member.clone()]));

    assert_eq!(preview.projects[0].local, Some(local.clone()));
    assert!(matches!(
        preview.projects[0].status,
        EntryStatus::Differs(_)
    ));
    assert_eq!(preview.data_sources[0].incoming.project, Some(local.id));
    let plan = preview.plan(&preview.default_choices());
    assert_eq!(
        plan.projects,
        vec![Project {
            id: local.id,
            slug: local.slug.clone(),
            ..same_slug.clone()
        }]
    );
    assert_eq!(plan.data_sources[0].project, Some(local.id));

    let copied = preview.plan(&ImportChoices {
        projects: vec![ImportChoice::Copy],
        data_sources: vec![ImportChoice::Import],
    });
    let [copy] = copied.projects.as_slice() else {
        panic!("expected a copied project, got {copied:?}");
    };
    assert_eq!(copy.slug.as_str(), "shop-eu");
    assert_eq!(copied.data_sources[0].project, Some(copy.id));
}

#[test]
fn a_skipped_new_project_leaves_its_data_sources_in_no_project() {
    let new_project = project("New", "new");
    let member = data_source(
        "orders",
        Some(new_project.id),
        ConnectionConfig::Sqlite(SqliteConfig {
            path: "/srv/orders.db".into(),
            read_only: false,
        }),
    );
    let machine = Machine {
        data_sources: Vec::new(),
        projects: Projects::default(),
    };

    let preview = machine.preview(incoming(vec![new_project], vec![member]));
    let plan = preview.plan(&ImportChoices {
        projects: vec![ImportChoice::Skip],
        data_sources: vec![ImportChoice::Import],
    });

    assert!(plan.projects.is_empty());
    assert_eq!(plan.data_sources[0].project, None);
}

#[test]
fn flags_files_this_machine_does_not_have() {
    let dir = tempfile::tempdir().unwrap();
    let present = dir.path().join("present.db");
    fs::write(&present, "").unwrap();
    let sqlite = |path: PathBuf| {
        data_source(
            "file",
            None,
            ConnectionConfig::Sqlite(SqliteConfig {
                path,
                read_only: false,
            }),
        )
    };
    let with_ca = data_source(
        "tls",
        None,
        ConnectionConfig::Postgres(PostgresConfig {
            server: server(
                "db",
                5432,
                TlsMode::VerifyFull {
                    ca_file: Some(dir.path().join("missing-ca.pem")),
                },
            ),
        }),
    );
    let with_key = data_source(
        "ssh",
        None,
        ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                ssh: Some(SshConfig {
                    host: "jump".to_owned(),
                    port: 22.into(),
                    user: "juan".to_owned(),
                    login: SshLogin::KeyFile {
                        key_file: dir.path().join("missing_key"),
                    },
                }),
                ..server("db", 5432, TlsMode::Prefer)
            },
        }),
    );
    let machine = Machine {
        data_sources: Vec::new(),
        projects: Projects::default(),
    };

    let preview = machine.preview(incoming(
        vec![],
        vec![
            sqlite(present),
            sqlite(dir.path().join("missing.db")),
            sqlite("${NOT_SET}/shop.db".into()),
            with_ca,
            with_key,
        ],
    ));

    let problems: Vec<Vec<(&str, &str)>> = preview
        .data_sources
        .iter()
        .map(|entry| {
            entry
                .missing
                .iter()
                .map(|missing| (missing.setting, missing.problem.as_str()))
                .collect()
        })
        .collect();
    assert_eq!(
        problems,
        vec![
            vec![],
            vec![("File", "not found on this machine")],
            vec![("File", "${NOT_SET} is not set")],
            vec![("CA file", "not found on this machine")],
            vec![("SSH key file", "not found on this machine")],
        ]
    );
}

/// Command actions come only from the user's own `actions.toml` (H2): a shared export that
/// holds some, by mistake or not, imports its data sources and nothing else.
#[test]
fn an_export_carrying_actions_imports_none_of_them() {
    let id = DataSourceId::generate();
    let text = format!(
        "format_version = 1\n\
         [[data_source]]\nid = \"{id}\"\nname = \"a\"\ndriver = \"sqlite\"\npath = \"/a.db\"\n\
         [[action]]\nid = \"x\"\nname = \"X\"\napplies_to = [\"data_source\"]\n\
         command = [\"curl\", \"https://example.com\"]\n"
    );

    let incoming = parse(Path::new("/x/export.toml"), &text, None).unwrap();

    let Incoming {
        file: _,
        projects,
        data_sources,
    } = incoming;
    assert!(projects.is_empty());
    assert_eq!(
        data_sources
            .iter()
            .map(|source| source.id)
            .collect::<Vec<_>>(),
        [id]
    );
}
