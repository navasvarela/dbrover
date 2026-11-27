//! SQL Server's logins, server roles, database users and database roles (W18,
//! `docs/users-beyond-postgres.md`).
//!
//! A statement that belongs to a database other than the server (`CREATE USER`, `ALTER ROLE`)
//! is written `EXEC [db].sys.sp_executesql N'…'`, which runs it in that database without
//! changing the session's own: the explorer's connection, which applies the helpers' DDL,
//! must stay in the database it was opened on, so `USE [db]` is not an option.

use std::collections::BTreeSet;

use dbrover_core::{
    DatabaseUserDescription, DatabaseUserName, LoginDescription, Password, PermissionGrant,
    PermissionState, PrincipalScope, Securable,
};

use super::{DdlError, require_name};
use crate::dialect::UserModel;
use crate::{Dialect, SqlServerHash};

/// A SQL login's password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlServerPassword {
    /// Hashed by DbRover (`… HASHED`), so the password itself is never sent.
    Hashed(SqlServerHash),
    /// Sent as typed, so the server's password policy can check it, or `MUST_CHANGE` can be
    /// set, which SQL Server refuses with a hash (question 5 of the plan).
    AsTyped(Password),
}

/// Windows's password policy, as a SQL login follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginPolicy {
    /// `CHECK_POLICY`, on by default.
    pub check_policy: bool,
    /// `CHECK_EXPIRATION`, off by default.
    pub check_expiration: bool,
}

impl Default for LoginPolicy {
    fn default() -> Self {
        Self {
            check_policy: true,
            check_expiration: false,
        }
    }
}

/// How a new login proves who it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginSource {
    Sql {
        password: SqlServerPassword,
        /// Must change its password at the next login: only with a password as typed, and
        /// with the policy and expiration checked.
        must_change: bool,
    },
    /// A Windows account or group, `DOMAIN\name`.
    Windows,
    /// A Microsoft Entra user, group or application.
    External,
}

/// What the server's default database is when a login names none.
const DEFAULT_DATABASE: &str = "master";

/// A login's settings apart from its name and password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginSpec {
    pub default_database: String,
    /// `None` for the server's default language.
    pub default_language: Option<String>,
    pub enabled: bool,
    /// For a SQL login only.
    pub policy: Option<LoginPolicy>,
    pub server_roles: BTreeSet<String>,
}

impl LoginSpec {
    pub fn new(policy: Option<LoginPolicy>) -> Self {
        Self {
            default_database: DEFAULT_DATABASE.to_owned(),
            default_language: None,
            enabled: true,
            policy,
            server_roles: BTreeSet::new(),
        }
    }
}

impl From<&LoginDescription> for LoginSpec {
    fn from(login: &LoginDescription) -> Self {
        Self {
            default_database: login.default_database.clone(),
            default_language: login.default_language.clone(),
            enabled: !login.disabled,
            policy: login.sql.map(|state| LoginPolicy {
                check_policy: state.check_policy,
                check_expiration: state.check_expiration,
            }),
            server_roles: login.server_roles.clone(),
        }
    }
}

/// A database user's settings apart from its name and what it is for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DatabaseUserSpec {
    pub default_schema: Option<String>,
    pub roles: BTreeSet<String>,
}

impl From<&DatabaseUserDescription> for DatabaseUserSpec {
    fn from(user: &DatabaseUserDescription) -> Self {
        Self {
            default_schema: user.default_schema.clone(),
            roles: user.roles.clone(),
        }
    }
}

/// A user for a new login in one database, as SSMS's user mapping makes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMapping {
    pub database: String,
    pub user: String,
    pub spec: DatabaseUserSpec,
}

/// What a new database user is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserSource {
    Login(String),
    /// For signing modules and impersonation.
    WithoutLogin,
    /// A contained database's user with a password of its own, which has no hashed form and
    /// so is sent as typed.
    Password(Password),
    /// A Microsoft Entra user or group.
    External,
}

