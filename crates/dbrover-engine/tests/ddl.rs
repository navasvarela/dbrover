//! The object helpers' DDL: applied on the explorer's connection, all or none where the
//! engine's DDL is transactional, refused on read-only data sources, recorded in the history;
//! a table built from a spec described back as the spec says, and views and schemas created,
//! changed and dropped, on every engine with a test server; and PostgreSQL's roles, with
//! their passwords kept out of the history.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use common::{
    TestEngine, clickhouse_test_server, mysql_test_servers, postgres_test_server, sql_dialect,
    sqlite_data_source, sqlite_database, sqlserver_test_server,
};
use dbrover_core::ChildrenQuery;
use dbrover_core::{
    CatalogNode, CatalogPath, ConnectionConfig, DataSource, DataSourceId, DriverKind, DuckDbConfig,
    DuckDbDatabase, FolderKind, Identifier, Identity, IdentityGeneration, NodeKind, Password,
    PostgresConfig, RelationName, RoleAttribute, RoleDescription, SafetyLevel, TableDescription,
};
use dbrover_engine::{
    Command, ConsoleId, DatabaseFile, DdlFailure, Event, PasswordUpdate, RunId, RunOrigin,
    RunPurpose, ScanApproval, SessionState, TransactionMode,
};
use dbrover_sql::{
    Check, ColumnSpec, DdlOperation, Dialect, ExistingTable, ForeignKey, ObjectName, OwnedObjects,
    ParameterValues, PasswordChange, ReferentialAction, RoleSpec, SchemaCreation, SchemaSpec,
    ScramVerifier, TableAlteration, TableChange, TableSpec, UniqueKey, ViewKind, ViewSpec,
    ddl_statements, split_statements, view_query,
};
use dbrover_store::{HistoryOutcome, HistoryQuery};

fn start(dir: &std::path::Path) -> TestEngine {
    let mut engine = TestEngine::start(&dir.join("data_sources.toml"), Arc::default());
    engine.expect_started(vec![]);
    engine
}

fn save(engine: &mut TestEngine, data_source: &DataSource, password: Option<Password>) {
    engine.send(Command::SaveDataSource {
        data_source: data_source.clone(),
        password: password.map_or(PasswordUpdate::Keep, PasswordUpdate::Set),
        ssh_secret: PasswordUpdate::Keep,
    });
    engine.expect(Event::DataSourceSaved(data_source.clone()));
}

fn data_source(name: &str, config: ConnectionConfig) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: name.to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config,
    }
}

/// Applies `statements` and waits for the answer, skipping connection changes.
fn apply(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    statements: Vec<String>,
) -> Result<(), DdlFailure> {
    engine.send(Command::ApplyDdl {
        data_source,
        statements,
    });
    loop {
        match engine.next_event() {
            Event::DdlApplied {
                data_source: answered,
                result,
            } if answered == data_source => return result,
            Event::SessionChanged {
                state: SessionState::Failed(error),
                ..
            } => panic!("could not connect: {error}"),
            Event::SessionChanged { .. } => {}
            other => panic!("expected DdlApplied, got {other:?}"),
        }
    }
}

fn describe(engine: &mut TestEngine, data_source: DataSourceId, name: &str) -> TableDescription {
    let table = RelationName {
        schema: None,
        name: Identifier {
            name: name.to_owned(),
            quoted: true,
        },
    };
    engine.send(Command::DescribeTable {
        data_source,
        table: table.clone(),
    });
    loop {
        match engine.next_event() {
            Event::TableDescribed { result, .. } => {
                return result
                    .expect("describe")
                    .unwrap_or_else(|| panic!("{name} exists"));
            }
            Event::SessionChanged { .. } => {}
            other => panic!("expected TableDescribed, got {other:?}"),
        }
    }
}

fn sqlite_tables(database: &std::path::Path) -> Vec<String> {
    let connection = rusqlite::Connection::open(database).unwrap();
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn applies_ddl_all_or_none_where_ddl_is_transactional_and_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let database = sqlite_database(dir.path());
    let mut engine = start(dir.path());
    let app = sqlite_data_source(database.clone());
    save(&mut engine, &app, None);

    let failed = apply(
        &mut engine,
        app.id,
        vec![
            "CREATE TABLE kept (id INTEGER)".to_owned(),
            "CREATE TABLE t (id INTEGER)".to_owned(),
        ],
    );
    let failure = failed.expect_err("t exists already");
    assert_eq!(failure.statement, Some(1));
    assert_eq!(failure.applied, 0);
    assert!(failure.message.contains("already exists"), "{failure:?}");
    assert_eq!(
        sqlite_tables(&database),
        ["t"],
        "the first table was undone"
    );

    let applied = apply(
        &mut engine,
        app.id,
        vec![
            "CREATE TABLE a (id INTEGER)".to_owned(),
            "CREATE TABLE b (id INTEGER)".to_owned(),
        ],
    );
    assert_eq!(applied, Ok(()));
    assert_eq!(sqlite_tables(&database), ["a", "b", "t"]);

    engine.send(Command::SearchHistory(HistoryQuery::all(app.id)));
    let entries = match engine.next_event() {
        Event::HistoryFound { result, .. } => result.unwrap(),
        other => panic!("expected the history, got {other:?}"),
    };
    let recorded: Vec<(&str, HistoryOutcome)> = entries
        .iter()
        .map(|entry| (entry.sql.as_str(), entry.outcome))
        .collect();
    assert_eq!(
        recorded,
        [
            ("CREATE TABLE b (id INTEGER)", HistoryOutcome::Ok),
            ("CREATE TABLE a (id INTEGER)", HistoryOutcome::Ok),
            ("CREATE TABLE t (id INTEGER)", HistoryOutcome::Error),
            ("CREATE TABLE kept (id INTEGER)", HistoryOutcome::Ok),
        ],
        "newest first, the undone statement too, since it ran"
    );
    engine.shut_down();
}

#[test]
fn a_read_only_data_source_refuses_ddl_without_sending_it() {
    let dir = tempfile::tempdir().unwrap();
    let database = sqlite_database(dir.path());
    let mut engine = start(dir.path());
    let production = DataSource {
        name: "production".to_owned(),
        safety: SafetyLevel::ReadOnly,
        ..sqlite_data_source(database.clone())
    };
    save(&mut engine, &production, None);

    let refused = apply(&mut engine, production.id, vec!["DROP TABLE t".to_owned()]);
    assert_eq!(
        refused,
        Err(DdlFailure {
            statement: None,
            message: "Refused: \u{201c}production\u{201d} is read-only, so nothing in it can be \
                      created or changed."
                .to_owned(),
            applied: 0,
        })
    );
    assert_eq!(sqlite_tables(&database), ["t"]);
    engine.shut_down();
}

