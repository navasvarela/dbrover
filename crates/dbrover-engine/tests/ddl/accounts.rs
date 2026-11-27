//! MySQL's and MariaDB's accounts through the engine (W18): what the server offers, an
//! account created with a password hashed on the client and kept out of the history, and
//! Copy DDL without it.

use dbrover_core::{AccountKind, AccountName, ManagedUsers, MysqlFlavor, UserManagement};
use dbrover_sql::{AccountHash, AccountOperation, AccountPassword, AccountSpec};

use super::*;

/// The compose servers' root password, which may create accounts; the test login may not.
const ROOT_PASSWORD: &str = "dbrover";

fn user_management(engine: &mut TestEngine, data_source: DataSourceId) -> UserManagement {
    engine.send(Command::LoadUserManagement(data_source));
    loop {
        match engine.next_event() {
            Event::UserManagementLoaded {
                data_source: answered,
                result,
            } if answered == data_source => return result.expect("what the server offers"),
            Event::SessionChanged { .. } => {}
            other => panic!("expected UserManagementLoaded, got {other:?}"),
        }
    }
}

#[test]
fn creates_an_account_whose_password_stays_out_of_the_history_and_copy_ddl() {
    for (mut config, _) in mysql_test_servers() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = start(dir.path());
        config.server.user = "root".to_owned();
        let mysql = data_source("mysql", ConnectionConfig::Mysql(config));
        save(&mut engine, &mysql, Some(Password::new(ROOT_PASSWORD)));
        let management = user_management(&mut engine, mysql.id);
        let ManagedUsers::Accounts(server) = &management.server else {
            panic!("not accounts: {management:?}");
        };
        assert!(management.may_change.allowed(), "{management:?}");
        let flavor = server.flavor;
        let hash = match flavor {
            MysqlFlavor::Mysql => AccountHash::caching_sha2("W18e-pass!"),
            MysqlFlavor::MariaDb => AccountHash::mysql_native("W18e-pass!"),
        };
        let name = AccountName::new(unique("w18e", &mysql), "%");
        let _ = apply(
            &mut engine,
            mysql.id,
            vec![format!("DROP USER IF EXISTS `{}`@`%`", name.user)],
        );
        let create = DdlOperation::Account {
            flavor,
            operation: AccountOperation::Create {
                name: name.clone(),
                spec: AccountSpec {
                    locked: true,
                    ..AccountSpec::new(AccountKind::User)
                },
                password: Some(AccountPassword::Hashed(hash.clone())),
            },
        };
        let statements = ddl_statements(&[create], Dialect::Mysql).unwrap();
        assert_eq!(apply(&mut engine, mysql.id, statements), Ok(()));

        let path = CatalogPath::root()
            .child(NodeKind::Folder(FolderKind::Users), "users")
            .child(NodeKind::User, name.label());
        let ddl = load_ddl(&mut engine, mysql.id, path);
        assert_eq!(
            ddl,
            format!("CREATE USER `{}`@`%` ACCOUNT LOCK;\n", name.user),
            "Copy DDL has no password"
        );

        engine.send(Command::SearchHistory(HistoryQuery::all(mysql.id)));
        let entries = match engine.next_event() {
            Event::HistoryFound { result, .. } => result.unwrap(),
            other => panic!("expected the history, got {other:?}"),
        };
        let created = entries
            .iter()
            .find(|entry| entry.sql.starts_with("CREATE USER"))
            .unwrap_or_else(|| panic!("no CREATE USER in {entries:#?}"));
        assert!(created.sql.contains("'<redacted>'"), "{}", created.sql);
        for entry in &entries {
            assert!(!entry.sql.contains(hash.as_str()), "{}", entry.sql);
        }
        assert_eq!(
            apply(
                &mut engine,
                mysql.id,
                vec![format!("DROP USER `{}`@`%`", name.user)]
            ),
            Ok(())
        );
        engine.shut_down();
    }
}