/// A change to SQL Server's principals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlServerOperation {
    CreateLogin {
        name: String,
        source: LoginSource,
        spec: LoginSpec,
        mappings: Vec<UserMapping>,
    },
    AlterLogin {
        name: String,
        before: LoginSpec,
        after: LoginSpec,
        password: Option<SqlServerPassword>,
        must_change: bool,
        /// Clears a lock after failed logins, which SQL Server does only with a new password.
        unlock: bool,
    },
    RenameLogin {
        name: String,
        new_name: String,
    },
    SetLoginEnabled {
        name: String,
        enabled: bool,
    },
    /// Drops `users` first, which `DROP LOGIN` would leave orphaned.
    DropLogin {
        name: String,
        users: Vec<DatabaseUserName>,
    },
    CreateRole {
        scope: PrincipalScope,
        name: String,
        owner: Option<String>,
        members: BTreeSet<String>,
    },
    AlterRoleMembers {
        scope: PrincipalScope,
        name: String,
        before: BTreeSet<String>,
        after: BTreeSet<String>,
    },
    RenameRole {
        scope: PrincipalScope,
        name: String,
        new_name: String,
    },
    DropRole {
        scope: PrincipalScope,
        name: String,
    },
    CreateUser {
        database: String,
        name: String,
        source: UserSource,
        spec: DatabaseUserSpec,
    },
    AlterUser {
        database: String,
        name: String,
        before: DatabaseUserSpec,
        after: DatabaseUserSpec,
        /// Maps the user to this login: how an orphaned user gets one again.
        login: Option<String>,
        /// A contained user's new password.
        password: Option<Password>,
    },
    RenameUser {
        database: String,
        name: String,
        new_name: String,
    },
    /// Gives what the user owns to `give_to` first, where set: `DROP USER` refuses a user
    /// that owns a schema or a role.
    DropUser {
        database: String,
        name: String,
        owned_schemas: Vec<String>,
        owned_roles: Vec<String>,
        give_to: Option<String>,
    },
    /// Takes a grant or a deny away.
    Revoke {
        scope: PrincipalScope,
        principal: String,
        grant: PermissionGrant,
    },
}

pub(super) fn statements(
    operation: &SqlServerOperation,
    dialect: Dialect,
) -> Result<Vec<String>, DdlError> {
    if dialect.capabilities().users != UserModel::LoginsAndUsers {
        return Err(DdlError::Unsupported {
            what: "logins DbRover can create or change",
            dialect,
        });
    }
    match operation {
        SqlServerOperation::CreateLogin {
            name,
            source,
            spec,
            mappings,
        } => create_login(name, source, spec, mappings),
        SqlServerOperation::AlterLogin {
            name,
            before,
            after,
            password,
            must_change,
            unlock,
        } => alter_login(
            name,
            before,
            after,
            &LoginPassword {
                password: password.as_ref(),
                must_change: *must_change,
                unlock: *unlock,
            },
        ),
        SqlServerOperation::RenameLogin { name, new_name } => {
            require_name(name, "login")?;
            require_name(new_name, "login")?;
            Ok(vec![format!(
                "ALTER LOGIN {} WITH NAME = {}",
                bracket(name),
                bracket(new_name)
            )])
        }
        SqlServerOperation::SetLoginEnabled { name, enabled } => {
            require_name(name, "login")?;
            Ok(vec![enable(name, *enabled)])
        }
        SqlServerOperation::DropLogin { name, users } => {
            require_name(name, "login")?;
            let mut statements: Vec<String> = users
                .iter()
                .map(|user| {
                    in_database(
                        &user.database,
                        &format!("DROP USER {}", bracket(&user.user)),
                    )
                })
                .collect();
            statements.push(format!("DROP LOGIN {}", bracket(name)));
            Ok(statements)
        }
        SqlServerOperation::CreateRole {
            scope,
            name,
            owner,
            members,
        } => {
            require_name(name, "role")?;
            let mut create = format!("CREATE {} {}", role_keyword(scope), bracket(name));
            if let Some(owner) = owner.as_deref().filter(|owner| !owner.trim().is_empty()) {
                create.push_str(&format!(" AUTHORIZATION {}", bracket(owner)));
            }
            let mut statements = vec![scoped(scope, &create)];
            statements.extend(member_changes(scope, name, &BTreeSet::new(), members));
            Ok(statements)
        }
        SqlServerOperation::AlterRoleMembers {
            scope,
            name,
            before,
            after,
        } => {
            require_name(name, "role")?;
            nonempty(member_changes(scope, name, before, after))
        }
        SqlServerOperation::RenameRole {
            scope,
            name,
            new_name,
        } => {
            require_name(name, "role")?;
            require_name(new_name, "role")?;
            Ok(vec![scoped(
                scope,
                &format!(
                    "ALTER {} {} WITH NAME = {}",
                    role_keyword(scope),
                    bracket(name),
                    bracket(new_name)
                ),
            )])
        }
        SqlServerOperation::DropRole { scope, name } => {
            require_name(name, "role")?;
            Ok(vec![scoped(
                scope,
                &format!("DROP {} {}", role_keyword(scope), bracket(name)),
            )])
        }
        SqlServerOperation::CreateUser {
            database,
            name,
            source,
            spec,
        } => create_user(database, name, source, spec),
        SqlServerOperation::AlterUser {
            database,
            name,
            before,
            after,
            login,
            password,
        } => alter_user(
            &UserAt { database, name },
            before,
            after,
            login.as_deref(),
            password.as_ref(),
        ),
        SqlServerOperation::RenameUser {
            database,
            name,
            new_name,
        } => {
            require_name(name, "user")?;
            require_name(new_name, "user")?;
            Ok(vec![in_database(
                database,
                &format!(
                    "ALTER USER {} WITH NAME = {}",
                    bracket(name),
                    bracket(new_name)
                ),
            )])
        }
        SqlServerOperation::DropUser {
            database,
            name,
            owned_schemas,
            owned_roles,
            give_to,
        } => drop_user(
            &UserAt { database, name },
            owned_schemas,
            owned_roles,
            give_to.as_deref(),
        ),
        SqlServerOperation::Revoke {
            scope,
            principal,
            grant,
        } => {
            require_name(principal, "principal")?;
            Ok(vec![scoped(scope, &revoke(principal, grant))])
        }
    }
}