/// A parent table and a child pointing at it, with a key of two columns, a unique key, a
/// check and a default: the same spec on every engine, with its types and what it can't have
/// adjusted for the engine.
fn round_trip_tables(dialect: Dialect, suffix: &str) -> (TableSpec, TableSpec) {
    let (integer, text) = match dialect {
        Dialect::ClickHouse => ("Int32", "String"),
        _ => ("integer", "varchar(40)"),
    };
    let column = |name: &str, data_type: &str, nullable: bool, default: Option<&str>| ColumnSpec {
        name: name.to_owned(),
        data_type: data_type.to_owned(),
        nullable,
        default: default.map(str::to_owned),
        identity: None,
    };
    let names = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    let parent_name = format!("dbrover_parent_{suffix}");
    let parent = TableSpec {
        name: ObjectName::new(None, &parent_name),
        columns: vec![column("id", integer, false, None)],
        primary_key: names(&["id"]),
        unique_keys: vec![],
        foreign_keys: vec![],
        checks: vec![],
    };
    let capabilities = dialect.capabilities();
    let child = TableSpec {
        name: ObjectName::new(None, &format!("dbrover_child_{suffix}")),
        columns: vec![
            column("parent", integer, false, None),
            column("line", integer, false, None),
            column("code", text, false, None),
            column("quantity", integer, false, Some("1")),
            column("note", text, true, None),
        ],
        primary_key: names(&["parent", "line"]),
        unique_keys: if capabilities.unique_keys {
            vec![UniqueKey {
                name: None,
                columns: names(&["parent", "code"]),
            }]
        } else {
            vec![]
        },
        foreign_keys: if capabilities.foreign_keys {
            vec![ForeignKey {
                name: None,
                columns: names(&["parent"]),
                target: ObjectName::new(None, &parent_name),
                target_columns: names(&["id"]),
                on_delete: ReferentialAction::NoAction,
                on_update: ReferentialAction::NoAction,
            }]
        } else {
            vec![]
        },
        checks: vec![Check {
            name: Some(format!("quantity_positive_{suffix}")),
            expression: "quantity > 0".to_owned(),
        }],
    };
    (parent, child)
}

/// Creates the two tables in one `ApplyDdl`, checks the child is described as its spec says,
/// then drops both.
fn check_round_trip(engine: &mut TestEngine, data_source: &DataSource, password: Option<Password>) {
    save(engine, data_source, password);
    let dialect = sql_dialect(data_source.config.driver());
    let suffix = data_source.id.to_string().replace('-', "");
    let suffix = &suffix[..8];
    let (parent, child) = round_trip_tables(dialect, suffix);
    let create = ddl_statements(
        &[
            DdlOperation::CreateTable(parent.clone()),
            DdlOperation::CreateTable(child.clone()),
        ],
        dialect,
    )
    .unwrap();
    assert_eq!(apply(engine, data_source.id, create), Ok(()), "{dialect:?}");

    let described = describe(engine, data_source.id, &child.name.name);
    let columns: Vec<(&str, bool)> = described
        .columns
        .iter()
        .map(|column| (column.name.as_str(), column.nullable))
        .collect();
    let expected: Vec<(&str, bool)> = child
        .columns
        .iter()
        .map(|column| (column.name.as_str(), column.nullable))
        .collect();
    assert_eq!(columns, expected, "{dialect:?}");
    let quantity = &described.columns[3];
    assert!(
        quantity.default.as_deref().is_some_and(|d| d.contains('1')),
        "{dialect:?} default: {quantity:?}"
    );
    // A ClickHouse table's key is its sort order, which a description never reports.
    if dialect != Dialect::ClickHouse {
        assert_eq!(described.primary_key, child.primary_key, "{dialect:?}");
    }

    let drop = ddl_statements(
        &[
            DdlOperation::DropTable {
                table: child.name,
                cascade: false,
            },
            DdlOperation::DropTable {
                table: parent.name,
                cascade: false,
            },
        ],
        dialect,
    )
    .unwrap();
    assert_eq!(apply(engine, data_source.id, drop), Ok(()), "{dialect:?}");
}

#[test]
fn a_table_built_from_a_spec_is_described_as_the_spec_says_on_every_engine() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let sqlite = sqlite_data_source(sqlite_database(dir.path()));
    check_round_trip(&mut engine, &sqlite, None);
    let duckdb = data_source(
        "duckdb",
        ConnectionConfig::DuckDb(DuckDbConfig {
            database: DuckDbDatabase::InMemory,
        }),
    );
    check_round_trip(&mut engine, &duckdb, None);
    if let Some(server) = postgres_test_server() {
        let postgres = data_source("postgres", ConnectionConfig::Postgres(server.config));
        check_round_trip(&mut engine, &postgres, Some(server.password));
    }
    for (config, password) in mysql_test_servers() {
        let mysql = data_source("mysql", ConnectionConfig::Mysql(config));
        check_round_trip(&mut engine, &mysql, Some(password));
    }
    if let Some((config, password)) = sqlserver_test_server() {
        let sqlserver = data_source("sqlserver", ConnectionConfig::SqlServer(config));
        check_round_trip(&mut engine, &sqlserver, Some(password));
    }
    if let Some((config, password)) = clickhouse_test_server() {
        let clickhouse = data_source("clickhouse", ConnectionConfig::ClickHouse(config));
        check_round_trip(&mut engine, &clickhouse, Some(password));
    }
    engine.shut_down();
}

fn load_ddl(engine: &mut TestEngine, data_source: DataSourceId, path: CatalogPath) -> String {
    engine.send(Command::LoadDdl {
        data_source,
        path: path.clone(),
    });
    loop {
        match engine.next_event() {
            Event::DdlLoaded {
                path: loaded,
                result,
                ..
            } if loaded == path => return result.expect("the table's DDL"),
            Event::SessionChanged { .. } => {}
            other => panic!("expected DdlLoaded, got {other:?}"),
        }
    }
}

#[test]
fn copies_a_sqlite_tables_ddl_from_the_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let app = sqlite_data_source(sqlite_database(dir.path()));
    save(&mut engine, &app, None);
    let path = CatalogPath::root()
        .child(NodeKind::Schema, "main")
        .child(NodeKind::Folder(FolderKind::Tables), "tables")
        .child(NodeKind::Table, "t");
    let ddl = load_ddl(&mut engine, app.id, path);
    assert!(
        ddl.contains("CREATE TABLE t (id INTEGER PRIMARY KEY)"),
        "{ddl}"
    );
    engine.shut_down();
}

