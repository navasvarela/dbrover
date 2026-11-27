//! What a driver reads before the users dialogs open (W18, `docs/users-beyond-postgres.md`):
//! one account, login, user or role, and what the server and this login allow.

use crate::{
    AccountDescription, DatabaseUserDescription, LoginDescription, MysqlFlavor,
    SqlServerRoleDescription,
};

/// One of the principals a users folder lists, as the server describes it. PostgreSQL's
/// roles have their own [`RoleDescription`](crate::RoleDescription).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrincipalDescription {
    /// A MySQL or MariaDB user or role.
    Account(AccountDescription),
    Login(LoginDescription),
    DatabaseUser(DatabaseUserDescription),
    /// A server role or a database role.
    SqlServerRole(SqlServerRoleDescription),
}

/// Whether this login may do something, and if not, what would let it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permit {
    Allowed,
    /// The privilege or permission that would allow it, as the server names it.
    Needs(String),
}

impl Permit {
    pub fn allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// What the server offers for managing its users, and what this login may do there: read
/// once when a users folder first loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserManagement {
    /// The account or login DbRover is connected as, named as its folder lists it.
    pub current: String,
    /// The connection is encrypted, so a password may be sent as typed (question 5 of the
    /// plan).
    pub encrypted: bool,
    /// Listing the other accounts or logins; without it, the folder shows the login's own.
    pub may_list: Permit,
    /// Creating, changing and dropping them.
    pub may_change: Permit,
    pub server: ManagedUsers,
}

/// The engine's own part of [`UserManagement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedUsers {
    Accounts(AccountServer),
    Logins(LoginServer),
}

/// What a MySQL or MariaDB server offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountServer {
    pub flavor: MysqlFlavor,
    pub version: String,
    /// The authentication plugins the server has active, by name.
    pub plugins: Vec<String>,
    /// A password check is loaded (`validate_password`, MariaDB's `simple_password_check` or
    /// `cracklib_password_check`), which a hashed password would bypass.
    pub password_checks: bool,
    /// The lock after failed logins per account (MySQL 8.0.19 and later).
    pub failed_login_lock: bool,
}

/// What a SQL Server offers, in the database the connection is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginServer {
    pub database: String,
    /// The database is contained, so its users may have passwords of their own.
    pub contained: bool,
    /// Creating, changing and dropping the database's users and roles.
    pub may_change_users: Permit,
}