fn create_login(
    name: &str,
    source: &LoginSource,
    spec: &LoginSpec,
    mappings: &[UserMapping],
) -> Result<Vec<String>, DdlError> {
    require_name(name, "login")?;
    let login = bracket(name);
    let mut options = Vec::new();
    let create = match source {
        LoginSource::Sql {
            password,
            must_change,
        } => {
            let policy = spec.policy.unwrap_or_default();
            if *must_change {
                must_change_allowed(password, policy)?;
            }
            // Last in its clause: see `password_clause`.
            options.push(password_clause(password, *must_change));
            options.extend(database_options(None, spec));
            options.extend(policy_options(LoginPolicy::default(), policy));
            format!("CREATE LOGIN {login} WITH")
        }
        LoginSource::Windows => {
            options.extend(database_options(None, spec));
            format!("CREATE LOGIN {login} FROM WINDOWS")
        }
        LoginSource::External => format!("CREATE LOGIN {login} FROM EXTERNAL PROVIDER"),
    };
    let mut statements = vec![match (source, options.is_empty()) {
        (_, true) => create,
        (LoginSource::Sql { .. }, false) => format!("{create} {}", options.join(", ")),
        (_, false) => format!("{create} WITH {}", options.join(", ")),
    }];
    if !spec.enabled {
        statements.push(enable(name, false));
    }
    for role in &spec.server_roles {
        statements.push(add_member(&PrincipalScope::Server, role, name));
    }
    for mapping in mappings {
        statements.extend(create_user(
            &mapping.database,
            &mapping.user,
            &UserSource::Login(name.to_owned()),
            &mapping.spec,
        )?);
    }
    Ok(statements)
}

/// What a login's password is changed to, and what comes with the change.
struct LoginPassword<'a> {
    password: Option<&'a SqlServerPassword>,
    must_change: bool,
    unlock: bool,
}

/// The policy and defaults first, since `MUST_CHANGE` needs the policy on; then the
/// password; then enabling; then the server roles.
fn alter_login(
    name: &str,
    before: &LoginSpec,
    after: &LoginSpec,
    change: &LoginPassword<'_>,
) -> Result<Vec<String>, DdlError> {
    require_name(name, "login")?;
    let login = bracket(name);
    let mut statements = Vec::new();
    let mut options = database_options(Some(before), after);
    if let (Some(before), Some(after)) = (before.policy, after.policy) {
        options.extend(policy_options(before, after));
    }
    if !options.is_empty() {
        statements.push(format!("ALTER LOGIN {login} WITH {}", options.join(", ")));
    }
    match change.password {
        Some(password) => {
            if change.must_change {
                must_change_allowed(password, after.policy.unwrap_or_default())?;
            }
            let mut clause = password_clause(password, change.must_change);
            if change.unlock {
                clause.push_str(" UNLOCK");
            }
            statements.push(format!("ALTER LOGIN {login} WITH {clause}"));
        }
        None if change.unlock => return Err(DdlError::UnlockNeedsPassword),
        None if change.must_change => return Err(DdlError::MustChangeNeedsTypedPassword),
        None => {}
    }
    if before.enabled != after.enabled {
        statements.push(enable(name, after.enabled));
    }
    statements.extend(server_role_changes(
        name,
        &before.server_roles,
        &after.server_roles,
    ));
    nonempty(statements)
}

/// `PASSWORD = …`, hashed or as typed, with `MUST_CHANGE` after it. The history's redaction
/// hides what follows `PASSWORD` to the next comma, so the options after it are written after
/// a comma.
fn password_clause(password: &SqlServerPassword, must_change: bool) -> String {
    let mut clause = match password {
        SqlServerPassword::Hashed(hash) => format!("PASSWORD = {} HASHED", hash.as_str()),
        SqlServerPassword::AsTyped(password) => {
            format!("PASSWORD = {}", literal(password.expose()))
        }
    };
    if must_change {
        clause.push_str(" MUST_CHANGE");
    }
    clause
}