/// PostgreSQL and SQL Server keep no `CREATE TABLE`, so Copy DDL rebuilds it; running what it
/// copied after dropping the table must make the same table again.
fn check_rebuilt_ddl(engine: &mut TestEngine, data_source: &DataSource, password: Password) {
    save(engine, data_source, Some(password));
    let dialect = sql_dialect(data_source.config.driver());
    let schema = unique("dbrover_copy", data_source);
    let suffix = &schema["dbrover_copy_".len()..];
    let (mut parent, mut child) = round_trip_tables(dialect, suffix);
    parent.name.schema = Some(schema.clone());
    parent.columns[0].identity = Some(Identity {
        generation: IdentityGeneration::Always,
        start: 100,
        increment: 5,
    });
    child.name.schema = Some(schema.clone());
    child.foreign_keys[0].target = parent.name.clone();
    let operations = [
        DdlOperation::CreateSchema(SchemaSpec {
            name: schema.clone(),
            owner: None,
        }),
        DdlOperation::CreateTable(parent.clone()),
        DdlOperation::CreateTable(child.clone()),
    ];
    let created = apply(
        engine,
        data_source.id,
        ddl_statements(&operations, dialect).unwrap(),
    );
    assert_eq!(created, Ok(()), "{dialect:?}");

    let path = default_schema(data_source)
        .parent()
        .unwrap()
        .child(NodeKind::Schema, schema.clone())
        .child(NodeKind::Folder(FolderKind::Tables), "tables")
        .child(NodeKind::Table, child.name.name.clone());
    let parent_ddl = load_ddl(
        engine,
        data_source.id,
        path.parent()
            .unwrap()
            .child(NodeKind::Table, parent.name.name.clone()),
    );
    let identity = match dialect {
        Dialect::SqlServer => "id int IDENTITY(100, 5) NOT NULL",
        _ => "id integer GENERATED ALWAYS AS IDENTITY (START WITH 100 INCREMENT BY 5) NOT NULL",
    };
    assert!(parent_ddl.contains(identity), "{parent_ddl}");
    let ddl = load_ddl(engine, data_source.id, path);
    assert!(
        ddl.starts_with(&format!("CREATE TABLE {schema}.{} (", child.name.name)),
        "{ddl}"
    );
    for part in [
        "quantity ",
        "NOT NULL",
        "PRIMARY KEY (parent, line)",
        "FOREIGN KEY (parent)",
        "UNIQUE (parent, code)",
        "CHECK (",
    ] {
        assert!(ddl.contains(part), "{part} in {ddl}");
    }

    let drop = |table: &TableSpec| DdlOperation::DropTable {
        table: table.name.clone(),
        cascade: false,
    };
    let mut recreate = drop(&child).statements(dialect).unwrap();
    recreate.extend(
        split_statements(&ddl, dialect)
            .into_iter()
            .map(|range| ddl[range].to_owned()),
    );
    let recreated = apply(engine, data_source.id, recreate);
    let cleanup = [
        drop(&child),
        drop(&parent),
        DdlOperation::DropSchema {
            schema,
            cascade: false,
        },
    ];
    let cleaned = apply(
        engine,
        data_source.id,
        ddl_statements(&cleanup, dialect).unwrap(),
    );
    assert_eq!(recreated, Ok(()), "{dialect:?}: {ddl}");
    assert_eq!(cleaned, Ok(()), "{dialect:?}");
}

#[test]
fn rebuilds_a_tables_ddl_that_recreates_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    if let Some(server) = postgres_test_server() {
        let postgres = data_source("postgres", ConnectionConfig::Postgres(server.config));
        check_rebuilt_ddl(&mut engine, &postgres, server.password);
    }
    if let Some((config, password)) = sqlserver_test_server() {
        let sqlserver = data_source("sqlserver", ConnectionConfig::SqlServer(config));
        check_rebuilt_ddl(&mut engine, &sqlserver, password);
    }
    engine.shut_down();
}

/// Where DDL commits as it runs, a failure leaves the statements before it applied, and says
/// how many.
#[test]
fn mysql_keeps_the_statements_before_a_failure_and_says_so() {
    let Some((config, password)) = mysql_test_servers().into_iter().next() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let mysql = data_source("mysql", ConnectionConfig::Mysql(config));
    save(&mut engine, &mysql, Some(password));
    let table = format!(
        "dbrover_kept_{}",
        &mysql.id.to_string().replace('-', "")[..8]
    );
    let failed = apply(
        &mut engine,
        mysql.id,
        vec![
            format!("CREATE TABLE {table} (id integer)"),
            format!("CREATE TABLE {table} (id integer)"),
        ],
    );
    let cleaned = apply(&mut engine, mysql.id, vec![format!("DROP TABLE {table}")]);
    let failure = failed.expect_err("the second CREATE fails");
    assert_eq!((failure.statement, failure.applied), (Some(1), 1));
    assert_eq!(cleaned, Ok(()), "the first table was kept");
    engine.shut_down();
}

#[test]
fn the_history_keeps_ddl_apart_from_catalog_queries() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let app = sqlite_data_source(sqlite_database(dir.path()));
    save(&mut engine, &app, None);
    engine.send(Command::LoadChildren {
        id: app.id,
        path: CatalogPath::root(),
        query: ChildrenQuery::default(),
    });
    loop {
        if let Event::ChildrenLoaded { .. } = engine.next_event() {
            break;
        }
    }
    engine.send(Command::SearchHistory(HistoryQuery::all(app.id)));
    match engine.next_event() {
        Event::HistoryFound { result, .. } => assert_eq!(result.unwrap(), vec![]),
        other => panic!("expected the history, got {other:?}"),
    }
    engine.shut_down();
}

/// Every data source the round trips run on: SQLite and an in-memory DuckDB, and each test
/// server that is set, with its password.
fn every_engine() -> Vec<(DataSource, Option<Password>)> {
    let mut sources = vec![(
        data_source(
            "duckdb",
            ConnectionConfig::DuckDb(DuckDbConfig {
                database: DuckDbDatabase::InMemory,
            }),
        ),
        None,
    )];
    if let Some(server) = postgres_test_server() {
        sources.push((
            data_source("postgres", ConnectionConfig::Postgres(server.config)),
            Some(server.password),
        ));
    }
    for (config, password) in mysql_test_servers() {
        sources.push((
            data_source("mysql", ConnectionConfig::Mysql(config)),
            Some(password),
        ));
    }
    if let Some((config, password)) = sqlserver_test_server() {
        sources.push((
            data_source("sqlserver", ConnectionConfig::SqlServer(config)),
            Some(password),
        ));
    }
    if let Some((config, password)) = clickhouse_test_server() {
        sources.push((
            data_source("clickhouse", ConnectionConfig::ClickHouse(config)),
            Some(password),
        ));
    }
    sources
}

/// A name no other test run uses, from the data source's id.
fn unique(prefix: &str, data_source: &DataSource) -> String {
    let id = data_source.id.to_string().replace('-', "");
    format!("{prefix}_{}", &id[..8])
}

/// Where unqualified names go: the connection's default schema, under its database where
/// the tree lists databases.
fn default_schema(data_source: &DataSource) -> CatalogPath {
    let root = match data_source.config.database() {
        Some(database) => CatalogPath::root().child(NodeKind::Database, database),
        None => CatalogPath::root(),
    };
    root.child(NodeKind::Schema, data_source.config.default_schema())
}

fn view_path(data_source: &DataSource, kind: ViewKind, name: &str) -> CatalogPath {
    let (folder, node) = match kind {
        ViewKind::View => (FolderKind::Views, NodeKind::View),
        ViewKind::Materialized => (FolderKind::MaterializedViews, NodeKind::MaterializedView),
    };
    default_schema(data_source)
        .child(NodeKind::Folder(folder), folder.label())
        .child(node, name)
}

