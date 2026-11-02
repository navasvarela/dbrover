//! An explorer that pages (`docs/nosql.md`, N0a): a schema of thousands of tables loads a page
//! at a time, with the explorer's filter applied by the server; and a search of the whole
//! catalog on the server (C0g). Each test works in a database
//! of its own, dropped after it. Skipped unless `DBROVER_TEST_POSTGRES_URL` is set.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Instant;

use common::{ParsedUrl, TestEngine, postgres_test_server};
use dbrover_core::{
    CATALOG_PAGE_SIZE, CatalogMatches, CatalogPath, CatalogSearch, ChildrenPage, ChildrenQuery,
    Connection, ConnectionConfig, DataSource, DataSourceId, FolderKind, NodeKind, PostgresConfig,
    Route, SafetyLevel,
};
use dbrover_engine::{Command, Event, PasswordUpdate};

/// How many tables one transaction creates. PostgreSQL holds a lock per table created until
/// the transaction ends, and its lock table holds about 6,400 by default
/// (`max_locks_per_transaction` × `max_connections`), so 20,000 at once would run out.
const TABLES_PER_TRANSACTION: usize = 2_000;

#[test]
fn loads_a_schema_of_2500_tables_a_page_at_a_time_filtered_by_the_server() {
    if let Some(server) = postgres_test_server() {
        check_paging(server, 2_500);
    }
}

/// The plan's exit criterion at full size. Ignored by default: creating the tables takes
/// tens of seconds.
#[test]
#[ignore = "creates 20,000 tables; run with --ignored"]
fn loads_a_schema_of_20000_tables_a_page_at_a_time_filtered_by_the_server() {
    if let Some(server) = postgres_test_server() {
        check_paging(server, 20_000);
    }
}

fn check_paging(server: ParsedUrl, count: usize) {
    let scratch = Scratch::with_tables(server, count);
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: scratch.data_source.clone(),
        password: PasswordUpdate::Set(scratch.password.clone()),
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(scratch.data_source.clone()));
    let tables = CatalogPath::root()
        .child(NodeKind::Database, &scratch.name)
        .child(NodeKind::Schema, "public")
        .child(
            NodeKind::Folder(FolderKind::Tables),
            FolderKind::Tables.label(),
        );

    let started = Instant::now();
    let mut labels = Vec::new();
    let mut query = ChildrenQuery::first_page(None);
    let mut pages = 0;
    loop {
        let page = load(&mut engine, &scratch.data_source, &tables, &query);
        pages += 1;
        assert!(page.nodes.len() <= CATALOG_PAGE_SIZE);
        labels.extend(page.nodes.into_iter().map(|node| node.label));
        match page.next {
            Some(next) => query = query.after(next),
            None => break,
        }
    }
    println!(
        "{count} tables in {pages} pages, in {:?}",
        started.elapsed()
    );
    assert_eq!(labels, table_names(count));
    assert_eq!(pages, count.div_ceil(CATALOG_PAGE_SIZE));

    let filtered = load(
        &mut engine,
        &scratch.data_source,
        &tables,
        &ChildrenQuery::first_page(Some("T_012".to_owned())),
    );
    let expected: Vec<String> = table_names(count)
        .into_iter()
        .filter(|name| name.contains("t_012"))
        .collect();
    let found: Vec<String> = filtered.nodes.into_iter().map(|node| node.label).collect();
    assert_eq!(
        found, expected,
        "the filter ignores case and `_` is no wildcard"
    );
    assert_eq!(filtered.next, None);
    engine.shut_down();
}

/// C0g: a search of the whole database finds tables and views in schemas never expanded, on
/// the server, with their paths, and is cut at its limit.
#[test]
fn searches_every_schema_on_the_server() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let mut scratch = Scratch::with_tables(server, 2_500);
    scratch.run_in_database(
        "CREATE SCHEMA sales; CREATE TABLE sales.\"Orders\" (id int); \
         CREATE VIEW sales.open_orders AS SELECT 1 AS id",
    );
    let dir = tempfile::tempdir().unwrap();
    let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    engine.send(Command::SaveDataSource {
        data_source: scratch.data_source.clone(),
        password: PasswordUpdate::Set(scratch.password.clone()),
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(scratch.data_source.clone()));

    let orders = search(&mut engine, &scratch.data_source, "ORDERS");
    let sales = CatalogPath::root()
        .child(NodeKind::Database, &scratch.name)
        .child(NodeKind::Schema, "sales");
    let in_folder = |folder: FolderKind, kind, name: &str| {
        sales
            .child(NodeKind::Folder(folder), folder.label())
            .child(kind, name)
    };
    let paths: Vec<CatalogPath> = orders.nodes.iter().map(|node| node.path.clone()).collect();
    assert_eq!(
        paths,
        [
            in_folder(FolderKind::Tables, NodeKind::Table, "Orders"),
            in_folder(FolderKind::Views, NodeKind::View, "open_orders"),
        ]
    );
    assert_eq!(orders.nodes[0].detail.as_deref(), Some("sales"));
    assert!(!orders.more);

    // t_01200 to t_01299: exactly the limit.
    let hundred = search(&mut engine, &scratch.data_source, "T_012");
    assert_eq!((hundred.nodes.len(), hundred.more), (100, false));
    assert_eq!(hundred.nodes[0].label, "t_01200");
    // t_01000 to t_01999: cut at the limit.
    let many = search(&mut engine, &scratch.data_source, "t_01");
    assert_eq!((many.nodes.len(), many.more), (100, true));
    engine.shut_down();
}