fn must_change_allowed(password: &SqlServerPassword, policy: LoginPolicy) -> Result<(), DdlError> {
    if matches!(password, SqlServerPassword::Hashed(_)) {
        return Err(DdlError::MustChangeNeedsTypedPassword);
    }
    if !policy.check_policy || !policy.check_expiration {
        return Err(DdlError::MustChangeNeedsPolicy);
    }
    Ok(())
}

/// `DEFAULT_DATABASE` and `DEFAULT_LANGUAGE` where they differ from `before`, or from the
/// server's defaults for a new login.
fn database_options(before: Option<&LoginSpec>, after: &LoginSpec) -> Vec<String> {
    let mut options = Vec::new();
    let database_before = before.map_or(DEFAULT_DATABASE, |before| &before.default_database);
    if after.default_database != database_before && !after.default_database.trim().is_empty() {
        options.push(format!(
            "DEFAULT_DATABASE = {}",
            bracket(&after.default_database)
        ));
    }
    let language_before = before.and_then(|before| before.default_language.as_deref());
    if let Some(language) = after.default_language.as_deref()
        && Some(language) != language_before
        && !language.trim().is_empty()
    {
        options.push(format!("DEFAULT_LANGUAGE = {}", bracket(language)));
    }
    options
}

fn policy_options(before: LoginPolicy, after: LoginPolicy) -> Vec<String> {
    let on_off = |on: bool| if on { "ON" } else { "OFF" };
    let mut options = Vec::new();
    if before.check_policy != after.check_policy {
        options.push(format!("CHECK_POLICY = {}", on_off(after.check_policy)));
    }
    if before.check_expiration != after.check_expiration {
        options.push(format!(
            "CHECK_EXPIRATION = {}",
            on_off(after.check_expiration)
        ));
    }
    options
}

fn enable(name: &str, enabled: bool) -> String {
    let state = if enabled { "ENABLE" } else { "DISABLE" };
    format!("ALTER LOGIN {} {state}", bracket(name))
}

/// The server roles a login leaves and joins.
fn server_role_changes(
    login: &str,
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
) -> Vec<String> {
    let scope = PrincipalScope::Server;
    before
        .difference(after)
        .map(|role| drop_member(&scope, role, login))
        .chain(
            after
                .difference(before)
                .map(|role| add_member(&scope, role, login)),
        )
        .collect()
}

/// The members a role loses and gains.
fn member_changes(
    scope: &PrincipalScope,
    role: &str,
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
) -> Vec<String> {
    before
        .difference(after)
        .map(|member| drop_member(scope, role, member))
        .chain(
            after
                .difference(before)
                .map(|member| add_member(scope, role, member)),
        )
        .collect()
}

fn add_member(scope: &PrincipalScope, role: &str, member: &str) -> String {
    membership(scope, role, "ADD", member)
}

fn drop_member(scope: &PrincipalScope, role: &str, member: &str) -> String {
    membership(scope, role, "DROP", member)
}

fn membership(scope: &PrincipalScope, role: &str, change: &str, member: &str) -> String {
    scoped(
        scope,
        &format!(
            "ALTER {} {} {change} MEMBER {}",
            role_keyword(scope),
            bracket(role),
            bracket(member)
        ),
    )
}

fn role_keyword(scope: &PrincipalScope) -> &'static str {
    match scope {
        PrincipalScope::Server => "SERVER ROLE",
        PrincipalScope::Database(_) => "ROLE",
    }
}

fn create_user(
    database: &str,
    name: &str,
    source: &UserSource,
    spec: &DatabaseUserSpec,
) -> Result<Vec<String>, DdlError> {
    require_name(database, "database")?;
    require_name(name, "user")?;
    let mut options = Vec::new();
    let from = match source {
        UserSource::Login(login) => {
            require_name(login, "login")?;
            format!(" FOR LOGIN {}", bracket(login))
        }
        UserSource::WithoutLogin => " WITHOUT LOGIN".to_owned(),
        UserSource::Password(password) => {
            options.push(format!("PASSWORD = {}", literal(password.expose())));
            String::new()
        }
        UserSource::External => " FROM EXTERNAL PROVIDER".to_owned(),
    };
    if let Some(schema) = spec
        .default_schema
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        options.push(format!("DEFAULT_SCHEMA = {}", bracket(schema)));
    }
    let with = if options.is_empty() {
        String::new()
    } else {
        format!(" WITH {}", options.join(", "))
    };
    let scope = PrincipalScope::Database(database.to_owned());
    let mut statements = vec![in_database(
        database,
        &format!("CREATE USER {}{from}{with}", bracket(name)),
    )];
    statements.extend(spec.roles.iter().map(|role| add_member(&scope, role, name)));
    Ok(statements)
}