fn load_definition(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    path: CatalogPath,
) -> String {
    engine.send(Command::LoadDefinition {
        id: data_source,
        path: path.clone(),
    });
    loop {
        match engine.next_event() {
            Event::DefinitionLoaded {
                path: loaded,
                result,
                ..
            } if loaded == path => return result.expect("the definition"),
            Event::SessionChanged { .. } => {}
            other => panic!("expected DefinitionLoaded, got {other:?}"),
        }
    }
}

fn load_children(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    path: CatalogPath,
) -> Vec<CatalogNode> {
    engine.send(Command::LoadChildren {
        id: data_source,
        path: path.clone(),
        query: ChildrenQuery::default(),
    });
    loop {
        match engine.next_event() {
            Event::ChildrenLoaded {
                path: loaded,
                result,
                ..
            } if loaded == path => return result.expect("the children").nodes,
            Event::SessionChanged { .. } => {}
            other => panic!("expected ChildrenLoaded, got {other:?}"),
        }
    }
}

/// Applies one operation, built for the data source's dialect.
fn apply_operation(
    engine: &mut TestEngine,
    data_source: &DataSource,
    operation: DdlOperation,
) -> Result<(), DdlFailure> {
    let dialect = sql_dialect(data_source.config.driver());
    let statements = operation.statements(dialect).expect("the operation's SQL");
    apply(engine, data_source.id, statements)
}

/// The query the catalog shows for the view at `path`, as Edit view starts from it.
fn shown_query(engine: &mut TestEngine, data_source: &DataSource, path: CatalogPath) -> String {
    let dialect = sql_dialect(data_source.config.driver());
    let definition = load_definition(engine, data_source.id, path);
    view_query(&definition, dialect)
        .unwrap_or_else(|| panic!("{dialect:?}: no query in {definition:?}"))
}

/// Creates a view, edits its query starting from the one its definition shows, copies its
/// DDL, renames it where the engine can, and drops it.
fn check_view_round_trip(
    engine: &mut TestEngine,
    data_source: &DataSource,
    password: Option<Password>,
) {
    save(engine, data_source, password);
    let dialect = sql_dialect(data_source.config.driver());
    let name = unique("dbrover_view", data_source);
    let view = ViewSpec {
        name: ObjectName::new(None, &name),
        kind: ViewKind::View,
        query: "SELECT 1 AS one".to_owned(),
        with_data: true,
    };
    let path = view_path(data_source, ViewKind::View, &name);
    let created = apply_operation(engine, data_source, DdlOperation::CreateView(view.clone()));
    assert_eq!(created, Ok(()), "{dialect:?}");
    let shown = shown_query(engine, data_source, path.clone());
    assert!(shown.contains('1'), "{dialect:?}: {shown}");

    // The same column with another value: PostgreSQL's `CREATE OR REPLACE VIEW` refuses to
    // rename or drop a view's columns.
    let edited = ViewSpec {
        query: shown.replace('1', "2"),
        ..view.clone()
    };
    let replaced = apply_operation(engine, data_source, DdlOperation::ReplaceView(edited));
    assert_eq!(replaced, Ok(()), "{dialect:?}");
    let shown = shown_query(engine, data_source, path.clone());
    assert!(shown.contains('2'), "{dialect:?}: {shown}");
    let copied = load_ddl(engine, data_source.id, path);
    assert!(copied.contains(&name), "{dialect:?}: {copied}");

    let mut dropped = view.name.clone();
    if dialect.capabilities().renames_views {
        let new_name = format!("{name}_renamed");
        let renamed = apply_operation(
            engine,
            data_source,
            DdlOperation::RenameView {
                view: view.name.clone(),
                kind: ViewKind::View,
                new_name: new_name.clone(),
            },
        );
        assert_eq!(renamed, Ok(()), "{dialect:?}");
        dropped.name = new_name;
    }
    let views = load_children(
        engine,
        data_source.id,
        path_parent(&view_path(data_source, ViewKind::View, &dropped.name)),
    );
    assert!(
        views.iter().any(|node| node.label == dropped.name),
        "{dialect:?} lists {}",
        dropped.name
    );
    let drop = DdlOperation::DropView {
        view: dropped,
        kind: ViewKind::View,
        cascade: false,
    };
    assert_eq!(
        apply_operation(engine, data_source, drop),
        Ok(()),
        "{dialect:?}"
    );
}

fn path_parent(path: &CatalogPath) -> CatalogPath {
    path.parent().expect("a view has a folder")
}

#[test]
fn creates_edits_renames_and_drops_a_view_on_every_engine() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let sqlite = sqlite_data_source(sqlite_database(dir.path()));
    check_view_round_trip(&mut engine, &sqlite, None);
    for (data_source, password) in every_engine() {
        check_view_round_trip(&mut engine, &data_source, password);
    }
    engine.shut_down();
}

#[test]
fn creates_refreshes_edits_and_drops_a_postgres_materialized_view() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let postgres = data_source("postgres", ConnectionConfig::Postgres(server.config));
    save(&mut engine, &postgres, Some(server.password));
    let name = unique("dbrover_matview", &postgres);
    let view = ViewSpec {
        name: ObjectName::new(None, &name),
        kind: ViewKind::Materialized,
        query: "SELECT 1 AS one".to_owned(),
        with_data: false,
    };
    let path = view_path(&postgres, ViewKind::Materialized, &name);
    let steps = [
        DdlOperation::CreateView(view.clone()),
        DdlOperation::RefreshMaterializedView {
            view: view.name.clone(),
            concurrently: false,
        },
        DdlOperation::ReplaceView(ViewSpec {
            query: "SELECT 2 AS two".to_owned(),
            with_data: true,
            ..view.clone()
        }),
    ];
    for step in steps {
        assert_eq!(
            apply_operation(&mut engine, &postgres, step.clone()),
            Ok(()),
            "{step:?}"
        );
    }
    let copied = load_ddl(&mut engine, postgres.id, path);
    assert!(
        copied.starts_with(&format!("CREATE MATERIALIZED VIEW public.{name} AS")),
        "{copied}"
    );
    assert!(copied.contains("2 AS two"), "{copied}");

    let renamed = format!("{name}_renamed");
    let steps = [
        DdlOperation::RenameView {
            view: view.name.clone(),
            kind: ViewKind::Materialized,
            new_name: renamed.clone(),
        },
        DdlOperation::DropView {
            view: ObjectName::new(None, &renamed),
            kind: ViewKind::Materialized,
            cascade: true,
        },
    ];
    for step in steps {
        assert_eq!(
            apply_operation(&mut engine, &postgres, step.clone()),
            Ok(()),
            "{step:?}"
        );
    }
    engine.shut_down();
}

/// The node schemas are listed under: the data source's database, or the data source itself
/// where its schemas are databases.
fn schemas_parent(data_source: &DataSource) -> CatalogPath {
    default_schema(data_source)
        .parent()
        .expect("a schema has a parent")
}