fn search(engine: &mut TestEngine, data_source: &DataSource, text: &str) -> CatalogMatches {
    // Searching uses the open connection, as the explorer does only once connected.
    engine.send(Command::Connect(data_source.id));
    let search = CatalogSearch::new(text);
    engine.send(Command::SearchCatalog {
        id: data_source.id,
        search: search.clone(),
    });
    loop {
        match engine.next_event() {
            Event::CatalogSearched {
                search: answered,
                result,
                ..
            } if answered == search => {
                return result.expect("the search").expect("PostgreSQL searches");
            }
            Event::SessionChanged { .. } => {}
            other => panic!("expected CatalogSearched, got {other:?}"),
        }
    }
}

fn load(
    engine: &mut TestEngine,
    data_source: &DataSource,
    path: &CatalogPath,
    query: &ChildrenQuery,
) -> ChildrenPage {
    engine.send(Command::LoadChildren {
        id: data_source.id,
        path: path.clone(),
        query: query.clone(),
    });
    loop {
        match engine.next_event() {
            Event::ChildrenLoaded {
                path: loaded,
                query: answered,
                result,
                ..
            } if &loaded == path && &answered == query => return result.expect("the page"),
            Event::SessionChanged { .. } => {}
            other => panic!("expected ChildrenLoaded, got {other:?}"),
        }
    }
}

/// `t_00001`, `t_00002`, …: padded, so they sort as they are numbered.
fn table_names(count: usize) -> Vec<String> {
    (1..=count).map(|n| format!("t_{n:05}")).collect()
}

/// A database created for one test, holding `count` empty tables, and dropped after it.
struct Scratch {
    runtime: tokio::runtime::Runtime,
    admin: Option<Box<dyn Connection>>,
    name: String,
    data_source: DataSource,
    password: dbrover_core::Password,
}

impl Scratch {
    /// Runs `sql` in the scratch database, on a connection of its own.
    fn run_in_database(&mut self, sql: &str) {
        let config = match &self.data_source.config {
            ConnectionConfig::Postgres(config) => config.clone(),
            other => panic!("not PostgreSQL: {other:?}"),
        };
        let mut connection = self
            .runtime
            .block_on(dbrover_driver_postgres::connect(
                &config,
                Some(&self.password),
                Route::Direct,
            ))
            .unwrap();
        run(&self.runtime, &mut connection, sql);
        let _ = self.runtime.block_on(Box::new(connection).close());
    }

    fn with_tables(server: ParsedUrl, count: usize) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let name = format!(
            "dbrover_explorer_{}",
            DataSourceId::generate().to_string().replace('-', "_")
        );
        let connect = |database: &str| {
            let mut config = server.config.clone();
            database.clone_into(&mut config.server.database);
            runtime
                .block_on(dbrover_driver_postgres::connect(
                    &config,
                    Some(&server.password),
                    Route::Direct,
                ))
                .unwrap()
        };
        let mut admin: Box<dyn Connection> = Box::new(connect("postgres"));
        run(&runtime, admin.as_mut(), &format!("CREATE DATABASE {name}"));
        let mut tables = connect(&name);
        for first in (1..=count).step_by(TABLES_PER_TRANSACTION) {
            let last = (first + TABLES_PER_TRANSACTION - 1).min(count);
            let create = format!(
                "DO $$ BEGIN FOR i IN {first}..{last} LOOP \
                 EXECUTE format('CREATE TABLE t_%s (id int)', lpad(i::text, 5, '0')); \
                 END LOOP; END $$"
            );
            run(&runtime, &mut tables, &create);
        }
        let _ = runtime.block_on(Box::new(tables).close());
        let data_source = DataSource {
            id: DataSourceId::generate(),
            name: "explorer".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Postgres(PostgresConfig {
                server: dbrover_core::ServerConfig {
                    database: name.clone(),
                    ..server.config.server.clone()
                },
            }),
        };
        Self {
            runtime,
            admin: Some(admin),
            name,
            data_source,
            password: server.password,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(mut admin) = self.admin.take() {
            let drop = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name);
            run(&self.runtime, admin.as_mut(), &drop);
            let _ = self.runtime.block_on(admin.close());
        }
    }
}

fn run(runtime: &tokio::runtime::Runtime, connection: &mut dyn Connection, sql: &str) {
    runtime.block_on(async {
        connection
            .execute(sql)
            .await
            .unwrap()
            .finish()
            .await
            .unwrap();
    });
}
