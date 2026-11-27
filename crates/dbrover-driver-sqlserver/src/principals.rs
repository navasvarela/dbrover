//! SQL Server's logins and server roles, and each database's users and roles: their folders,
//! one principal's description, and what the server and this login allow (W18,
//! `docs/users-beyond-postgres.md`).
//!
//! Logins and server roles come from the server's views, `sys.server_principals` and its
//! neighbours, which any database's connection can read. A database's users and roles come
//! from its own views, read through its name (`[shop].sys.database_principals`), as the
//! catalog reads its schemas.

use std::collections::{BTreeMap, BTreeSet};

use dbrover_core::{
    CatalogNode, CatalogPath, DatabaseUserDescription, DatabaseUserKind, DatabaseUserName,
    DriverError, FolderKind, LoginDescription, LoginKind, LoginServer, ManagedUsers, NodeKind,
    PermissionGrant, PermissionState, Permit, PrincipalDescription, PrincipalScope, Securable,
    SqlLoginState, SqlServerRoleDescription, UserManagement,
};
use tiberius::{Row, ToSql};

use crate::{SQL_SERVER_QUOTING, SqlClient, connection_error};

/// What lets a login see every other login.
const LISTS_LOGINS: &str = "VIEW ANY DEFINITION, or ALTER ANY LOGIN";
const CHANGES_LOGINS: &str = "ALTER ANY LOGIN";
const CHANGES_USERS: &str = "ALTER ANY USER";
/// The users every database has, which belong to the server.
const SYSTEM_USERS: [&str; 4] = ["dbo", "guest", "sys", "INFORMATION_SCHEMA"];
/// `sys.database_principals.authentication_type`.
const AUTHENTICATION_NONE: i32 = 0;
const AUTHENTICATION_INSTANCE: i32 = 1;
const AUTHENTICATION_DATABASE: i32 = 2;
const AUTHENTICATION_EXTERNAL: i32 = 4;
/// `sys.server_permissions.class` and `sys.database_permissions.class`.
const CLASS_DATABASE: u8 = 0;
const CLASS_OBJECT: u8 = 1;
const CLASS_SCHEMA: u8 = 3;
const CLASS_DATABASE_PRINCIPAL: u8 = 4;
const CLASS_SERVER: u8 = 100;
const CLASS_SERVER_PRINCIPAL: u8 = 101;
const CLASS_ENDPOINT: u8 = 105;