/// Creates a schema and a table in it, sees the schema listed, then drops it with what it
/// holds: with `CASCADE` where the engine has it, as `DROP DATABASE` always does, and
/// otherwise by dropping the table first, after the schema's drop was refused.
fn check_schema_round_trip(
    engine: &mut TestEngine,
    data_source: &DataSource,
    password: Option<Password>,
) {
    save(engine, data_source, password);
    let dialect = sql_dialect(data_source.config.driver());
    let capabilities = dialect.capabilities();
    let schema = unique("dbrover_schema", data_source);
    let table = TableSpec {
        name: ObjectName::new(Some(&schema), "items"),
        columns: vec![ColumnSpec {
            name: "id".to_owned(),
            data_type: capabilities.common_types[0].to_owned(),
            nullable: false,
            default: None,
            identity: None,
        }],
        primary_key: vec!["id".to_owned()],
        unique_keys: vec![],
        foreign_keys: vec![],
        checks: vec![],
    };
    let created = apply(
        engine,
        data_source.id,
        ddl_statements(
            &[
                DdlOperation::CreateSchema(SchemaSpec {
                    name: schema.clone(),
                    owner: None,
                }),
                DdlOperation::CreateTable(table.clone()),
            ],
            dialect,
        )
        .unwrap(),
    );
    assert_eq!(created, Ok(()), "{dialect:?}");
    let listed = |engine: &mut TestEngine| {
        load_children(engine, data_source.id, schemas_parent(data_source))
            .iter()
            .any(|node| node.label == schema)
    };
    assert!(listed(engine), "{dialect:?} lists {schema}");

    let drop = |cascade| DdlOperation::DropSchema {
        schema: schema.clone(),
        cascade,
    };
    let empties_itself = capabilities.drop_schema_cascade
        || capabilities.schema_creation == SchemaCreation::CreateDatabase;
    if !empties_itself {
        assert!(
            apply_operation(engine, data_source, drop(false)).is_err(),
            "{dialect:?} refuses to drop a schema holding a table"
        );
        let dropped = apply_operation(
            engine,
            data_source,
            DdlOperation::DropTable {
                table: table.name,
                cascade: false,
            },
        );
        assert_eq!(dropped, Ok(()), "{dialect:?}");
    }
    let dropped = apply_operation(engine, data_source, drop(capabilities.drop_schema_cascade));
    assert_eq!(dropped, Ok(()), "{dialect:?}");
    assert!(!listed(engine), "{dialect:?} no longer lists {schema}");
}

/// MySQL's and MariaDB's test user may only use its own database, so it can't create
/// another; their `CREATE DATABASE` is covered by the builder's golden tests.
#[test]
fn creates_lists_and_drops_a_schema_with_what_it_holds() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    for (data_source, password) in every_engine() {
        if !matches!(data_source.config, ConnectionConfig::Mysql(_)) {
            check_schema_round_trip(&mut engine, &data_source, password);
        }
    }
    engine.shut_down();
}

fn describe_role(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    path: &CatalogPath,
) -> Result<RoleDescription, String> {
    engine.send(Command::DescribeRole {
        data_source,
        path: path.clone(),
    });
    loop {
        match engine.next_event() {
            Event::RoleDescribed {
                path: described,
                result,
                ..
            } if described == *path => return result,
            Event::SessionChanged { .. } => {}
            other => panic!("expected RoleDescribed, got {other:?}"),
        }
    }
}

/// Logs in to the test server as `user`, as a new data source would.
fn log_in(engine: &mut TestEngine, server: &PostgresConfig, user: &str, password: &str) -> bool {
    let mut config = server.clone();
    user.clone_into(&mut config.server.user);
    let login = data_source(user, ConnectionConfig::Postgres(config));
    engine.send(Command::TestConnection {
        data_source: login.clone(),
        password: PasswordUpdate::Set(Password::new(password)),
        ssh_secret: PasswordUpdate::Keep,
    });
    loop {
        match engine.next_event() {
            Event::ConnectionTested { id, result } if id == login.id => return result.is_ok(),
            Event::SessionChanged { .. } => {}
            other => panic!("expected ConnectionTested, got {other:?}"),
        }
    }
}