#[test]
fn creates_a_sql_server_login_whose_hash_stays_out_of_the_history() {
    use dbrover_sql::{
        DatabaseUserSpec, LoginPolicy, LoginSource, LoginSpec, SqlServerHash, SqlServerOperation,
        SqlServerPassword, UserMapping,
    };

    let Some((config, password)) = sqlserver_test_server() else {
        return;
    };
    let database = config.server.database.clone();
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let sqlserver = data_source("sqlserver", ConnectionConfig::SqlServer(config));
    save(&mut engine, &sqlserver, Some(password));
    let management = user_management(&mut engine, sqlserver.id);
    assert!(
        matches!(management.server, ManagedUsers::Logins(_)),
        "{management:?}"
    );
    let login = unique("w18f", &sqlserver);
    let hash = SqlServerHash::hash("W18f-Pass-2026!");
    let create = DdlOperation::SqlServer(SqlServerOperation::CreateLogin {
        name: login.clone(),
        source: LoginSource::Sql {
            password: SqlServerPassword::Hashed(hash.clone()),
            must_change: false,
        },
        spec: LoginSpec {
            server_roles: ["##MS_DefinitionReader##".to_owned()].into(),
            ..LoginSpec::new(Some(LoginPolicy {
                check_policy: false,
                check_expiration: false,
            }))
        },
        mappings: vec![UserMapping {
            database: database.clone(),
            user: login.clone(),
            spec: DatabaseUserSpec {
                default_schema: Some("dbo".to_owned()),
                roles: ["db_datareader".to_owned()].into(),
            },
        }],
    });
    let statements = ddl_statements(&[create], Dialect::SqlServer).unwrap();
    assert_eq!(apply(&mut engine, sqlserver.id, statements), Ok(()));

    engine.send(Command::SearchHistory(HistoryQuery::all(sqlserver.id)));
    let entries = match engine.next_event() {
        Event::HistoryFound { result, .. } => result.unwrap(),
        other => panic!("expected the history, got {other:?}"),
    };
    let created = entries
        .iter()
        .find(|entry| entry.sql.starts_with("CREATE LOGIN"))
        .unwrap_or_else(|| panic!("no CREATE LOGIN in {entries:#?}"));
    assert!(
        created.sql.contains("PASSWORD = 0x<redacted> HASHED"),
        "{}",
        created.sql
    );
    for entry in &entries {
        assert!(!entry.sql.contains(&hash.as_str()[2..]), "{}", entry.sql);
    }
    let drop = DdlOperation::SqlServer(SqlServerOperation::DropLogin {
        name: login.clone(),
        users: vec![dbrover_core::DatabaseUserName {
            database,
            user: login,
        }],
    });
    let statements = ddl_statements(&[drop], Dialect::SqlServer).unwrap();
    assert_eq!(apply(&mut engine, sqlserver.id, statements), Ok(()));
    engine.shut_down();
}

/// The children at `path` of `data_source`, or why it couldn't connect or list them.
fn try_children(
    engine: &mut TestEngine,
    data_source: DataSourceId,
    path: CatalogPath,
) -> Result<Vec<String>, String> {
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
            } if loaded == path => {
                return result
                    .map(|page| page.nodes.into_iter().map(|node| node.label).collect())
                    .map_err(|error| error.to_string());
            }
            Event::SessionChanged {
                state: SessionState::Failed(error),
                ..
            } => return Err(error),
            Event::SessionChanged { .. } => {}
            other => panic!("expected ChildrenLoaded, got {other:?}"),
        }
    }
}

