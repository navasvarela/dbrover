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