/// E3's exit, below the dialogs: a login user with a password and a membership is created,
/// is copied as DDL, logs in, is changed, renamed and dropped with what it owns given away; and neither its
/// password nor the verifier sent for it reaches the history.
#[test]
fn creates_logs_in_as_changes_and_drops_a_postgres_role() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let postgres = data_source(
        "postgres",
        ConnectionConfig::Postgres(server.config.clone()),
    );
    save(&mut engine, &postgres, Some(server.password));
    let owner = server.config.server.user.clone();
    let group = unique("dbrover_e3_readers", &postgres);
    let user = unique("dbrover_e3_user", &postgres);
    let renamed = format!("{user}_renamed");
    let table = unique("dbrover_e3_owned", &postgres);
    let password = format!("pw-{}", unique("secret", &postgres));
    let verifier = ScramVerifier::hash(&password);
    let new_user = RoleSpec {
        attributes: [RoleAttribute::Login, RoleAttribute::Inherit].into(),
        connection_limit: Some(3),
        valid_until: None,
        member_of: [group.clone()].into(),
    };
    let steps = [
        DdlOperation::CreateRole {
            name: group.clone(),
            spec: RoleSpec {
                attributes: [RoleAttribute::Inherit].into(),
                connection_limit: None,
                valid_until: None,
                member_of: BTreeSet::new(),
            },
            password: None,
        },
        DdlOperation::CreateRole {
            name: user.clone(),
            spec: new_user.clone(),
            password: Some(verifier.clone()),
        },
    ];
    for step in steps {
        assert_eq!(apply_operation(&mut engine, &postgres, step), Ok(()));
    }
    let owned = vec![
        format!("CREATE TABLE {table} (id integer)"),
        format!("ALTER TABLE {table} OWNER TO {user}"),
    ];
    assert_eq!(apply(&mut engine, postgres.id, owned), Ok(()));

    let roles = CatalogPath::root().child(NodeKind::Folder(FolderKind::Roles), "roles");
    let listed = load_children(&mut engine, postgres.id, roles.clone());
    let node = listed
        .iter()
        .find(|node| node.label == user)
        .expect("the new user in the roles folder");
    assert_eq!(
        node.detail.as_deref(),
        Some(format!("login, member of {group}").as_str())
    );
    let path = roles.child(NodeKind::Role, &user);
    let described = describe_role(&mut engine, postgres.id, &path).unwrap();
    assert_eq!(described.attributes, new_user.attributes);
    assert_eq!(described.connection_limit, Some(3));
    assert_eq!(described.member_of, new_user.member_of);
    assert_eq!(described.owned_objects, 1, "{described:?}");
    let group_path = roles.child(NodeKind::Role, &group);
    let described_group = describe_role(&mut engine, postgres.id, &group_path).unwrap();
    assert_eq!(described_group.members, [user.clone()].into());

    assert_eq!(
        load_ddl(&mut engine, postgres.id, path.clone()),
        format!("CREATE ROLE {user} WITH LOGIN CONNECTION LIMIT 3 IN ROLE {group};\n"),
        "Copy DDL leaves the password out"
    );

    assert!(log_in(&mut engine, &server.config, &user, &password));
    assert!(!log_in(
        &mut engine,
        &server.config,
        &user,
        "not the password"
    ));

    let changed = RoleSpec {
        attributes: [RoleAttribute::CreateDb].into(),
        connection_limit: None,
        valid_until: Some("2100-01-01".to_owned()),
        member_of: BTreeSet::new(),
    };
    let steps = [
        DdlOperation::AlterRole {
            name: user.clone(),
            before: new_user.clone(),
            after: changed.clone(),
            password: PasswordChange::Remove,
        },
        DdlOperation::RenameRole {
            name: user.clone(),
            new_name: renamed.clone(),
        },
    ];
    for step in steps {
        assert_eq!(apply_operation(&mut engine, &postgres, step), Ok(()));
    }
    let renamed_path = roles.child(NodeKind::Role, &renamed);
    let described = describe_role(&mut engine, postgres.id, &renamed_path).unwrap();
    assert_eq!(described.attributes, changed.attributes);
    assert_eq!(described.connection_limit, None);
    assert!(
        described
            .valid_until
            .as_deref()
            .is_some_and(|until| until.starts_with("2100-01-01")),
        "{described:?}"
    );
    assert!(described.member_of.is_empty());
    assert!(describe_role(&mut engine, postgres.id, &path).is_err());
    let unexpiring = DdlOperation::AlterRole {
        name: renamed.clone(),
        before: changed.clone(),
        after: RoleSpec {
            valid_until: None,
            ..changed.clone()
        },
        password: PasswordChange::Keep,
    };
    assert_eq!(apply_operation(&mut engine, &postgres, unexpiring), Ok(()));
    let described = describe_role(&mut engine, postgres.id, &renamed_path).unwrap();
    assert_eq!(described.valid_until, None, "'infinity' reads as never");

    let refused = apply_operation(
        &mut engine,
        &postgres,
        DdlOperation::DropRole {
            name: renamed.clone(),
            owned: OwnedObjects::Keep,
        },
    );
    assert!(refused.is_err(), "it still owns the table");
    let steps = [
        DdlOperation::DropRole {
            name: renamed.clone(),
            owned: OwnedObjects::ReassignTo(owner),
        },
        DdlOperation::DropRole {
            name: group.clone(),
            owned: OwnedObjects::Drop,
        },
    ];
    for step in steps {
        assert_eq!(apply_operation(&mut engine, &postgres, step), Ok(()));
    }
    assert!(describe_role(&mut engine, postgres.id, &renamed_path).is_err());
    assert_eq!(
        apply(
            &mut engine,
            postgres.id,
            vec![format!("DROP TABLE {table}")]
        ),
        Ok(()),
        "the table was given to the owner, not dropped"
    );

    engine.send(Command::SearchHistory(HistoryQuery::all(postgres.id)));
    let entries = match engine.next_event() {
        Event::HistoryFound { result, .. } => result.unwrap(),
        other => panic!("expected the history, got {other:?}"),
    };
    let created = format!(
        "CREATE ROLE {user} WITH LOGIN CONNECTION LIMIT 3 IN ROLE {group} PASSWORD '<redacted>'"
    );
    assert!(
        entries.iter().any(|entry| entry.sql == created),
        "{entries:#?}"
    );
    for entry in &entries {
        assert!(!entry.sql.contains(&password), "{}", entry.sql);
        assert!(!entry.sql.contains(verifier.as_str()), "{}", entry.sql);
        assert!(!entry.sql.contains("SCRAM-SHA-256"), "{}", entry.sql);
    }
    engine.shut_down();
}

/// Runs one query on a console of its own and returns its rows.
fn query(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    sql: &str,
) -> Vec<Vec<Option<String>>> {
    static RUNS: AtomicU64 = AtomicU64::new(1);
    let run = RunId(RUNS.fetch_add(1, Ordering::Relaxed));
    let console = ConsoleId(run.0);
    engine.send(Command::Execute {
        console,
        data_source,
        run,
        statements: vec![sql.to_owned()],
        parameters: ParameterValues::default(),
        transactions: TransactionMode::Auto,
        purpose: RunPurpose::Run,
        origin: RunOrigin::Console,
        scans: ScanApproval::Ask,
    });
    let mut rows = Vec::new();
    loop {
        match engine.next_event() {
            Event::RowsFetched {
                run: fetched,
                rows: page,
                ..
            } if fetched == run => rows.extend(page),
            Event::StatementFinished {
                run: finished,
                result: Err(failure),
                ..
            } if finished == run => panic!("{sql}: {failure:?}"),
            Event::RunFinished { run: finished, .. } if finished == run => break,
            _ => {}
        }
    }
    engine.send(Command::CloseConsole(console));
    loop {
        if let Event::ConsoleSessionChanged {
            console: closed,
            state: SessionState::Disconnected,
        } = engine.next_event()
            && closed == console
        {
            return rows;
        }
    }
}

fn load_table(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    path: &CatalogPath,
) -> ExistingTable {
    engine.send(Command::LoadTable {
        data_source,
        path: path.clone(),
    });
    loop {
        match engine.next_event() {
            Event::TableLoaded {
                path: loaded,
                result,
                ..
            } if &loaded == path => return result.expect("the table"),
            Event::SessionChanged { .. } => {}
            other => panic!("expected TableLoaded, got {other:?}"),
        }
    }
}

/// The table a Modify table round trip starts from, with two rows, and the table its foreign
/// key may point at.
fn orders_to_modify(dialect: Dialect, suffix: &str) -> (TableSpec, TableSpec) {
    let (integer, text) = match dialect {
        Dialect::ClickHouse => ("Int32", "String"),
        _ => ("integer", "varchar(20)"),
    };
    let column = |name: &str, data_type: &str, nullable: bool, default: Option<&str>| ColumnSpec {
        name: name.to_owned(),
        data_type: data_type.to_owned(),
        nullable,
        default: default.map(str::to_owned),
        identity: None,
    };
    let customers = TableSpec {
        name: ObjectName::new(None, &format!("dbrover_customers_{suffix}")),
        columns: vec![column("id", integer, false, None)],
        primary_key: vec!["id".to_owned()],
        unique_keys: vec![],
        foreign_keys: vec![],
        checks: vec![],
    };
    let orders = TableSpec {
        name: ObjectName::new(None, &format!("dbrover_orders_{suffix}")),
        columns: vec![
            column("id", integer, false, None),
            column("status", text, false, Some("'new'")),
            column("note", text, true, None),
            column("legacy", integer, true, None),
            column("customer", integer, true, None),
        ],
        primary_key: vec!["id".to_owned()],
        unique_keys: vec![],
        foreign_keys: vec![],
        checks: vec![],
    };
    (customers, orders)
}