/// The milestone's exit (`docs/users-beyond-postgres.md`): on MySQL and MariaDB, a user
/// created with a password, SELECT on a database through its default role, logs in from a new
/// data source and reads; locked, its login is refused; dropped, it is gone. On SQL Server, a
/// SQL login with a user in `db_datareader` logs in and reads; disabled, it is refused;
/// dropped with its users, nothing is left. A read-only data source refuses the statements; no
/// password or hash reaches the history.
#[test]
fn w18_exit() {
    use dbrover_core::{AdminOption, Privilege, PrivilegeSet};
    use dbrover_sql::{
        DatabaseUserSpec, LoginPolicy, LoginSource, LoginSpec, SqlServerHash, SqlServerOperation,
        SqlServerPassword, UserMapping,
    };

    const PASSWORD: &str = "W18-Exit-2026!";
    let dir = tempfile::tempdir().unwrap();
    let mut engine = start(dir.path());
    let mut hashes = Vec::new();

    for (mut config, _) in mysql_test_servers() {
        let database = config.server.database.clone();
        config.server.user = "root".to_owned();
        let admin = data_source("mysql", ConnectionConfig::Mysql(config.clone()));
        save(&mut engine, &admin, Some(Password::new(ROOT_PASSWORD)));
        let ManagedUsers::Accounts(server) = user_management(&mut engine, admin.id).server else {
            panic!("not accounts");
        };
        let flavor = server.flavor;
        let user = unique("w18x", &admin);
        let role = AccountName::role(format!("{user}_reader"), flavor);
        let account = AccountName::new(user.clone(), "%");
        let table = format!("`{database}`.`{user}_orders`");
        let setup = vec![format!("CREATE TABLE {table} (id INT PRIMARY KEY)")];
        assert_eq!(apply(&mut engine, admin.id, setup), Ok(()));
        let hash = match flavor {
            MysqlFlavor::Mysql => AccountHash::caching_sha2(PASSWORD),
            MysqlFlavor::MariaDb => AccountHash::mysql_native(PASSWORD),
        };
        hashes.push(hash.as_str().to_owned());
        let operations = [
            DdlOperation::Account {
                flavor,
                operation: AccountOperation::Create {
                    name: role.clone(),
                    spec: AccountSpec {
                        databases: [(
                            database.clone(),
                            PrivilegeSet {
                                privileges: [Privilege::new("SELECT")].into(),
                                grant_option: false,
                            },
                        )]
                        .into(),
                        ..AccountSpec::new(AccountKind::Role)
                    },
                    password: None,
                },
            },
            DdlOperation::Account {
                flavor,
                operation: AccountOperation::Create {
                    name: account.clone(),
                    spec: AccountSpec {
                        roles: [(role.clone(), AdminOption::Without)].into(),
                        default_roles: [role.clone()].into(),
                        ..AccountSpec::new(AccountKind::User)
                    },
                    password: Some(AccountPassword::Hashed(hash)),
                },
            },
        ];
        let statements = ddl_statements(&operations, Dialect::Mysql).unwrap();

        // A read-only data source refuses them, unsent.
        let mut read_only = data_source("mysql read-only", ConnectionConfig::Mysql(config.clone()));
        read_only.safety = SafetyLevel::ReadOnly;
        save(&mut engine, &read_only, Some(Password::new(ROOT_PASSWORD)));
        assert!(apply(&mut engine, read_only.id, statements.clone()).is_err());
        assert_eq!(apply(&mut engine, admin.id, statements), Ok(()));

        // The new account logs in and reads the table through its default role.
        let mut as_user = config.clone();
        as_user.server.user.clone_from(&user);
        let login = data_source("as the new user", ConnectionConfig::Mysql(as_user));
        save(&mut engine, &login, Some(Password::new(PASSWORD)));
        let tables = CatalogPath::root()
            .child(NodeKind::Schema, &database)
            .child(NodeKind::Folder(FolderKind::Tables), "tables");
        let seen = try_children(&mut engine, login.id, tables.clone()).unwrap();
        assert!(seen.contains(&format!("{user}_orders")), "{seen:?}");

        // Locked, a new connection is refused.
        let lock = DdlOperation::Account {
            flavor,
            operation: AccountOperation::SetLocked {
                name: account.clone(),
                locked: true,
            },
        };
        assert_eq!(
            apply(
                &mut engine,
                admin.id,
                ddl_statements(&[lock], Dialect::Mysql).unwrap()
            ),
            Ok(())
        );
        let locked_out = data_source("locked out", login.config.clone());
        save(&mut engine, &locked_out, Some(Password::new(PASSWORD)));
        assert!(try_children(&mut engine, locked_out.id, tables).is_err());

        // Dropped, with its role; it is gone from the users folder.
        let drops = [
            DdlOperation::Account {
                flavor,
                operation: AccountOperation::Drop {
                    name: account.clone(),
                    kind: AccountKind::User,
                },
            },
            DdlOperation::Account {
                flavor,
                operation: AccountOperation::Drop {
                    name: role,
                    kind: AccountKind::Role,
                },
            },
        ];
        let mut statements = ddl_statements(&drops, Dialect::Mysql).unwrap();
        statements.push(format!("DROP TABLE {table}"));
        assert_eq!(apply(&mut engine, admin.id, statements), Ok(()));
        let users = CatalogPath::root().child(NodeKind::Folder(FolderKind::Users), "users");
        let listed = try_children(&mut engine, admin.id, users).unwrap();
        assert!(!listed.contains(&account.label()), "{listed:?}");
        check_history(&mut engine, admin.id, &hashes);
    }

    if let Some((config, password)) = sqlserver_test_server() {
        let database = config.server.database.clone();
        let admin = data_source("sqlserver", ConnectionConfig::SqlServer(config.clone()));
        save(&mut engine, &admin, Some(password));
        let login = unique("w18x", &admin);
        let table = format!("{login}_orders");
        let setup = vec![format!("CREATE TABLE dbo.[{table}] (id int PRIMARY KEY)")];
        assert_eq!(apply(&mut engine, admin.id, setup), Ok(()));
        let hash = SqlServerHash::hash(PASSWORD);
        hashes.push(hash.as_str()[2..].to_owned());
        let create = DdlOperation::SqlServer(SqlServerOperation::CreateLogin {
            name: login.clone(),
            source: LoginSource::Sql {
                password: SqlServerPassword::Hashed(hash),
                must_change: false,
            },
            spec: LoginSpec {
                default_database: database.clone(),
                ..LoginSpec::new(Some(LoginPolicy {
                    check_policy: false,
                    check_expiration: false,
                }))
            },
            mappings: vec![UserMapping {
                database: database.clone(),
                user: login.clone(),
                spec: DatabaseUserSpec {
                    default_schema: Some("dbo".to_owned()),
                    roles: ["db_datareader".to_owned()].into(),
                },
            }],
        });
        let statements = ddl_statements(&[create], Dialect::SqlServer).unwrap();
        assert_eq!(apply(&mut engine, admin.id, statements), Ok(()));

        let mut as_login = config.clone();
        as_login.server.user.clone_from(&login);
        let reader = data_source("as the new login", ConnectionConfig::SqlServer(as_login));
        save(&mut engine, &reader, Some(Password::new(PASSWORD)));
        let tables = CatalogPath::root()
            .child(NodeKind::Database, &database)
            .child(NodeKind::Schema, "dbo")
            .child(NodeKind::Folder(FolderKind::Tables), "tables");
        let seen = try_children(&mut engine, reader.id, tables.clone()).unwrap();
        assert!(seen.contains(&table), "{seen:?}");

        let disable = DdlOperation::SqlServer(SqlServerOperation::SetLoginEnabled {
            name: login.clone(),
            enabled: false,
        });
        let statements = ddl_statements(&[disable], Dialect::SqlServer).unwrap();
        assert_eq!(apply(&mut engine, admin.id, statements), Ok(()));
        let refused = data_source("disabled", reader.config.clone());
        save(&mut engine, &refused, Some(Password::new(PASSWORD)));
        assert!(try_children(&mut engine, refused.id, tables).is_err());

        // SQL Server won't drop a login that is still connected.
        disconnect(&mut engine, reader.id);
        let drop = DdlOperation::SqlServer(SqlServerOperation::DropLogin {
            name: login.clone(),
            users: vec![dbrover_core::DatabaseUserName {
                database: database.clone(),
                user: login.clone(),
            }],
        });
        let mut statements = ddl_statements(&[drop], Dialect::SqlServer).unwrap();
        statements.push(format!("DROP TABLE dbo.[{table}]"));
        assert_eq!(apply(&mut engine, admin.id, statements), Ok(()));
        let users = CatalogPath::root()
            .child(NodeKind::Database, &database)
            .child(NodeKind::Folder(FolderKind::Users), "users");
        let logins = CatalogPath::root().child(NodeKind::Folder(FolderKind::Logins), "logins");
        assert!(
            !try_children(&mut engine, admin.id, users)
                .unwrap()
                .contains(&login)
        );
        assert!(
            !try_children(&mut engine, admin.id, logins)
                .unwrap()
                .contains(&login)
        );
        check_history(&mut engine, admin.id, &hashes);
    }
    engine.shut_down();
}

/// No password, nor any of `hashes`, in `data_source`'s history.
fn check_history(engine: &mut TestEngine, data_source: DataSourceId, hashes: &[String]) {
    engine.send(Command::SearchHistory(HistoryQuery::all(data_source)));
    let entries = match engine.next_event() {
        Event::HistoryFound { result, .. } => result.unwrap(),
        other => panic!("expected the history, got {other:?}"),
    };
    assert!(!entries.is_empty());
    for entry in &entries {
        assert!(!entry.sql.contains("W18-Exit-2026"), "{}", entry.sql);
        for hash in hashes {
            assert!(!entry.sql.contains(hash.as_str()), "{}", entry.sql);
        }
    }
}

/// Closes `data_source`'s connection, and waits until the engine says so.
fn disconnect(engine: &mut TestEngine, data_source: DataSourceId) {
    engine.send(Command::Disconnect(data_source));
    loop {
        if let Event::SessionChanged {
            id,
            state: SessionState::Disconnected,
        } = engine.next_event()
            && id == data_source
        {
            return;
        }
    }
}