/// Where a principal folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrincipalFolder<'a> {
    Logins,
    ServerRoles,
    Users { catalog: &'a str },
    Roles { catalog: &'a str },
}

/// The principals in the folder at `path`.
pub(crate) async fn list(
    client: &mut SqlClient,
    path: &CatalogPath,
    folder: PrincipalFolder<'_>,
) -> Result<Vec<CatalogNode>, DriverError> {
    match folder {
        PrincipalFolder::Logins => list_logins(client, path).await,
        PrincipalFolder::ServerRoles => {
            let sql = "SELECT r.name, CAST(r.is_fixed_role AS int), \
                              (SELECT COUNT(*) FROM sys.server_role_members m \
                               WHERE m.role_principal_id = r.principal_id) \
                       FROM sys.server_principals r WHERE r.type = 'R' ORDER BY r.name";
            list_roles(client, path, sql).await
        }
        PrincipalFolder::Users { catalog } => list_users(client, path, catalog).await,
        PrincipalFolder::Roles { catalog } => {
            let sql = format!(
                "SELECT r.name, CAST(r.is_fixed_role AS int), \
                        (SELECT COUNT(*) FROM {catalog}.database_role_members m \
                         WHERE m.role_principal_id = r.principal_id) \
                 FROM {catalog}.database_principals r WHERE r.type = 'R' ORDER BY r.name"
            );
            list_roles(client, path, &sql).await
        }
    }
}

async fn list_logins(
    client: &mut SqlClient,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    let memberships = memberships(
        client,
        "SELECT m.name, r.name FROM sys.server_role_members rm \
         JOIN sys.server_principals r ON r.principal_id = rm.role_principal_id \
         JOIN sys.server_principals m ON m.principal_id = rm.member_principal_id",
    )
    .await?;
    let rows = query(
        client,
        "SELECT p.name, p.type, CAST(p.is_disabled AS int), \
                CAST(LOGINPROPERTY(p.name, 'IsLocked') AS int) \
         FROM sys.server_principals p WHERE p.type <> 'R' ORDER BY p.name",
        &[],
    )
    .await?;
    rows.iter()
        .map(|row| {
            let name = text(row, 0)?.unwrap_or_default();
            let kind = text(row, 1)?.as_deref().and_then(LoginKind::from_code);
            let mut parts: Vec<String> = kind
                .map(LoginKind::label)
                .map(str::to_owned)
                .into_iter()
                .collect();
            parts.extend(memberships.get(&name).into_iter().flatten().cloned());
            if number(row, 2)? == Some(1) {
                parts.push("disabled".to_owned());
            }
            if number(row, 3)? == Some(1) {
                parts.push("locked".to_owned());
            }
            let system = name.starts_with("##")
                || name.starts_with("NT SERVICE\\")
                || name.starts_with("NT AUTHORITY\\")
                || matches!(
                    kind,
                    Some(LoginKind::Certificate | LoginKind::AsymmetricKey)
                );
            Ok(node(
                path,
                NodeKind::Login,
                name,
                Some(parts.join(", ")),
                system,
            ))
        })
        .collect()
}

async fn list_roles(
    client: &mut SqlClient,
    path: &CatalogPath,
    sql: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let rows = query(client, sql, &[]).await?;
    rows.iter()
        .map(|row| {
            let name = text(row, 0)?.unwrap_or_default();
            let mut parts = Vec::new();
            if number(row, 1)? == Some(1) {
                parts.push("fixed".to_owned());
            }
            match number(row, 2)?.unwrap_or_default() {
                0 => {}
                1 => parts.push("1 member".to_owned()),
                n => parts.push(format!("{n} members")),
            }
            let detail = (!parts.is_empty()).then(|| parts.join(", "));
            let system = name == "public";
            Ok(node(path, NodeKind::Role, name, detail, system))
        })
        .collect()
}

async fn list_users(
    client: &mut SqlClient,
    path: &CatalogPath,
    catalog: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let memberships = memberships(
        client,
        &format!(
            "SELECT m.name, r.name FROM {catalog}.database_role_members rm \
             JOIN {catalog}.database_principals r ON r.principal_id = rm.role_principal_id \
             JOIN {catalog}.database_principals m ON m.principal_id = rm.member_principal_id"
        ),
    )
    .await?;
    let rows = query(
        client,
        &format!(
            "SELECT dp.name, dp.type, dp.authentication_type, sp.name \
             FROM {catalog}.database_principals dp \
             LEFT JOIN sys.server_principals sp ON sp.sid = dp.sid \
             WHERE dp.type NOT IN ('R', 'A') ORDER BY dp.name"
        ),
        &[],
    )
    .await?;
    rows.iter()
        .map(|row| {
            let name = text(row, 0)?.unwrap_or_default();
            let kind = user_kind(text(row, 1)?.as_deref(), number(row, 2)?, text(row, 3)?);
            let mut parts = vec![user_kind_label(&kind)];
            parts.extend(memberships.get(&name).into_iter().flatten().cloned());
            let system = SYSTEM_USERS.contains(&name.as_str())
                || name.starts_with("##")
                || kind == DatabaseUserKind::Key;
            Ok(node(
                path,
                NodeKind::User,
                name,
                Some(parts.join(", ")),
                system,
            ))
        })
        .collect()
}

/// What a database user is for, from its type code, how it authenticates and its login.
fn user_kind(
    code: Option<&str>,
    authentication: Option<i32>,
    login: Option<String>,
) -> DatabaseUserKind {
    if matches!(code, Some("C" | "K")) {
        return DatabaseUserKind::Key;
    }
    match (authentication, login) {
        (_, Some(login)) => DatabaseUserKind::Login(login),
        (Some(AUTHENTICATION_INSTANCE), None) => DatabaseUserKind::Orphaned,
        (Some(AUTHENTICATION_DATABASE), None) => DatabaseUserKind::Contained,
        (Some(AUTHENTICATION_EXTERNAL), None) => DatabaseUserKind::External,
        (Some(AUTHENTICATION_NONE) | None, None) => DatabaseUserKind::WithoutLogin,
        // A Windows user whose login isn't on this server.
        (Some(_), None) => DatabaseUserKind::Orphaned,
    }
}

fn user_kind_label(kind: &DatabaseUserKind) -> String {
    match kind {
        DatabaseUserKind::Login(login) => format!("for login {login}"),
        DatabaseUserKind::Orphaned => "no login (orphaned)".to_owned(),
        DatabaseUserKind::WithoutLogin => "without login".to_owned(),
        DatabaseUserKind::Contained => "contained".to_owned(),
        DatabaseUserKind::External => "Microsoft Entra".to_owned(),
        DatabaseUserKind::Key => "certificate or key".to_owned(),
    }
}

/// Each member's roles, by the member's name, from a query answering member and role.
async fn memberships(
    client: &mut SqlClient,
    sql: &str,
) -> Result<BTreeMap<String, Vec<String>>, DriverError> {
    let mut roles: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in &query(client, sql, &[]).await? {
        if let (Some(member), Some(role)) = (text(row, 0)?, text(row, 1)?) {
            roles.entry(member).or_default().push(role);
        }
    }
    for list in roles.values_mut() {
        list.sort();
    }
    Ok(roles)
}

fn node(
    parent: &CatalogPath,
    kind: NodeKind,
    name: String,
    detail: Option<String>,
    system: bool,
) -> CatalogNode {
    CatalogNode {
        path: parent.child(kind, name.clone()),
        label: name,
        detail: detail.filter(|detail| !detail.is_empty()),
        has_children: false,
        has_definition: false,
        system,
    }
}

/// The principal at `path`: a login or server role at the top, or a user or role in a
/// database, whose catalog views are `catalog`'s.
pub(crate) async fn describe(
    client: &mut SqlClient,
    path: &CatalogPath,
    catalog_of: impl Fn(&str) -> String,
) -> Result<PrincipalDescription, DriverError> {
    let invalid = || DriverError::InvalidPath(path.clone());
    let folder = |segment: &dbrover_core::PathSegment| match segment.kind {
        NodeKind::Folder(folder) => Some(folder),
        _ => None,
    };
    match path.segments() {
        [logins, login]
            if folder(logins) == Some(FolderKind::Logins) && login.kind == NodeKind::Login =>
        {
            describe_login(client, &login.name)
                .await?
                .map(PrincipalDescription::Login)
                .ok_or_else(invalid)
        }
        [roles, role]
            if folder(roles) == Some(FolderKind::ServerRoles) && role.kind == NodeKind::Role =>
        {
            describe_role(client, PrincipalScope::Server, "sys", &role.name)
                .await?
                .map(PrincipalDescription::SqlServerRole)
                .ok_or_else(invalid)
        }
        [database, users, user]
            if database.kind == NodeKind::Database
                && folder(users) == Some(FolderKind::Users)
                && user.kind == NodeKind::User =>
        {
            let catalog = catalog_of(&database.name);
            describe_user(client, &database.name, &catalog, &user.name)
                .await?
                .map(PrincipalDescription::DatabaseUser)
                .ok_or_else(invalid)
        }
        [database, roles, role]
            if database.kind == NodeKind::Database
                && folder(roles) == Some(FolderKind::Roles)
                && role.kind == NodeKind::Role =>
        {
            let catalog = catalog_of(&database.name);
            let scope = PrincipalScope::Database(database.name.clone());
            describe_role(client, scope, &catalog, &role.name)
                .await?
                .map(PrincipalDescription::SqlServerRole)
                .ok_or_else(invalid)
        }
        _ => Err(invalid()),
    }
}

async fn describe_login(
    client: &mut SqlClient,
    name: &str,
) -> Result<Option<LoginDescription>, DriverError> {
    let rows = query(
        client,
        "SELECT p.type, CAST(p.is_disabled AS int), p.default_database_name, \
                p.default_language_name, CAST(l.is_policy_checked AS int), \
                CAST(l.is_expiration_checked AS int), \
                CAST(LOGINPROPERTY(p.name, 'IsLocked') AS int), \
                CAST(LOGINPROPERTY(p.name, 'IsExpired') AS int), \
                CAST(LOGINPROPERTY(p.name, 'IsMustChange') AS int), \
                CAST(LOGINPROPERTY(p.name, 'BadPasswordCount') AS int), \
                CAST(LOGINPROPERTY(p.name, 'DaysUntilExpiration') AS int), p.principal_id \
         FROM sys.server_principals p \
         LEFT JOIN sys.sql_logins l ON l.principal_id = p.principal_id \
         WHERE p.name = @P1 AND p.type <> 'R'",
        &[&name],
    )
    .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let Some(kind) = text(row, 0)?.as_deref().and_then(LoginKind::from_code) else {
        return Ok(None);
    };
    let principal_id = number(row, 11)?.unwrap_or_default();
    let on = |index| -> Result<bool, DriverError> { Ok(number(row, index)? == Some(1)) };
    let sql = (kind == LoginKind::Sql)
        .then(|| -> Result<SqlLoginState, DriverError> {
            Ok(SqlLoginState {
                check_policy: on(4)?,
                check_expiration: on(5)?,
                locked: on(6)?,
                expired: on(7)?,
                must_change: on(8)?,
                bad_password_count: number(row, 9)?
                    .and_then(|count| u32::try_from(count).ok())
                    .unwrap_or_default(),
                days_until_expiration: number(row, 10)?,
            })
        })
        .transpose()?;
    let disabled = on(1)?;
    let default_database = text(row, 2)?.unwrap_or_else(|| "master".to_owned());
    let default_language = text(row, 3)?;
    let server_roles = names(
        client,
        "SELECT r.name FROM sys.server_role_members m \
         JOIN sys.server_principals r ON r.principal_id = m.role_principal_id \
         WHERE m.member_principal_id = @P1",
        &[&principal_id],
    )
    .await?;
    let permissions = server_permissions(client, principal_id).await?;
    let owned_databases = names(
        client,
        "SELECT d.name FROM sys.databases d JOIN sys.server_principals p ON p.sid = d.owner_sid \
         WHERE p.principal_id = @P1 ORDER BY d.name",
        &[&principal_id],
    )
    .await?;
    let users = users_of_login(client, name).await?;
    Ok(Some(LoginDescription {
        name: name.to_owned(),
        kind,
        disabled,
        default_database,
        default_language,
        sql,
        server_roles: server_roles.into_iter().collect(),
        permissions,
        users,
        owned_databases,
    }))
}

/// The users made for the login `name`, in each database the reading login may open.
async fn users_of_login(
    client: &mut SqlClient,
    name: &str,
) -> Result<BTreeSet<DatabaseUserName>, DriverError> {
    let databases = names(
        client,
        "SELECT name FROM sys.databases WHERE state = 0 AND HAS_DBACCESS(name) = 1 ORDER BY name",
        &[],
    )
    .await?;
    let mut users = BTreeSet::new();
    for database in databases {
        let catalog = format!("{}.sys", SQL_SERVER_QUOTING.identifier(&database));
        let sql = format!(
            "SELECT dp.name FROM {catalog}.database_principals dp \
             JOIN sys.server_principals sp ON sp.sid = dp.sid WHERE sp.name = @P1"
        );
        for user in names(client, &sql, &[&name]).await? {
            users.insert(DatabaseUserName {
                database: database.clone(),
                user,
            });
        }
    }
    Ok(users)
}

async fn server_permissions(
    client: &mut SqlClient,
    principal_id: i32,
) -> Result<BTreeSet<PermissionGrant>, DriverError> {
    let rows = query(
        client,
        "SELECT p.permission_name, p.state, CAST(p.class AS int), sp.name, sp.type, e.name \
         FROM sys.server_permissions p \
         LEFT JOIN sys.server_principals sp \
                ON p.class = 101 AND sp.principal_id = p.major_id \
         LEFT JOIN sys.endpoints e ON p.class = 105 AND e.endpoint_id = p.major_id \
         WHERE p.grantee_principal_id = @P1",
        &[&principal_id],
    )
    .await?;
    let mut grants = BTreeSet::new();
    for row in &rows {
        let class = class_of(row, 2)?;
        let securable = match class {
            CLASS_SERVER => Securable::Server,
            CLASS_SERVER_PRINCIPAL => Securable::Other {
                class: if text(row, 4)?.as_deref() == Some("R") {
                    "SERVER ROLE".to_owned()
                } else {
                    "LOGIN".to_owned()
                },
                name: text(row, 3)?.unwrap_or_default(),
            },
            CLASS_ENDPOINT => Securable::Other {
                class: "ENDPOINT".to_owned(),
                name: text(row, 5)?.unwrap_or_default(),
            },
            _ => continue,
        };
        if let Some(grant) = grant(row, securable)? {
            grants.insert(grant);
        }
    }
    Ok(grants)
}

async fn describe_user(
    client: &mut SqlClient,
    database: &str,
    catalog: &str,
    name: &str,
) -> Result<Option<DatabaseUserDescription>, DriverError> {
    let rows = query(
        client,
        &format!(
            "SELECT dp.type, dp.authentication_type, sp.name, dp.default_schema_name, \
                    dp.principal_id \
             FROM {catalog}.database_principals dp \
             LEFT JOIN sys.server_principals sp ON sp.sid = dp.sid \
             WHERE dp.name = @P1 AND dp.type NOT IN ('R', 'A')"
        ),
        &[&name],
    )
    .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let kind = user_kind(text(row, 0)?.as_deref(), number(row, 1)?, text(row, 2)?);
    let default_schema = text(row, 3)?;
    let principal_id = number(row, 4)?.unwrap_or_default();
    let roles = names(
        client,
        &format!(
            "SELECT r.name FROM {catalog}.database_role_members m \
             JOIN {catalog}.database_principals r ON r.principal_id = m.role_principal_id \
             WHERE m.member_principal_id = @P1"
        ),
        &[&principal_id],
    )
    .await?;
    let owned_schemas = names(
        client,
        &format!("SELECT name FROM {catalog}.schemas WHERE principal_id = @P1 ORDER BY name"),
        &[&principal_id],
    )
    .await?;
    let owned_roles = names(
        client,
        &format!(
            "SELECT name FROM {catalog}.database_principals \
             WHERE type = 'R' AND owning_principal_id = @P1 ORDER BY name"
        ),
        &[&principal_id],
    )
    .await?;
    Ok(Some(DatabaseUserDescription {
        database: database.to_owned(),
        name: name.to_owned(),
        kind,
        default_schema,
        roles: roles.into_iter().collect(),
        permissions: database_permissions(client, catalog, principal_id).await?,
        owned_schemas,
        owned_roles,
    }))
}

async fn database_permissions(
    client: &mut SqlClient,
    catalog: &str,
    principal_id: i32,
) -> Result<BTreeSet<PermissionGrant>, DriverError> {
    let rows = query(
        client,
        &format!(
            "SELECT p.permission_name, p.state, CAST(p.class AS int), s.name, os.name, o.name, \
                    c.name, dp.name, dp.type \
             FROM {catalog}.database_permissions p \
             LEFT JOIN {catalog}.schemas s ON p.class = 3 AND s.schema_id = p.major_id \
             LEFT JOIN {catalog}.objects o ON p.class = 1 AND o.object_id = p.major_id \
             LEFT JOIN {catalog}.schemas os ON os.schema_id = o.schema_id \
             LEFT JOIN {catalog}.columns c \
                    ON p.class = 1 AND p.minor_id <> 0 \
                   AND c.object_id = p.major_id AND c.column_id = p.minor_id \
             LEFT JOIN {catalog}.database_principals dp \
                    ON p.class = 4 AND dp.principal_id = p.major_id \
             WHERE p.grantee_principal_id = @P1"
        ),
        &[&principal_id],
    )
    .await?;
    let mut grants = BTreeSet::new();
    for row in &rows {
        let securable = match class_of(row, 2)? {
            CLASS_DATABASE => Securable::Database,
            CLASS_SCHEMA => Securable::Schema(text(row, 3)?.unwrap_or_default()),
            CLASS_OBJECT => {
                let schema = text(row, 4)?.unwrap_or_default();
                let object = text(row, 5)?.unwrap_or_default();
                match text(row, 6)? {
                    Some(column) => Securable::Column {
                        schema,
                        object,
                        column,
                    },
                    None => Securable::Object {
                        schema,
                        name: object,
                    },
                }
            }
            CLASS_DATABASE_PRINCIPAL => Securable::Other {
                class: match text(row, 8)?.as_deref() {
                    Some("R") => "ROLE".to_owned(),
                    Some("A") => "APPLICATION ROLE".to_owned(),
                    _ => "USER".to_owned(),
                },
                name: text(row, 7)?.unwrap_or_default(),
            },
            // Types, XML schema collections, assemblies and the rest: rare, and left to a
            // console.
            _ => continue,
        };
        if let Some(grant) = grant(row, securable)? {
            grants.insert(grant);
        }
    }
    Ok(grants)
}

/// A permission row's grant: its name in column 0 and its state in column 1.
fn grant(row: &Row, securable: Securable) -> Result<Option<PermissionGrant>, DriverError> {
    let Some(state) = text(row, 1)?
        .as_deref()
        .and_then(PermissionState::from_code)
    else {
        return Ok(None);
    };
    Ok(Some(PermissionGrant {
        permission: text(row, 0)?.unwrap_or_default(),
        securable,
        state,
    }))
}

fn class_of(row: &Row, index: usize) -> Result<u8, DriverError> {
    Ok(number(row, index)?
        .and_then(|class| u8::try_from(class).ok())
        .unwrap_or(u8::MAX))
}

async fn describe_role(
    client: &mut SqlClient,
    scope: PrincipalScope,
    catalog: &str,
    name: &str,
) -> Result<Option<SqlServerRoleDescription>, DriverError> {
    let (principals, members) = match scope {
        PrincipalScope::Server => ("server_principals", "server_role_members"),
        PrincipalScope::Database(_) => ("database_principals", "database_role_members"),
    };
    let rows = query(
        client,
        &format!(
            "SELECT CAST(r.is_fixed_role AS int), o.name, r.principal_id \
             FROM {catalog}.{principals} r \
             LEFT JOIN {catalog}.{principals} o ON o.principal_id = r.owning_principal_id \
             WHERE r.type = 'R' AND r.name = @P1"
        ),
        &[&name],
    )
    .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let fixed = number(row, 0)? == Some(1);
    let owner = text(row, 1)?;
    let principal_id = number(row, 2)?.unwrap_or_default();
    let members = names(
        client,
        &format!(
            "SELECT m.name FROM {catalog}.{members} rm \
             JOIN {catalog}.{principals} m ON m.principal_id = rm.member_principal_id \
             WHERE rm.role_principal_id = @P1"
        ),
        &[&principal_id],
    )
    .await?;
    Ok(Some(SqlServerRoleDescription {
        scope,
        name: name.to_owned(),
        fixed,
        owner,
        members: members.into_iter().collect(),
    }))
}

/// What the server offers and this login may do, in the database it is connected to.
pub(crate) async fn user_management(client: &mut SqlClient) -> Result<UserManagement, DriverError> {
    let rows = query(
        client,
        "SELECT SUSER_SNAME(), HAS_PERMS_BY_NAME(NULL, NULL, 'VIEW ANY DEFINITION'), \
                HAS_PERMS_BY_NAME(NULL, NULL, 'ALTER ANY LOGIN'), DB_NAME(), \
                CAST((SELECT containment FROM sys.databases WHERE name = DB_NAME()) AS int), \
                HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'ALTER ANY USER')",
        &[],
    )
    .await?;
    let row = rows
        .first()
        .ok_or_else(|| connection_error("no answer about this login"))?;
    let permit = |allowed: bool, needs: &str| {
        if allowed {
            Permit::Allowed
        } else {
            Permit::Needs(needs.to_owned())
        }
    };
    let changes_logins = number(row, 2)? == Some(1);
    Ok(UserManagement {
        current: text(row, 0)?.unwrap_or_default(),
        encrypted: encrypted(client).await,
        may_list: permit(number(row, 1)? == Some(1) || changes_logins, LISTS_LOGINS),
        may_change: permit(changes_logins, CHANGES_LOGINS),
        server: ManagedUsers::Logins(LoginServer {
            database: text(row, 3)?.unwrap_or_default(),
            contained: number(row, 4)?.unwrap_or_default() > 0,
            may_change_users: permit(number(row, 5)? == Some(1), CHANGES_USERS),
        }),
    })
}

/// Whether this session is encrypted, from `sys.dm_exec_connections`, which a login without
/// `VIEW SERVER STATE` may not read even for its own session (tried on SQL Server 2022);
/// `CONNECTIONPROPERTY` has no such property.
async fn encrypted(client: &mut SqlClient) -> Option<bool> {
    let rows = query(
        client,
        "SELECT encrypt_option FROM sys.dm_exec_connections WHERE session_id = @@SPID",
        &[],
    )
    .await
    .ok()?;
    let option = text(rows.first()?, 0).ok()??;
    Some(option.eq_ignore_ascii_case("TRUE"))
}

/// The first column of each row of `sql`, as names.
async fn names(
    client: &mut SqlClient,
    sql: &str,
    params: &[&dyn ToSql],
) -> Result<Vec<String>, DriverError> {
    let rows = query(client, sql, params).await?;
    let mut names = Vec::with_capacity(rows.len());
    for row in &rows {
        if let Some(name) = text(row, 0)? {
            names.push(name);
        }
    }
    Ok(names)
}

async fn query(
    client: &mut SqlClient,
    sql: &str,
    params: &[&dyn ToSql],
) -> Result<Vec<Row>, DriverError> {
    client
        .query(sql, params)
        .await
        .map_err(connection_error)?
        .into_first_result()
        .await
        .map_err(connection_error)
}

fn text(row: &Row, index: usize) -> Result<Option<String>, DriverError> {
    let value: Option<&str> = row.try_get(index).map_err(connection_error)?;
    Ok(value.map(str::to_owned))
}

fn number(row: &Row, index: usize) -> Result<Option<i32>, DriverError> {
    row.try_get(index).map_err(connection_error)
}