/// Loads the table, then changes it as far as the dialect can in place: renames `status` to
/// `state` and widens it, gives `note` a default, drops `legacy`, adds `shipped_on`, and where
/// the dialect adds constraints, a check and a foreign key. Then loads it again, checks it
/// reads as the new definition and its rows are still there, and takes the constraints off.
fn check_modify_table(
    engine: &mut TestEngine,
    data_source: &DataSource,
    password: Option<Password>,
) {
    save(engine, data_source, password);
    let dialect = sql_dialect(data_source.config.driver());
    let capabilities = dialect.capabilities();
    let suffix = unique("m", data_source);
    let suffix = &suffix[2..];
    let (customers, orders) = orders_to_modify(dialect, suffix);
    let table = orders.name.to_sql(dialect);
    let mut create = ddl_statements(
        &[
            DdlOperation::CreateTable(customers.clone()),
            DdlOperation::CreateTable(orders.clone()),
        ],
        dialect,
    )
    .unwrap();
    create.push(format!(
        "INSERT INTO {table} (id, status, note) VALUES (1, 'paid', 'first'), (2, 'new', NULL)"
    ));
    assert_eq!(apply(engine, data_source.id, create), Ok(()), "{dialect:?}");

    let path = default_schema(data_source)
        .child(NodeKind::Folder(FolderKind::Tables), "tables")
        .child(NodeKind::Table, orders.name.name.clone());
    let loaded = load_table(engine, data_source.id, &path);
    let shape = |spec: &TableSpec| -> Vec<(String, bool)> {
        spec.columns
            .iter()
            .map(|column| (column.name.clone(), column.nullable))
            .collect()
    };
    assert_eq!(shape(&loaded.spec), shape(&orders), "{dialect:?}");
    let status_default = loaded.spec.columns[1].default.clone();
    assert!(
        status_default.as_deref().is_some_and(|d| d.contains("new")),
        "{dialect:?}: {status_default:?}"
    );

    let mut after = loaded.spec.clone();
    let wide = match dialect {
        Dialect::ClickHouse => "LowCardinality(String)",
        _ => "varchar(40)",
    };
    after.columns[1].name = "state".to_owned();
    after.columns[1].data_type = wide.to_owned();
    after.columns[2].default = Some("'none'".to_owned());
    after.columns.remove(3);
    after.columns.push(ColumnSpec {
        name: "shipped_on".to_owned(),
        data_type: match dialect {
            Dialect::ClickHouse => "Date",
            _ => "date",
        }
        .to_owned(),
        nullable: true,
        default: None,
        identity: None,
    });
    let constraints = capabilities.changes_in_place(TableChange::AddConstraint);
    if constraints {
        after.checks.push(Check {
            name: Some(format!("dbrover_positive_{suffix}")),
            expression: "id > 0".to_owned(),
        });
        if capabilities.foreign_keys {
            after.foreign_keys.push(ForeignKey {
                name: Some(format!("dbrover_customer_{suffix}")),
                columns: vec!["customer".to_owned()],
                target: customers.name.clone(),
                target_columns: vec!["id".to_owned()],
                on_delete: ReferentialAction::SetNull,
                on_update: ReferentialAction::NoAction,
            });
        }
    }
    let in_place = [TableChange::ChangeColumnType, TableChange::ChangeDefault]
        .into_iter()
        .all(|change| capabilities.changes_in_place(change));
    if !in_place {
        after.columns[1].data_type = loaded.spec.columns[1].data_type.clone();
        after.columns[2].default = None;
    }
    let alteration = TableAlteration {
        table: loaded.clone(),
        after: after.clone(),
        renamed: BTreeMap::from([("state".to_owned(), "status".to_owned())]),
        conversions: BTreeMap::new(),
    };
    let statements = DdlOperation::AlterTable(Box::new(alteration))
        .statements(dialect)
        .unwrap_or_else(|error| panic!("{dialect:?}: {error}"));
    assert_eq!(
        apply(engine, data_source.id, statements.clone()),
        Ok(()),
        "{dialect:?}: {statements:#?}"
    );

    let altered = load_table(engine, data_source.id, &path);
    assert_eq!(shape(&altered.spec), shape(&after), "{dialect:?}");
    if in_place {
        // DuckDB keeps no length for VARCHAR.
        assert!(
            altered.spec.columns[1].data_type.contains("40")
                || altered.spec.columns[1]
                    .data_type
                    .starts_with("LowCardinality")
                || dialect == Dialect::DuckDb,
            "{dialect:?}: {:?}",
            altered.spec.columns[1]
        );
        assert!(
            altered.spec.columns[2]
                .default
                .as_deref()
                .is_some_and(|d| d.contains("none")),
            "{dialect:?}: {:?}",
            altered.spec.columns[2]
        );
    }
    assert_eq!(
        altered.spec.primary_key, loaded.spec.primary_key,
        "{dialect:?}"
    );
    assert_eq!(altered.spec.checks.len(), after.checks.len(), "{dialect:?}");
    let keys: Vec<(&[String], ReferentialAction)> = altered
        .spec
        .foreign_keys
        .iter()
        .map(|key| (key.target_columns.as_slice(), key.on_delete))
        .collect();
    let expected_keys: Vec<(&[String], ReferentialAction)> = after
        .foreign_keys
        .iter()
        .map(|key| (key.target_columns.as_slice(), key.on_delete))
        .collect();
    assert_eq!(keys, expected_keys, "{dialect:?}");
    let rows = query(
        engine,
        data_source.id,
        &format!("SELECT id, state FROM {table} ORDER BY id"),
    );
    let some = |text: &str| Some(text.to_owned());
    assert_eq!(
        rows,
        [[some("1"), some("paid")], [some("2"), some("new")]],
        "{dialect:?}"
    );

    if constraints {
        let mut bare = altered.spec.clone();
        bare.checks.clear();
        bare.foreign_keys.clear();
        let undo = DdlOperation::AlterTable(Box::new(TableAlteration {
            table: altered,
            after: bare,
            renamed: BTreeMap::new(),
            conversions: BTreeMap::new(),
        }))
        .statements(dialect)
        .unwrap();
        assert_eq!(apply(engine, data_source.id, undo), Ok(()), "{dialect:?}");
        let undone = load_table(engine, data_source.id, &path);
        assert!(undone.spec.checks.is_empty(), "{dialect:?}: {undone:?}");
        assert!(
            undone.spec.foreign_keys.is_empty(),
            "{dialect:?}: {undone:?}"
        );
    }

    let drop = ddl_statements(
        &[
            DdlOperation::DropTable {
                table: orders.name,
                cascade: false,
            },
            DdlOperation::DropTable {
                table: customers.name,
                cascade: false,
            },
        ],
        dialect,
    )
    .unwrap();
    assert_eq!(apply(engine, data_source.id, drop), Ok(()), "{dialect:?}");
}

#[test]
fn modifies_a_table_with_rows_on_every_engine() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let sqlite = sqlite_data_source(sqlite_database(dir.path()));
    check_modify_table(&mut engine, &sqlite, None);
    // A file, which the console reading the rows shares; each connection to an in-memory
    // database has one of its own.
    let file = DatabaseFile::DuckDb(dir.path().join("modify.duckdb"));
    engine.send(Command::CreateDatabaseFile(file.clone()));
    engine.expect(Event::DatabaseFileCreated {
        file,
        result: Ok(()),
    });
    let duckdb = data_source(
        "duckdb",
        ConnectionConfig::DuckDb(DuckDbConfig {
            database: DuckDbDatabase::File {
                path: dir.path().join("modify.duckdb"),
                read_only: false,
            },
        }),
    );
    check_modify_table(&mut engine, &duckdb, None);
    let servers = every_engine()
        .into_iter()
        .filter(|(data_source, _)| data_source.config.driver() != DriverKind::DuckDb);
    for (data_source, password) in servers {
        check_modify_table(&mut engine, &data_source, password);
    }
    engine.shut_down();
}

