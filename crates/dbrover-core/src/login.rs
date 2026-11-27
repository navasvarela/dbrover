//! SQL Server's principals: logins and server roles, which belong to the server, and database
//! users and database roles, which belong to one database (W18,
//! `docs/users-beyond-postgres.md`).

use std::collections::BTreeSet;

/// How a login proves who it is, from `sys.server_principals.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoginKind {
    /// A name and a password the server checks (`S`).
    Sql,
    /// A Windows account (`U`).
    Windows,
    /// A Windows group, whose members log in as themselves (`G`).
    WindowsGroup,
    /// A Microsoft Entra user or application (`E`).
    External,
    /// A Microsoft Entra group (`X`).
    ExternalGroup,
    /// Mapped to a certificate, for signing modules; nobody logs in as it (`C`).
    Certificate,
    /// Mapped to an asymmetric key, likewise (`K`).
    AsymmetricKey,
}

impl LoginKind {
    /// The kind a `sys.server_principals.type` code stands for; `None` for a server role
    /// (`R`) or a code SQL Server may add later.
    pub fn from_code(code: &str) -> Option<Self> {
        Some(match code.trim() {
            "S" => Self::Sql,
            "U" => Self::Windows,
            "G" => Self::WindowsGroup,
            "E" => Self::External,
            "X" => Self::ExternalGroup,
            "C" => Self::Certificate,
            "K" => Self::AsymmetricKey,
            _ => return None,
        })
    }

    /// How the explorer and the dialogs name it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sql => "SQL login",
            Self::Windows => "Windows login",
            Self::WindowsGroup => "Windows group",
            Self::External => "Microsoft Entra",
            Self::ExternalGroup => "Microsoft Entra group",
            Self::Certificate => "certificate",
            Self::AsymmetricKey => "asymmetric key",
        }
    }
}

/// A SQL login's password policy and its state, as `sys.sql_logins` and `LOGINPROPERTY`
/// report them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SqlLoginState {
    /// `CHECK_POLICY`: Windows's password policy applies.
    pub check_policy: bool,
    /// `CHECK_EXPIRATION`: the password expires as the policy says.
    pub check_expiration: bool,
    /// Locked after too many failed logins.
    pub locked: bool,
    pub expired: bool,
    /// Must change its password at the next login.
    pub must_change: bool,
    pub bad_password_count: u32,
    /// `None` where the password doesn't expire.
    pub days_until_expiration: Option<i32>,
}

/// Whether a permission is granted, granted with the right to grant it on, or denied, from
/// `sys.server_permissions.state` and `sys.database_permissions.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PermissionState {
    Grant,
    GrantWithGrantOption,
    /// Wins over every grant, a role's included.
    Deny,
}

impl PermissionState {
    /// The state a catalog's `state` code stands for: `G`, `W` or `D`. `R` (revoked) never
    /// appears in the catalog.
    pub fn from_code(code: &str) -> Option<Self> {
        Some(match code.trim() {
            "G" => Self::Grant,
            "W" => Self::GrantWithGrantOption,
            "D" => Self::Deny,
            _ => return None,
        })
    }
}

/// What a permission is on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Securable {
    /// The whole server (`VIEW SERVER STATE`).
    Server,
    /// The database the permission is recorded in (`CONNECT`, `CREATE TABLE`).
    Database,
    Schema(String),
    /// A table, view, procedure or other schema object.
    Object {
        schema: String,
        name: String,
    },
    Column {
        schema: String,
        object: String,
        column: String,
    },
    /// Any other class, written `ON <class>::[name]`: `LOGIN`, `SERVER ROLE`, `ENDPOINT`,
    /// `USER`, `ROLE`, `TYPE` and so on.
    Other {
        class: String,
        name: String,
    },
}

/// One row of a principal's permissions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PermissionGrant {
    /// As the catalog names it, in upper case: `VIEW SERVER STATE`, `SELECT`.
    pub permission: String,
    pub securable: Securable,
    pub state: PermissionState,
}

/// A database user, by the database it is in and its name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DatabaseUserName {
    pub database: String,
    pub user: String,
}

/// A login as the server describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginDescription {
    pub name: String,
    pub kind: LoginKind,
    pub disabled: bool,
    pub default_database: String,
    pub default_language: Option<String>,
    /// For a SQL login only.
    pub sql: Option<SqlLoginState>,
    pub server_roles: BTreeSet<String>,
    pub permissions: BTreeSet<PermissionGrant>,
    /// The users made for it, in each database the reading login could see.
    pub users: BTreeSet<DatabaseUserName>,
    /// The databases it owns, which stop `DROP LOGIN`.
    pub owned_databases: Vec<String>,
}

/// What a database user is for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DatabaseUserKind {
    /// Made for a login, which reaches the database through it.
    Login(String),
    /// Made for a login that no longer exists.
    Orphaned,
    /// Has no login: for signing modules and impersonation.
    WithoutLogin,
    /// Has a password of its own, in a contained database.
    Contained,
    /// A Microsoft Entra user or group, with no login of the server's.
    External,
    /// Mapped to a certificate or asymmetric key.
    Key,
}

/// A database user as the server describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseUserDescription {
    pub database: String,
    pub name: String,
    pub kind: DatabaseUserKind,
    pub default_schema: Option<String>,
    pub roles: BTreeSet<String>,
    pub permissions: BTreeSet<PermissionGrant>,
    /// The schemas it owns, which stop `DROP USER`.
    pub owned_schemas: Vec<String>,
    /// The database roles it owns, which stop `DROP USER` too.
    pub owned_roles: Vec<String>,
}

/// Where a SQL Server role, or a permission, belongs: the server or one database.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrincipalScope {
    Server,
    Database(String),
}

/// A server role or a database role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlServerRoleDescription {
    pub scope: PrincipalScope,
    pub name: String,
    /// One of SQL Server's own (`sysadmin`, `db_owner`, `##MS_…##`), which can't be changed
    /// apart from its members.
    pub fixed: bool,
    pub owner: Option<String>,
    pub members: BTreeSet<String>,
}

/// The fixed roles and the permissions whose grant gives administration of the server or a
/// whole database, said before Execute (question 8 of the plan).
const ADMINISTRATIVE: [&str; 6] = [
    "sysadmin",
    "securityadmin",
    "CONTROL SERVER",
    "db_owner",
    "db_securityadmin",
    "CONTROL",
];

/// Whether granting the role or permission `name` gives administration.
pub fn sql_server_is_administrative(name: &str) -> bool {
    ADMINISTRATIVE
        .iter()
        .any(|administrative| administrative.eq_ignore_ascii_case(name.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn principal_codes_are_read() {
        assert_eq!(LoginKind::from_code("S "), Some(LoginKind::Sql));
        assert_eq!(LoginKind::from_code("E"), Some(LoginKind::External));
        assert_eq!(LoginKind::from_code("R"), None);
        assert_eq!(
            PermissionState::from_code("W"),
            Some(PermissionState::GrantWithGrantOption)
        );
    }

    #[test]
    fn administration_is_recognised_in_any_case() {
        assert!(sql_server_is_administrative("SysAdmin"));
        assert!(sql_server_is_administrative("control server"));
        assert!(!sql_server_is_administrative("db_datareader"));
    }
}