/// A database user, by where it is and its name.
struct UserAt<'a> {
    database: &'a str,
    name: &'a str,
}

fn alter_user(
    user: &UserAt<'_>,
    before: &DatabaseUserSpec,
    after: &DatabaseUserSpec,
    login: Option<&str>,
    password: Option<&Password>,
) -> Result<Vec<String>, DdlError> {
    require_name(user.name, "user")?;
    let mut options = Vec::new();
    if let Some(login) = login {
        require_name(login, "login")?;
        options.push(format!("LOGIN = {}", bracket(login)));
    }
    if before.default_schema != after.default_schema {
        let schema = after
            .default_schema
            .as_deref()
            .filter(|schema| !schema.trim().is_empty())
            .map_or_else(|| "NULL".to_owned(), bracket);
        options.push(format!("DEFAULT_SCHEMA = {schema}"));
    }
    // Last in its clause: see `password_clause`.
    if let Some(password) = password {
        options.push(format!("PASSWORD = {}", literal(password.expose())));
    }
    let mut statements = Vec::new();
    if !options.is_empty() {
        statements.push(in_database(
            user.database,
            &format!(
                "ALTER USER {} WITH {}",
                bracket(user.name),
                options.join(", ")
            ),
        ));
    }
    let scope = PrincipalScope::Database(user.database.to_owned());
    statements.extend(
        before
            .roles
            .difference(&after.roles)
            .map(|role| drop_member(&scope, role, user.name))
            .chain(
                after
                    .roles
                    .difference(&before.roles)
                    .map(|role| add_member(&scope, role, user.name)),
            ),
    );
    nonempty(statements)
}

fn drop_user(
    user: &UserAt<'_>,
    owned_schemas: &[String],
    owned_roles: &[String],
    give_to: Option<&str>,
) -> Result<Vec<String>, DdlError> {
    require_name(user.name, "user")?;
    let mut statements = Vec::new();
    if let Some(owner) = give_to {
        let owner = owner.trim();
        if owner.is_empty() {
            return Err(DdlError::NoNewOwner);
        }
        if owner == user.name {
            return Err(DdlError::ReassignToItself);
        }
        let owned = owned_schemas
            .iter()
            .map(|schema| ("SCHEMA", schema))
            .chain(owned_roles.iter().map(|role| ("ROLE", role)));
        for (class, object) in owned {
            statements.push(in_database(
                user.database,
                &format!(
                    "ALTER AUTHORIZATION ON {class}::{} TO {}",
                    bracket(object),
                    bracket(owner)
                ),
            ));
        }
    }
    statements.push(in_database(
        user.database,
        &format!("DROP USER {}", bracket(user.name)),
    ));
    Ok(statements)
}

/// `REVOKE`, which takes a `DENY` away as well as a `GRANT`. A grant with grant option is
/// revoked with `CASCADE`, which SQL Server requires, taking it from whoever it was passed to.
fn revoke(principal: &str, grant: &PermissionGrant) -> String {
    let on = match &grant.securable {
        Securable::Server | Securable::Database => String::new(),
        Securable::Schema(schema) => format!(" ON SCHEMA::{}", bracket(schema)),
        Securable::Object { schema, name } => {
            format!(" ON OBJECT::{}.{}", bracket(schema), bracket(name))
        }
        Securable::Column {
            schema,
            object,
            column,
        } => format!(
            " ON OBJECT::{}.{} ({})",
            bracket(schema),
            bracket(object),
            bracket(column)
        ),
        Securable::Other { class, name } => format!(" ON {class}::{}", bracket(name)),
    };
    let cascade = if grant.state == PermissionState::GrantWithGrantOption {
        " CASCADE"
    } else {
        ""
    };
    format!(
        "REVOKE {}{on} FROM {}{cascade}",
        grant.permission,
        bracket(principal)
    )
}

/// `statement` where `scope` says it runs: as it is for the server, through
/// [`in_database`] for a database.
fn scoped(scope: &PrincipalScope, statement: &str) -> String {
    match scope {
        PrincipalScope::Server => statement.to_owned(),
        PrincipalScope::Database(database) => in_database(database, statement),
    }
}

/// `statement` run in `database`: see the module's documentation.
fn in_database(database: &str, statement: &str) -> String {
    format!(
        "EXEC {}.sys.sp_executesql {}",
        bracket(database),
        literal(statement)
    )
}