fn sqlite_rows(database: &std::path::Path, sql: &str) -> Vec<Vec<Option<String>>> {
    let connection = rusqlite::Connection::open(database).unwrap();
    let mut statement = connection.prepare(sql).unwrap();
    let width = statement.column_count();
    statement
        .query_map([], |row| {
            (0..width)
                .map(|index| {
                    row.get::<_, rusqlite::types::Value>(index)
                        .map(|value| match value {
                            rusqlite::types::Value::Null => None,
                            rusqlite::types::Value::Integer(number) => Some(number.to_string()),
                            rusqlite::types::Value::Real(number) => Some(number.to_string()),
                            rusqlite::types::Value::Text(text) => Some(text),
                            rusqlite::types::Value::Blob(_) => Some("blob".to_owned()),
                        })
                })
                .collect()
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// SQLite's rebuild, with foreign keys switched on as an application would: the rows, the
/// index and trigger on the table, a view reading it, `AUTOINCREMENT`, and another table's
/// cascading foreign key to it all survive, renamed columns renamed in each. A rebuild that
/// leaves a row's foreign key pointing at nothing is undone.
#[test]
fn rebuilds_a_sqlite_table_keeping_what_depends_on_it() {
    let dir = tempfile::tempdir().unwrap();
    let database = sqlite_database(dir.path());
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE audit (order_id INTEGER);
             CREATE TABLE orders (id INTEGER PRIMARY KEY AUTOINCREMENT,
               customer INTEGER REFERENCES customers (id), total TEXT, legacy INTEGER);
             CREATE INDEX orders_customer ON orders (customer);
             CREATE TRIGGER orders_audit AFTER INSERT ON orders
               BEGIN INSERT INTO audit VALUES (NEW.id); END;
             CREATE VIEW customer_totals AS SELECT customer, total FROM orders;
             CREATE TABLE lines (order_id INTEGER REFERENCES orders (id) ON DELETE CASCADE,
               sku TEXT);
             INSERT INTO customers VALUES (1, 'ada'), (2, 'bob');
             INSERT INTO orders (customer, total, legacy) VALUES (1, '10.5', 7), (2, '20', 8);
             INSERT INTO lines VALUES (1, 'a'), (2, 'b');",
        )
        .unwrap();
    let mut engine = start(dir.path());
    let app = sqlite_data_source(database.clone());
    save(&mut engine, &app, None);
    // Ends the transaction ApplyDdl opens, so the switch takes, and opens another for it.
    let switched = apply(
        &mut engine,
        app.id,
        vec![
            "COMMIT".to_owned(),
            "PRAGMA foreign_keys = ON".to_owned(),
            "BEGIN".to_owned(),
        ],
    );
    assert_eq!(switched, Ok(()));

    let tables = default_schema(&app).child(NodeKind::Folder(FolderKind::Tables), "tables");
    let orders = load_table(
        &mut engine,
        app.id,
        &tables.child(NodeKind::Table, "orders"),
    );
    assert_eq!(orders.dependents.len(), 2, "{:?}", orders.dependents);
    assert_eq!(orders.rebuild_refusal, None);
    let mut after = orders.spec.clone();
    after.columns[1].name = "buyer".to_owned();
    after.columns[1].nullable = false;
    after.columns[2].data_type = "REAL".to_owned();
    after.columns.remove(3);
    after.foreign_keys[0].columns = vec!["buyer".to_owned()];
    after.checks.push(Check {
        name: None,
        expression: "total >= 0".to_owned(),
    });
    let alteration = TableAlteration {
        table: orders,
        after,
        renamed: BTreeMap::from([("buyer".to_owned(), "customer".to_owned())]),
        conversions: BTreeMap::new(),
    };
    assert!(alteration.rebuilds(Dialect::Sqlite));
    let statements = DdlOperation::AlterTable(Box::new(alteration))
        .statements(Dialect::Sqlite)
        .unwrap();
    assert_eq!(
        apply(&mut engine, app.id, statements.clone()),
        Ok(()),
        "{statements:#?}"
    );

    let some = |text: &str| Some(text.to_owned());
    assert_eq!(
        sqlite_rows(&database, "SELECT id, buyer, total FROM orders ORDER BY id"),
        [
            [some("1"), some("1"), some("10.5")],
            [some("2"), some("2"), some("20")]
        ]
    );
    assert_eq!(
        sqlite_rows(&database, "SELECT count(*) FROM lines"),
        [[some("2")]],
        "dropping the old table ran no ON DELETE CASCADE"
    );
    assert_eq!(
        sqlite_rows(
            &database,
            "SELECT name FROM pragma_index_info('orders_customer')"
        ),
        [[some("buyer")]]
    );
    assert_eq!(
        sqlite_rows(&database, "SELECT * FROM customer_totals ORDER BY buyer"),
        [[some("1"), some("10.5")], [some("2"), some("20")]],
        "the view follows the rename"
    );
    let definition = sqlite_rows(
        &database,
        "SELECT sql FROM sqlite_schema WHERE name = 'orders'",
    );
    let definition = definition[0][0].clone().unwrap_or_default();
    for part in [
        "AUTOINCREMENT",
        "buyer INTEGER NOT NULL",
        "CHECK (total >= 0)",
    ] {
        assert!(definition.contains(part), "{part} in {definition}");
    }
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute("INSERT INTO orders (buyer, total) VALUES (1, 5)", [])
        .unwrap();
    assert_eq!(
        sqlite_rows(&database, "SELECT order_id FROM audit ORDER BY order_id"),
        [[some("1")], [some("2")], [some("3")]],
        "the trigger is back"
    );

    // Audit row 3's order has no customer 3: a key from audit to customers would point at
    // nothing, and the rebuild that adds one is undone.
    let audit = load_table(&mut engine, app.id, &tables.child(NodeKind::Table, "audit"));
    let mut after = audit.spec.clone();
    after.foreign_keys.push(ForeignKey {
        name: None,
        columns: vec!["order_id".to_owned()],
        target: ObjectName::new(Some("main"), "customers"),
        target_columns: vec!["id".to_owned()],
        on_delete: ReferentialAction::NoAction,
        on_update: ReferentialAction::NoAction,
    });
    let breaking = DdlOperation::AlterTable(Box::new(TableAlteration {
        table: audit,
        after,
        renamed: BTreeMap::new(),
        conversions: BTreeMap::new(),
    }))
    .statements(Dialect::Sqlite)
    .unwrap();
    let failure = apply(&mut engine, app.id, breaking).expect_err("a broken key");
    assert!(failure.message.contains("audit"), "{failure:?}");
    assert_eq!(
        sqlite_rows(
            &database,
            "SELECT count(*) FROM pragma_foreign_key_list('audit')"
        ),
        [[some("0")]],
        "undone"
    );
    engine.shut_down();
}
