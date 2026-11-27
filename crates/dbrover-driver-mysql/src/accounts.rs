//! MySQL's and MariaDB's accounts and roles: the users and roles folders, one account's
//! description, and what the server and this login allow (W18,
//! `docs/users-beyond-postgres.md`).
//!
//! Account properties come from the `mysql` schema, which only a login with `SELECT` on it (or
//! `CREATE USER`) may read; privileges come from `SHOW GRANTS` (global) and
//! `information_schema` (each database, table and column), which a login may always read for
//! its own account. So a login without those privileges still sees its own account.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU16;

use dbrover_core::{
    AccountDescription, AccountKind, AccountLimits, AccountName, AccountServer, AdminOption,
    CatalogNode, CatalogPath, DefinedKind, DefinedObject, DriverError, FailedLoginLock, FolderKind,
    LockTime, ManagedUsers, MysqlFlavor, NodeKind, ObjectLevel, ObjectPrivilege, PasswordLifetime,
    Permit, Privilege, PrivilegeSet, RoutineKind, UserManagement,
};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Params, Value};

use crate::activity::version_at_least;
use crate::{Flavour, connection_error};

/// `ER_TABLEACCESS_DENIED_ERROR`, `ER_SPECIFIC_ACCESS_DENIED_ERROR`,
/// `ER_DBACCESS_DENIED_ERROR` and `ER_NONEXISTING_GRANT`: the `mysql` schema, or a grant, this
/// login may not read; and `ER_NO_SUCH_TABLE`, a table a server this old doesn't have (MySQL
/// 5.7's `role_edges`).
const UNAVAILABLE: [u16; 5] = [1142, 1227, 1044, 1141, 1146];
/// What lets a login read every account.
const LISTS_ACCOUNTS: &str = "CREATE USER, or SELECT on the mysql database";
const CHANGES_ACCOUNTS: &str = "CREATE USER";
/// MySQL 8.0.19 added `FAILED_LOGIN_ATTEMPTS` and `PASSWORD_LOCK_TIME`.
const FAILED_LOGIN_LOCK_SINCE: (u32, u32, u32) = (8, 0, 19);
/// The `PASSWORD_LOCK_TIME` MySQL stores for `UNBOUNDED`.
const UNBOUNDED_LOCK: i64 = -1;

/// The accounts the server or its provider keeps for itself, shown as system objects. The
/// providers' names are from their documentation, not tried against their services.
const SYSTEM_ACCOUNTS: [&str; 10] = [
    "mysql.sys",
    "mysql.session",
    "mysql.infoschema",
    "mariadb.sys",
    "rdsadmin",
    "rdsrepladmin",
    "azure_superuser",
    "cloudsqladmin",
    "cloudsqlimport",
    "cloudsqlexport",
];

impl From<Flavour> for MysqlFlavor {
    fn from(flavour: Flavour) -> Self {
        match flavour {
            Flavour::Mysql => Self::Mysql,
            Flavour::MariaDb => Self::MariaDb,
        }
    }
}

/// One account as the folders list it.
struct Listed {
    name: AccountName,
    kind: AccountKind,
    locked: bool,
    expired: bool,
}

/// The accounts in the users or the roles folder at `path`.
pub(crate) async fn list(
    conn: &mut Conn,
    flavour: Flavour,
    path: &CatalogPath,
    folder: FolderKind,
) -> Result<Vec<CatalogNode>, DriverError> {
    let wanted = match folder {
        FolderKind::Roles => AccountKind::Role,
        _ => AccountKind::User,
    };
    let Some(accounts) = every_account(conn, flavour).await? else {
        // Only the login's own account, which it may always describe.
        if wanted == AccountKind::Role {
            return Ok(Vec::new());
        }
        let own = current_account(conn).await?;
        return Ok(vec![account_node(
            path,
            NodeKind::User,
            &own,
            Some("your account; the others need ".to_owned() + LISTS_ACCOUNTS),
        )]);
    };
    let grants = role_grants(conn, flavour).await?.unwrap_or_default();
    let kind = match wanted {
        AccountKind::User => NodeKind::User,
        AccountKind::Role => NodeKind::Role,
    };
    Ok(accounts
        .iter()
        .filter(|account| account.kind == wanted)
        .map(|account| {
            let detail = match wanted {
                AccountKind::User => user_detail(account, &grants),
                AccountKind::Role => role_detail(&account.name, &grants),
            };
            account_node(path, kind, &account.name, detail)
        })
        .collect())
}