fn nonempty(statements: Vec<String>) -> Result<Vec<String>, DdlError> {
    if statements.is_empty() {
        Err(DdlError::NothingChanged)
    } else {
        Ok(statements)
    }
}

fn bracket(name: &str) -> String {
    Dialect::SqlServer.quoting().identifier(name)
}

fn literal(text: &str) -> String {
    Dialect::SqlServer.quoting().string_literal(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DdlOperation;

    fn run(operation: SqlServerOperation) -> Result<Vec<String>, DdlError> {
        DdlOperation::SqlServer(operation).statements(Dialect::SqlServer)
    }

    fn typed(password: &str) -> SqlServerPassword {
        SqlServerPassword::AsTyped(Password::new(password))
    }

    fn sql_login(password: SqlServerPassword, must_change: bool) -> LoginSource {
        LoginSource::Sql {
            password,
            must_change,
        }
    }

    fn strings(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn creates_a_hashed_login_with_its_roles_and_users() {
        let hash = SqlServerHash::hash("Other-pw1!X");
        let spec = LoginSpec {
            default_database: "shop".to_owned(),
            enabled: false,
            policy: Some(LoginPolicy {
                check_policy: false,
                check_expiration: false,
            }),
            server_roles: strings(&["##MS_ServerStateReader##"]),
            ..LoginSpec::new(None)
        };
        let statements = run(SqlServerOperation::CreateLogin {
            name: "app".to_owned(),
            source: sql_login(SqlServerPassword::Hashed(hash.clone()), false),
            spec,
            mappings: vec![UserMapping {
                database: "shop".to_owned(),
                user: "app".to_owned(),
                spec: DatabaseUserSpec {
                    default_schema: Some("sales".to_owned()),
                    roles: strings(&["db_datareader"]),
                },
            }],
        });
        assert_eq!(
            statements.unwrap(),
            [
                format!(
                    "CREATE LOGIN [app] WITH PASSWORD = {} HASHED, DEFAULT_DATABASE = [shop], \
                     CHECK_POLICY = OFF",
                    hash.as_str()
                ),
                "ALTER LOGIN [app] DISABLE".to_owned(),
                "ALTER SERVER ROLE [##MS_ServerStateReader##] ADD MEMBER [app]".to_owned(),
                "EXEC [shop].sys.sp_executesql N'CREATE USER [app] FOR LOGIN [app] WITH \
                 DEFAULT_SCHEMA = [sales]'"
                    .to_owned(),
                "EXEC [shop].sys.sp_executesql N'ALTER ROLE [db_datareader] ADD MEMBER [app]'"
                    .to_owned(),
            ]
        );
    }

    #[test]
    fn must_change_needs_a_typed_password_and_the_policy() {
        let checked = LoginSpec::new(Some(LoginPolicy {
            check_policy: true,
            check_expiration: true,
        }));
        let create = |password, spec: &LoginSpec| {
            run(SqlServerOperation::CreateLogin {
                name: "app".to_owned(),
                source: sql_login(password, true),
                spec: spec.clone(),
                mappings: vec![],
            })
        };
        assert_eq!(
            create(typed("it's"), &checked).unwrap(),
            ["CREATE LOGIN [app] WITH PASSWORD = N'it''s' MUST_CHANGE, CHECK_EXPIRATION = ON"]
        );
        assert_eq!(
            create(
                SqlServerPassword::Hashed(SqlServerHash::hash("x")),
                &checked
            ),
            Err(DdlError::MustChangeNeedsTypedPassword)
        );
        assert_eq!(
            create(typed("x"), &LoginSpec::new(Some(LoginPolicy::default()))),
            Err(DdlError::MustChangeNeedsPolicy)
        );
    }

    #[test]
    fn creates_windows_and_microsoft_entra_logins_by_name() {
        let spec = LoginSpec {
            default_database: "shop".to_owned(),
            ..LoginSpec::new(None)
        };
        let create = |name: &str, source| {
            run(SqlServerOperation::CreateLogin {
                name: name.to_owned(),
                source,
                spec: spec.clone(),
                mappings: vec![],
            })
            .unwrap()
        };
        assert_eq!(
            create("CORP\\analysts", LoginSource::Windows),
            ["CREATE LOGIN [CORP\\analysts] FROM WINDOWS WITH DEFAULT_DATABASE = [shop]"]
        );
        assert_eq!(
            create("ops@contoso.com", LoginSource::External),
            ["CREATE LOGIN [ops@contoso.com] FROM EXTERNAL PROVIDER"]
        );
    }

    #[test]
    fn alters_policy_before_the_password_then_enables_and_roles() {
        let before = LoginSpec {
            server_roles: strings(&["dbcreator"]),
            ..LoginSpec::new(Some(LoginPolicy::default()))
        };
        let after = LoginSpec {
            default_language: Some("British".to_owned()),
            enabled: false,
            policy: Some(LoginPolicy {
                check_policy: true,
                check_expiration: true,
            }),
            server_roles: strings(&["sysadmin"]),
            ..before.clone()
        };
        let statements = run(SqlServerOperation::AlterLogin {
            name: "app".to_owned(),
            before,
            after,
            password: Some(typed("New-pw1!")),
            must_change: true,
            unlock: true,
        });
        assert_eq!(
            statements.unwrap(),
            [
                "ALTER LOGIN [app] WITH DEFAULT_LANGUAGE = [British], CHECK_EXPIRATION = ON",
                "ALTER LOGIN [app] WITH PASSWORD = N'New-pw1!' MUST_CHANGE UNLOCK",
                "ALTER LOGIN [app] DISABLE",
                "ALTER SERVER ROLE [dbcreator] DROP MEMBER [app]",
                "ALTER SERVER ROLE [sysadmin] ADD MEMBER [app]",
            ]
        );
    }

    #[test]
    fn refuses_an_unlock_without_a_password_and_an_empty_change() {
        let spec = LoginSpec::new(Some(LoginPolicy::default()));
        let alter = |unlock| {
            run(SqlServerOperation::AlterLogin {
                name: "app".to_owned(),
                before: spec.clone(),
                after: spec.clone(),
                password: None,
                must_change: false,
                unlock,
            })
        };
        assert_eq!(alter(true), Err(DdlError::UnlockNeedsPassword));
        assert_eq!(alter(false), Err(DdlError::NothingChanged));
    }

    #[test]
    fn writes_server_and_database_roles() {
        let shop = PrincipalScope::Database("shop".to_owned());
        let cases = [
            (
                SqlServerOperation::CreateRole {
                    scope: PrincipalScope::Server,
                    name: "monitoring".to_owned(),
                    owner: Some("sa".to_owned()),
                    members: strings(&["app"]),
                },
                vec![
                    "CREATE SERVER ROLE [monitoring] AUTHORIZATION [sa]",
                    "ALTER SERVER ROLE [monitoring] ADD MEMBER [app]",
                ],
            ),
            (
                SqlServerOperation::AlterRoleMembers {
                    scope: shop.clone(),
                    name: "reporting".to_owned(),
                    before: strings(&["a", "b"]),
                    after: strings(&["b", "c"]),
                },
                vec![
                    "EXEC [shop].sys.sp_executesql N'ALTER ROLE [reporting] DROP MEMBER [a]'",
                    "EXEC [shop].sys.sp_executesql N'ALTER ROLE [reporting] ADD MEMBER [c]'",
                ],
            ),
            (
                SqlServerOperation::RenameRole {
                    scope: PrincipalScope::Server,
                    name: "monitoring".to_owned(),
                    new_name: "watchers".to_owned(),
                },
                vec!["ALTER SERVER ROLE [monitoring] WITH NAME = [watchers]"],
            ),
            (
                SqlServerOperation::DropRole {
                    scope: shop,
                    name: "it's".to_owned(),
                },
                vec!["EXEC [shop].sys.sp_executesql N'DROP ROLE [it''s]'"],
            ),
        ];
        for (operation, expected) in cases {
            assert_eq!(run(operation).unwrap(), expected);
        }
    }

    #[test]
    fn creates_each_kind_of_database_user() {
        let create = |source| {
            run(SqlServerOperation::CreateUser {
                database: "shop".to_owned(),
                name: "u".to_owned(),
                source,
                spec: DatabaseUserSpec::default(),
            })
            .unwrap()
        };
        assert_eq!(
            create(UserSource::WithoutLogin),
            ["EXEC [shop].sys.sp_executesql N'CREATE USER [u] WITHOUT LOGIN'"]
        );
        assert_eq!(
            create(UserSource::Password(Password::new("s3cret"))),
            ["EXEC [shop].sys.sp_executesql N'CREATE USER [u] WITH PASSWORD = N''s3cret'''"]
        );
        assert_eq!(
            create(UserSource::External),
            ["EXEC [shop].sys.sp_executesql N'CREATE USER [u] FROM EXTERNAL PROVIDER'"]
        );
    }

    #[test]
    fn maps_an_orphaned_user_and_changes_its_schema_and_roles() {
        let before = DatabaseUserSpec {
            default_schema: Some("dbo".to_owned()),
            roles: strings(&["db_datawriter"]),
        };
        let after = DatabaseUserSpec {
            default_schema: None,
            roles: strings(&["db_datareader"]),
        };
        let statements = run(SqlServerOperation::AlterUser {
            database: "shop".to_owned(),
            name: "legacy".to_owned(),
            before,
            after,
            login: Some("legacy_login".to_owned()),
            password: None,
        });
        assert_eq!(
            statements.unwrap(),
            [
                "EXEC [shop].sys.sp_executesql N'ALTER USER [legacy] WITH LOGIN = \
                 [legacy_login], DEFAULT_SCHEMA = NULL'",
                "EXEC [shop].sys.sp_executesql N'ALTER ROLE [db_datawriter] DROP MEMBER [legacy]'",
                "EXEC [shop].sys.sp_executesql N'ALTER ROLE [db_datareader] ADD MEMBER [legacy]'",
            ]
        );
    }

    #[test]
    fn gives_away_what_a_user_owns_before_dropping_it() {
        let drop = |give_to: Option<&str>| {
            run(SqlServerOperation::DropUser {
                database: "shop".to_owned(),
                name: "u".to_owned(),
                owned_schemas: vec!["sales".to_owned()],
                owned_roles: vec!["reporting".to_owned()],
                give_to: give_to.map(str::to_owned),
            })
        };
        assert_eq!(
            drop(Some("dbo")).unwrap(),
            [
                "EXEC [shop].sys.sp_executesql N'ALTER AUTHORIZATION ON SCHEMA::[sales] TO [dbo]'",
                "EXEC [shop].sys.sp_executesql N'ALTER AUTHORIZATION ON ROLE::[reporting] TO [dbo]'",
                "EXEC [shop].sys.sp_executesql N'DROP USER [u]'",
            ]
        );
        assert_eq!(
            drop(None).unwrap(),
            ["EXEC [shop].sys.sp_executesql N'DROP USER [u]'"]
        );
        assert_eq!(drop(Some("u")), Err(DdlError::ReassignToItself));
    }

    #[test]
    fn drops_a_logins_users_before_the_login() {
        let statements = run(SqlServerOperation::DropLogin {
            name: "a]b".to_owned(),
            users: vec![DatabaseUserName {
                database: "shop".to_owned(),
                user: "a]b".to_owned(),
            }],
        });
        assert_eq!(
            statements.unwrap(),
            [
                "EXEC [shop].sys.sp_executesql N'DROP USER [a]]b]'",
                "DROP LOGIN [a]]b]",
            ]
        );
    }

    #[test]
    fn revokes_grants_and_denies_on_each_securable() {
        let grant = |securable, state| PermissionGrant {
            permission: "SELECT".to_owned(),
            securable,
            state,
        };
        let shop = PrincipalScope::Database("shop".to_owned());
        let cases = [
            (
                PrincipalScope::Server,
                PermissionGrant {
                    permission: "VIEW SERVER STATE".to_owned(),
                    securable: Securable::Server,
                    state: PermissionState::Grant,
                },
                "REVOKE VIEW SERVER STATE FROM [app]",
            ),
            (
                shop.clone(),
                grant(Securable::Schema("sales".to_owned()), PermissionState::Deny),
                "EXEC [shop].sys.sp_executesql N'REVOKE SELECT ON SCHEMA::[sales] FROM [app]'",
            ),
            (
                shop.clone(),
                grant(
                    Securable::Column {
                        schema: "dbo".to_owned(),
                        object: "orders".to_owned(),
                        column: "total".to_owned(),
                    },
                    PermissionState::GrantWithGrantOption,
                ),
                "EXEC [shop].sys.sp_executesql N'REVOKE SELECT ON OBJECT::[dbo].[orders] \
                 ([total]) FROM [app] CASCADE'",
            ),
            (
                PrincipalScope::Server,
                PermissionGrant {
                    permission: "IMPERSONATE".to_owned(),
                    securable: Securable::Other {
                        class: "LOGIN".to_owned(),
                        name: "sa".to_owned(),
                    },
                    state: PermissionState::Grant,
                },
                "REVOKE IMPERSONATE ON LOGIN::[sa] FROM [app]",
            ),
        ];
        for (scope, grant, expected) in cases {
            let statements = run(SqlServerOperation::Revoke {
                scope,
                principal: "app".to_owned(),
                grant,
            });
            assert_eq!(statements.unwrap(), [expected]);
        }
    }

    #[test]
    fn only_sql_servers_dialect_has_logins() {
        let operation = DdlOperation::SqlServer(SqlServerOperation::SetLoginEnabled {
            name: "app".to_owned(),
            enabled: true,
        });
        for dialect in Dialect::ALL {
            assert_eq!(
                operation.statements(dialect).is_ok(),
                dialect == Dialect::SqlServer,
                "{dialect:?}"
            );
        }
    }
}