fn account_node(
    parent: &CatalogPath,
    kind: NodeKind,
    name: &AccountName,
    detail: Option<String>,
) -> CatalogNode {
    let label = name.label();
    CatalogNode {
        path: parent.child(kind, label.clone()),
        label,
        detail,
        has_children: false,
        has_definition: false,
        system: SYSTEM_ACCOUNTS.contains(&name.user.as_str()),
    }
}

/// "locked, password expired, member of app_read", or whichever parts apply.
fn user_detail(account: &Listed, grants: &[RoleGrant]) -> Option<String> {
    let mut parts = Vec::new();
    if account.locked {
        parts.push("locked".to_owned());
    }
    if account.expired {
        parts.push("password expired".to_owned());
    }
    let roles: Vec<String> = grants
        .iter()
        .filter(|grant| grant.to == account.name)
        .map(|grant| grant.role.user.clone())
        .collect();
    if !roles.is_empty() {
        parts.push(format!("member of {}", roles.join(", ")));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn role_detail(role: &AccountName, grants: &[RoleGrant]) -> Option<String> {
    let members = grants.iter().filter(|grant| grant.role == *role).count();
    match members {
        0 => None,
        1 => Some("granted to 1 account".to_owned()),
        n => Some(format!("granted to {n} accounts")),
    }
}

/// Every account, or `None` when this login may not read the `mysql` schema.
async fn every_account(
    conn: &mut Conn,
    flavour: Flavour,
) -> Result<Option<Vec<Listed>>, DriverError> {
    let rows = match flavour {
        // A role is an account `CREATE ROLE` made: locked, with no password. MySQL has no
        // mark of its own for it (question 2 of the plan).
        Flavour::Mysql => {
            try_query(
                conn,
                "SELECT User, Host, account_locked = 'Y', password_expired = 'Y', \
                        account_locked = 'Y' AND authentication_string = '' \
                 FROM mysql.user ORDER BY User, Host",
                (),
            )
            .await?
        }
        // MariaDB keeps an account's properties in `global_priv`'s JSON; `mysql.user` is a view
        // over it without the lock. A changed password's time is 0 once it is expired, and
        // `JSON_VALUE` reads a JSON `true` as `1`.
        Flavour::MariaDb => {
            try_query(
                conn,
                "SELECT User, Host, \
                        IFNULL(JSON_VALUE(Priv, '$.account_locked'), 0) = 1, \
                        JSON_VALUE(Priv, '$.password_last_changed') = '0', \
                        IFNULL(JSON_VALUE(Priv, '$.is_role'), 0) = 1 \
                 FROM mysql.global_priv ORDER BY User, Host",
                (),
            )
            .await?
        }
    };
    let Some(rows) = rows else {
        return Ok(None);
    };
    rows.iter()
        .map(|row| {
            let role = flag(row, 4);
            Ok(Listed {
                name: AccountName::new(field(row, 0)?, field(row, 1)?),
                kind: if role {
                    AccountKind::Role
                } else {
                    AccountKind::User
                },
                locked: flag(row, 2) && !role,
                expired: flag(row, 3),
            })
        })
        .collect::<Result<Vec<_>, DriverError>>()
        .map(Some)
}

/// A role granted to an account.
struct RoleGrant {
    role: AccountName,
    to: AccountName,
    admin: AdminOption,
}

/// Every role grant, or `None` when this login may not read them.
async fn role_grants(
    conn: &mut Conn,
    flavour: Flavour,
) -> Result<Option<Vec<RoleGrant>>, DriverError> {
    let sql = match flavour {
        Flavour::Mysql => {
            "SELECT FROM_USER, FROM_HOST, TO_USER, TO_HOST, WITH_ADMIN_OPTION = 'Y' \
             FROM mysql.role_edges ORDER BY FROM_USER"
        }
        // A role has no host on MariaDB.
        Flavour::MariaDb => {
            "SELECT Role, '', User, Host, Admin_option = 'Y' \
             FROM mysql.roles_mapping ORDER BY Role"
        }
    };
    let Some(rows) = try_query(conn, sql, ()).await? else {
        return Ok(None);
    };
    rows.iter()
        .map(|row| {
            Ok(RoleGrant {
                role: AccountName::new(field(row, 0)?, field(row, 1)?),
                to: AccountName::new(field(row, 2)?, field(row, 3)?),
                admin: if flag(row, 4) {
                    AdminOption::With
                } else {
                    AdminOption::Without
                },
            })
        })
        .collect::<Result<Vec<_>, DriverError>>()
        .map(Some)
}

/// The account at `path`, a node of the users or roles folder.
pub(crate) async fn describe(
    conn: &mut Conn,
    flavour: Flavour,
    path: &CatalogPath,
) -> Result<AccountDescription, DriverError> {
    let invalid = || DriverError::InvalidPath(path.clone());
    let (kind, label) = match path.segments() {
        [_folder, account] if account.kind == NodeKind::User => {
            (AccountKind::User, account.name.as_str())
        }
        [_folder, account] if account.kind == NodeKind::Role => {
            (AccountKind::Role, account.name.as_str())
        }
        _ => return Err(invalid()),
    };
    let name = AccountName::from_label(label);
    let properties = properties(conn, flavour, &name).await?;
    // Only a login that can read the `mysql` schema learns more than the account's privileges;
    // it may describe its own account anyway, as an account that logs in.
    let properties = match properties {
        Some(Some(properties)) => properties,
        Some(None) => return Err(invalid()),
        None => Properties::unknown(kind),
    };
    let grants = role_grants(conn, flavour).await?.unwrap_or_default();
    let roles = grants
        .iter()
        .filter(|grant| grant.to == name)
        .map(|grant| (grant.role.clone(), grant.admin))
        .collect();
    let members = grants
        .iter()
        .filter(|grant| grant.role == name)
        .map(|grant| grant.to.clone())
        .collect();
    let default_roles = default_roles(conn, flavour, &name).await?;
    Ok(AccountDescription {
        global: global_privileges(conn, &name).await?,
        databases: database_privileges(conn, &name).await?,
        objects: object_privileges(conn, &name).await?,
        defines: defined_objects(conn, &name).await?,
        name,
        kind: properties.kind,
        plugin: properties.plugin,
        has_password: properties.has_password,
        locked: properties.locked,
        password_expired: properties.expired,
        lifetime: properties.lifetime,
        failed_login: properties.failed_login,
        limits: properties.limits,
        roles,
        default_roles,
        members,
    })
}

/// An account's own settings, from the `mysql` schema.
struct Properties {
    kind: AccountKind,
    plugin: String,
    has_password: bool,
    locked: bool,
    expired: bool,
    lifetime: PasswordLifetime,
    failed_login: Option<FailedLoginLock>,
    limits: AccountLimits,
}

impl Properties {
    /// What is shown of an account whose settings this login may not read.
    fn unknown(kind: AccountKind) -> Self {
        Self {
            kind,
            plugin: String::new(),
            has_password: kind == AccountKind::User,
            locked: false,
            expired: false,
            lifetime: PasswordLifetime::ServerDefault,
            failed_login: None,
            limits: AccountLimits::default(),
        }
    }
}

/// `None` when this login may not read them, `Some(None)` when there is no such account.
async fn properties(
    conn: &mut Conn,
    flavour: Flavour,
    name: &AccountName,
) -> Result<Option<Option<Properties>>, DriverError> {
    let sql = match flavour {
        Flavour::Mysql => {
            "SELECT plugin, authentication_string <> '', account_locked = 'Y', \
                    password_expired = 'Y', password_lifetime, max_user_connections, \
                    max_connections, max_questions, max_updates, \
                    JSON_EXTRACT(User_attributes, '$.Password_locking.failed_login_attempts'), \
                    JSON_EXTRACT(User_attributes, '$.Password_locking.password_lock_time_days'), \
                    account_locked = 'Y' AND authentication_string = '' \
             FROM mysql.user WHERE User = ? AND Host = ?"
        }
        Flavour::MariaDb => {
            "SELECT JSON_VALUE(Priv, '$.plugin'), \
                    IFNULL(JSON_VALUE(Priv, '$.authentication_string'), '') <> '', \
                    IFNULL(JSON_VALUE(Priv, '$.account_locked'), 0) = 1, \
                    JSON_VALUE(Priv, '$.password_last_changed') = '0', \
                    JSON_VALUE(Priv, '$.password_lifetime'), \
                    JSON_VALUE(Priv, '$.max_user_connections'), \
                    JSON_VALUE(Priv, '$.max_connections'), \
                    JSON_VALUE(Priv, '$.max_questions'), \
                    JSON_VALUE(Priv, '$.max_updates'), NULL, NULL, \
                    IFNULL(JSON_VALUE(Priv, '$.is_role'), 0) = 1 \
             FROM mysql.global_priv WHERE User = ? AND Host = ?"
        }
    };
    let params = (name.user.as_str(), name.host.as_str());
    let Some(rows) = try_query(conn, sql, params).await? else {
        return Ok(None);
    };
    let Some(row) = rows.first() else {
        return Ok(Some(None));
    };
    let number = |index: usize| -> Option<i64> {
        row.get(index)
            .cloned()
            .flatten()
            .and_then(|text| text.parse().ok())
    };
    let limit = |index| {
        number(index)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0)
    };
    let role = flag(row, 11);
    Ok(Some(Some(Properties {
        kind: if role {
            AccountKind::Role
        } else {
            AccountKind::User
        },
        plugin: row.first().cloned().flatten().unwrap_or_default(),
        has_password: flag(row, 1),
        locked: flag(row, 2) && !role,
        expired: flag(row, 3),
        lifetime: lifetime(number(4)),
        failed_login: failed_login(number(9), number(10)),
        limits: AccountLimits {
            max_user_connections: limit(5),
            max_connections_per_hour: limit(6),
            max_queries_per_hour: limit(7),
            max_updates_per_hour: limit(8),
        },
    })))
}

/// `NULL` (or MariaDB's `-1`) is the server's default; `0` is never.
fn lifetime(days: Option<i64>) -> PasswordLifetime {
    match days {
        Some(0) => PasswordLifetime::Never,
        Some(days) => u16::try_from(days)
            .ok()
            .and_then(NonZeroU16::new)
            .map_or(PasswordLifetime::ServerDefault, PasswordLifetime::Days),
        None => PasswordLifetime::ServerDefault,
    }
}

fn failed_login(attempts: Option<i64>, days: Option<i64>) -> Option<FailedLoginLock> {
    let attempts = NonZeroU16::new(u16::try_from(attempts?).ok()?)?;
    let lock = match days? {
        UNBOUNDED_LOCK => LockTime::Unbounded,
        days => LockTime::Days(NonZeroU16::new(u16::try_from(days).ok()?)?),
    };
    Some(FailedLoginLock { attempts, lock })
}

async fn default_roles(
    conn: &mut Conn,
    flavour: Flavour,
    name: &AccountName,
) -> Result<BTreeSet<AccountName>, DriverError> {
    let sql = match flavour {
        Flavour::Mysql => {
            "SELECT DEFAULT_ROLE_USER, DEFAULT_ROLE_HOST FROM mysql.default_roles \
             WHERE USER = ? AND HOST = ?"
        }
        Flavour::MariaDb => {
            "SELECT JSON_VALUE(Priv, '$.default_role'), '' FROM mysql.global_priv \
             WHERE User = ? AND Host = ? AND IFNULL(JSON_VALUE(Priv, '$.default_role'), '') <> ''"
        }
    };
    let params = (name.user.as_str(), name.host.as_str());
    let rows = try_query(conn, sql, params).await?.unwrap_or_default();
    rows.iter()
        .map(|row| Ok(AccountName::new(field(row, 0)?, field(row, 1)?)))
        .collect()
}

/// Privileges on `*.*`, from `SHOW GRANTS`: MariaDB's `USER_PRIVILEGES` leaves roles out.
/// Only the part before ` ON *.* TO` is read, so a line holding MariaDB's password hash
/// (`GRANT USAGE … IDENTIFIED BY PASSWORD '…'`) is never kept.
async fn global_privileges(
    conn: &mut Conn,
    name: &AccountName,
) -> Result<PrivilegeSet, DriverError> {
    let sql = format!("SHOW GRANTS FOR {}", grantee(name));
    let Some(rows) = try_query(conn, &sql, ()).await? else {
        return Ok(PrivilegeSet::default());
    };
    let mut set = PrivilegeSet::default();
    for line in rows.iter().filter_map(|row| row.first().cloned().flatten()) {
        let Some(rest) = line.strip_prefix("GRANT ") else {
            continue;
        };
        let Some((privileges, after)) = rest.split_once(" ON *.* TO ") else {
            continue;
        };
        if after.contains(" WITH GRANT OPTION") {
            set.grant_option = true;
        }
        set.privileges.extend(
            privileges
                .split(", ")
                .map(Privilege::new)
                .filter(|privilege| privilege.as_str() != "USAGE"),
        );
    }
    // MySQL lists `GRANT OPTION` among an account's privileges when it was granted with them.
    set.privileges.remove(&Privilege::new("GRANT OPTION"));
    Ok(set)
}

/// The account as `SHOW GRANTS FOR` takes it: `` `user`@`host` ``, and `` `role` `` for a
/// MariaDB role, which has no host.
fn grantee(name: &AccountName) -> String {
    let quote = |part: &str| format!("`{}`", part.replace('`', "``"));
    if name.host.is_empty() {
        quote(&name.user)
    } else {
        format!("{}@{}", quote(&name.user), quote(&name.host))
    }
}

/// The `GRANTEE` column's text for `name`.
fn grantee_column(name: &AccountName) -> String {
    format!("'{}'@'{}'", name.user, name.host)
}

async fn database_privileges(
    conn: &mut Conn,
    name: &AccountName,
) -> Result<BTreeMap<String, PrivilegeSet>, DriverError> {
    let rows = try_query(
        conn,
        "SELECT TABLE_SCHEMA, PRIVILEGE_TYPE, IS_GRANTABLE = 'YES' \
         FROM information_schema.SCHEMA_PRIVILEGES WHERE GRANTEE = ?",
        (grantee_column(name),),
    )
    .await?
    .unwrap_or_default();
    let mut databases: BTreeMap<String, PrivilegeSet> = BTreeMap::new();
    for row in &rows {
        let set = databases.entry(field(row, 0)?).or_default();
        set.privileges.insert(Privilege::new(&field(row, 1)?));
        set.grant_option |= flag(row, 2);
    }
    Ok(databases)
}

async fn object_privileges(
    conn: &mut Conn,
    name: &AccountName,
) -> Result<BTreeSet<ObjectPrivilege>, DriverError> {
    let grantee = grantee_column(name);
    let tables = try_query(
        conn,
        "SELECT TABLE_SCHEMA, TABLE_NAME, PRIVILEGE_TYPE \
         FROM information_schema.TABLE_PRIVILEGES WHERE GRANTEE = ?",
        (grantee.clone(),),
    )
    .await?
    .unwrap_or_default();
    let columns = try_query(
        conn,
        "SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, PRIVILEGE_TYPE \
         FROM information_schema.COLUMN_PRIVILEGES WHERE GRANTEE = ?",
        (grantee,),
    )
    .await?
    .unwrap_or_default();
    // Routines have no `information_schema` view; `procs_priv` needs the `mysql` schema.
    let routines = try_query(
        conn,
        "SELECT Db, Routine_name, Routine_type, Proc_priv FROM mysql.procs_priv \
         WHERE User = ? AND Host = ?",
        (name.user.as_str(), name.host.as_str()),
    )
    .await?
    .unwrap_or_default();
    let mut objects = BTreeSet::new();
    for row in &tables {
        objects.insert(ObjectPrivilege {
            level: ObjectLevel::Table {
                database: field(row, 0)?,
                table: field(row, 1)?,
            },
            privilege: Privilege::new(&field(row, 2)?),
        });
    }
    for row in &columns {
        objects.insert(ObjectPrivilege {
            level: ObjectLevel::Column {
                database: field(row, 0)?,
                table: field(row, 1)?,
                column: field(row, 2)?,
            },
            privilege: Privilege::new(&field(row, 3)?),
        });
    }
    for row in &routines {
        let kind = if field(row, 2)?.eq_ignore_ascii_case("FUNCTION") {
            RoutineKind::Function
        } else {
            RoutineKind::Procedure
        };
        // `Proc_priv` is a SET: `Execute,Alter Routine`; `Grant` is the grant option.
        for privilege in field(row, 3)?
            .split(',')
            .filter(|p| !p.is_empty() && *p != "Grant")
        {
            objects.insert(ObjectPrivilege {
                level: ObjectLevel::Routine {
                    database: field(row, 0)?,
                    name: field(row, 1)?,
                    kind,
                },
                privilege: Privilege::new(privilege),
            });
        }
    }
    Ok(objects)
}

/// The views, routines, triggers and events whose definer `name` is, which fail once it is
/// dropped. `DEFINER` is written `user@host`, unquoted.
async fn defined_objects(
    conn: &mut Conn,
    name: &AccountName,
) -> Result<Vec<DefinedObject>, DriverError> {
    let definer = format!("{}@{}", name.user, name.host);
    let sql = "SELECT 'view', TABLE_SCHEMA, TABLE_NAME FROM information_schema.VIEWS \
               WHERE DEFINER = ? \
               UNION ALL SELECT LOWER(ROUTINE_TYPE), ROUTINE_SCHEMA, ROUTINE_NAME \
               FROM information_schema.ROUTINES WHERE DEFINER = ? \
               UNION ALL SELECT 'trigger', TRIGGER_SCHEMA, TRIGGER_NAME \
               FROM information_schema.TRIGGERS WHERE DEFINER = ? \
               UNION ALL SELECT 'event', EVENT_SCHEMA, EVENT_NAME \
               FROM information_schema.EVENTS WHERE DEFINER = ? \
               ORDER BY 2, 3";
    let params = (
        definer.as_str(),
        definer.as_str(),
        definer.as_str(),
        definer.as_str(),
    );
    let rows = try_query(conn, sql, params).await?.unwrap_or_default();
    rows.iter()
        .map(|row| {
            let kind = match field(row, 0)?.as_str() {
                "view" => DefinedKind::View,
                "function" => DefinedKind::Function,
                "trigger" => DefinedKind::Trigger,
                "event" => DefinedKind::Event,
                _ => DefinedKind::Procedure,
            };
            Ok(DefinedObject {
                kind,
                database: field(row, 1)?,
                name: field(row, 2)?,
            })
        })
        .collect()
}

/// What the server offers and this login may do.
pub(crate) async fn user_management(
    conn: &mut Conn,
    flavour: Flavour,
    version: &str,
) -> Result<UserManagement, DriverError> {
    let current = current_account(conn).await?;
    let own: Vec<String> = conn
        .exec(
            "SELECT PRIVILEGE_TYPE FROM information_schema.USER_PRIVILEGES WHERE GRANTEE = ?",
            (grantee_column(&current),),
        )
        .await
        .map_err(connection_error)?;
    let creates_users = own.iter().any(|privilege| privilege == "CREATE USER");
    let reads_accounts = try_query(conn, "SELECT 1 FROM mysql.user LIMIT 1", ())
        .await?
        .is_some();
    let plugins: Vec<String> = conn
        .query(
            "SELECT PLUGIN_NAME FROM information_schema.PLUGINS \
             WHERE PLUGIN_TYPE = 'AUTHENTICATION' AND PLUGIN_STATUS = 'ACTIVE' ORDER BY 1",
        )
        .await
        .map_err(connection_error)?;
    let checks: Vec<String> = conn
        .query(
            "SELECT PLUGIN_NAME FROM information_schema.PLUGINS \
             WHERE PLUGIN_TYPE IN ('PASSWORD VALIDATION', 'VALIDATE PASSWORD') \
               AND PLUGIN_STATUS = 'ACTIVE'",
        )
        .await
        .map_err(connection_error)?;
    // MySQL 8's validate_password is a component, which PLUGINS doesn't list; its variables
    // say it is loaded.
    let validate_password: Vec<(String, String)> = conn
        .query("SHOW VARIABLES LIKE 'validate\\_password%'")
        .await
        .map_err(connection_error)?;
    let cipher: Option<(String, String)> = conn
        .query_first("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
        .await
        .map_err(connection_error)?;
    let permit = |allowed: bool, needs: &str| {
        if allowed {
            Permit::Allowed
        } else {
            Permit::Needs(needs.to_owned())
        }
    };
    Ok(UserManagement {
        current: current.label(),
        encrypted: Some(cipher.is_some_and(|(_, cipher)| !cipher.is_empty())),
        may_list: permit(reads_accounts || creates_users, LISTS_ACCOUNTS),
        may_change: permit(creates_users, CHANGES_ACCOUNTS),
        server: ManagedUsers::Accounts(AccountServer {
            flavor: flavour.into(),
            version: version.to_owned(),
            plugins,
            password_checks: !checks.is_empty() || !validate_password.is_empty(),
            failed_login_lock: flavour == Flavour::Mysql
                && version_at_least(version, FAILED_LOGIN_LOCK_SINCE),
        }),
    })
}

/// The account this login is, as `CURRENT_USER()` names it: `user@host`, the host after
/// the last `@`.
async fn current_account(conn: &mut Conn) -> Result<AccountName, DriverError> {
    let current: Option<String> = conn
        .query_first("SELECT CURRENT_USER()")
        .await
        .map_err(connection_error)?;
    Ok(AccountName::from_label(&current.unwrap_or_default()))
}

/// The rows of `sql`, as text, or `None` when this login may not read what it asks.
async fn try_query(
    conn: &mut Conn,
    sql: &str,
    params: impl Into<Params> + Send,
) -> Result<Option<Vec<Vec<Option<String>>>>, DriverError> {
    let rows: Vec<mysql_async::Row> = match conn.exec(sql, params).await {
        Ok(rows) => rows,
        Err(mysql_async::Error::Server(error)) if UNAVAILABLE.contains(&error.code) => {
            return Ok(None);
        }
        Err(error) => return Err(connection_error(error)),
    };
    Ok(Some(
        rows.into_iter()
            .map(|row| {
                row.unwrap_raw()
                    .into_iter()
                    .map(|value| value.and_then(text))
                    .collect()
            })
            .collect(),
    ))
}

fn text(value: Value) -> Option<String> {
    match value {
        Value::NULL => None,
        Value::Bytes(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        other => Some(other.as_sql(true)),
    }
}

/// Column `index`, which the query never leaves `NULL`.
fn field(row: &[Option<String>], index: usize) -> Result<String, DriverError> {
    row.get(index)
        .cloned()
        .flatten()
        .ok_or_else(|| connection_error(format!("account row without column {index}")))
}

/// A `1`/`0` column, `NULL` read as false.
fn flag(row: &[Option<String>], index: usize) -> bool {
    row.get(index).cloned().flatten().as_deref() == Some("1")
}
