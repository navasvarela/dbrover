//! MySQL's and MariaDB's accounts: `'name'@'host'` users, the roles granted to them, and
//! their privileges at each level (W18, `docs/users-beyond-postgres.md`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroU16;

/// Which server a MySQL-protocol connection reached. They share most of their SQL but spell
/// account statements differently (`SET DEFAULT ROLE … TO` and `… FOR`, roles with or without
/// a host, how a hashed password is written).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MysqlFlavor {
    Mysql,
    MariaDb,
}

/// An account's whole name. MySQL matches a login against both parts, so `app@%` and
/// `app@localhost` are two accounts with their own passwords and privileges.
///
/// A MariaDB role has no host: `host` is empty. A MySQL role has one, `%` unless given.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountName {
    pub user: String,
    pub host: String,
}

impl AccountName {
    pub fn new(user: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            host: host.into(),
        }
    }

    /// The host a role's name has when it is written without one: `%` on MySQL, where a role
    /// is an account, and none on MariaDB.
    pub fn role(name: impl Into<String>, flavor: MysqlFlavor) -> Self {
        let host = match flavor {
            MysqlFlavor::Mysql => "%",
            MysqlFlavor::MariaDb => "",
        };
        Self::new(name, host)
    }

    /// The account's name as the explorer shows it and as its catalog node is named:
    /// `user@host`, or the bare name of a role without a host.
    pub fn label(&self) -> String {
        self.to_string()
    }

    /// Reads a label written by [`AccountName::label`] back. The host is what follows the
    /// last `@`, since a user name may hold an `@` (`ops@example.com@%`) and a host can't.
    pub fn from_label(label: &str) -> Self {
        match label.rsplit_once('@') {
            Some((user, host)) => Self::new(user, host),
            None => Self::new(label, ""),
        }
    }
}

impl fmt::Display for AccountName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.is_empty() {
            f.write_str(&self.user)
        } else {
            write!(f, "{}@{}", self.user, self.host)
        }
    }
}

/// Whether an account logs in or is granted to others. MariaDB marks its roles; MySQL's are
/// accounts created locked and without a password, which is how DbRover tells them apart
/// (question 2 of the plan).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountKind {
    User,
    Role,
}

/// How long a password lasts before the server asks for a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PasswordLifetime {
    /// What the server's `default_password_lifetime` says.
    #[default]
    ServerDefault,
    Never,
    Days(NonZeroU16),
}

/// MySQL's lock after failed logins (`FAILED_LOGIN_ATTEMPTS`, `PASSWORD_LOCK_TIME`, 8.0.19
/// and later). MariaDB has only a server-wide setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailedLoginLock {
    pub attempts: NonZeroU16,
    pub lock: LockTime,
}

/// How long a [`FailedLoginLock`] lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockTime {
    Days(NonZeroU16),
    /// Until the account is unlocked (`PASSWORD_LOCK_TIME UNBOUNDED`).
    Unbounded,
}

/// What an account may use of the server, each `0` for no limit, as MySQL writes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccountLimits {
    /// Connections open at once.
    pub max_user_connections: u32,
    pub max_connections_per_hour: u32,
    pub max_queries_per_hour: u32,
    pub max_updates_per_hour: u32,
}

/// One privilege as the server names it, in upper case: `SELECT`, `CREATE USER`, or one of
/// MySQL 8's dynamic privileges such as `BACKUP_ADMIN`. The set is open, since components and
/// plugins add privileges of their own.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Privilege(String);

/// The privileges that let an account change other accounts, read or write the server's files,
/// or bypass its checks. A grant of one is said before Execute (question 8 of the plan).
const ADMINISTRATIVE: [&str; 9] = [
    "ALL",
    "ALL PRIVILEGES",
    "SUPER",
    "CREATE USER",
    "SYSTEM_USER",
    "SET_ANY_DEFINER",
    "SET_USER_ID",
    "FILE",
    "GRANT OPTION",
];

impl Privilege {
    /// `name` in upper case, with runs of spaces made one, as `SHOW PRIVILEGES` lists it.
    pub fn new(name: &str) -> Self {
        Self(
            name.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_uppercase(),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether granting it gives administration of the server.
    pub fn is_administrative(&self) -> bool {
        ADMINISTRATIVE.contains(&self.0.as_str())
    }
}

impl fmt::Display for Privilege {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Privileges at one level, and whether they may be passed on (`WITH GRANT OPTION`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrivilegeSet {
    pub privileges: BTreeSet<Privilege>,
    pub grant_option: bool,
}

impl PrivilegeSet {
    pub fn is_empty(&self) -> bool {
        self.privileges.is_empty() && !self.grant_option
    }
}

/// A stored routine's kind, which `GRANT … ON PROCEDURE` and `ON FUNCTION` name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RoutineKind {
    Procedure,
    Function,
}

impl RoutineKind {
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Procedure => "PROCEDURE",
            Self::Function => "FUNCTION",
        }
    }
}

/// Where a privilege finer than a database's applies.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectLevel {
    Table {
        database: String,
        table: String,
    },
    Column {
        database: String,
        table: String,
        column: String,
    },
    Routine {
        database: String,
        name: String,
        kind: RoutineKind,
    },
}

/// A privilege on a table, a column or a routine, which W18 lists and revokes but doesn't
/// grant (question 4 of the plan).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectPrivilege {
    pub level: ObjectLevel,
    pub privilege: Privilege,
}

/// Whether a granted role may itself be granted on by its holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdminOption {
    #[default]
    Without,
    With,
}

/// The kind of stored object an account can be the definer of, which runs with the definer's
/// privileges and fails once the definer is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DefinedKind {
    View,
    Procedure,
    Function,
    Trigger,
    Event,
}

impl DefinedKind {
    pub fn noun(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Procedure => "procedure",
            Self::Function => "function",
            Self::Trigger => "trigger",
            Self::Event => "event",
        }
    }
}

/// A view, routine, trigger or event whose `DEFINER` is an account.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefinedObject {
    pub kind: DefinedKind,
    pub database: String,
    pub name: String,
}

/// An account as the server describes it, for editing it and for what dropping it breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDescription {
    pub name: AccountName,
    pub kind: AccountKind,
    /// The authentication plugin, as the server names it (`caching_sha2_password`).
    pub plugin: String,
    pub has_password: bool,
    pub locked: bool,
    /// The password must be changed at the next login.
    pub password_expired: bool,
    pub lifetime: PasswordLifetime,
    /// MySQL only; `None` where none is set or the server has no such lock.
    pub failed_login: Option<FailedLoginLock>,
    pub limits: AccountLimits,
    /// The roles granted to it.
    pub roles: BTreeMap<AccountName, AdminOption>,
    /// The roles active when it logs in. MariaDB allows one.
    pub default_roles: BTreeSet<AccountName>,
    /// Privileges on `*.*`.
    pub global: PrivilegeSet,
    /// Privileges on `db.*`, by the database name or pattern as granted (`shop`, `shop\_%`).
    pub databases: BTreeMap<String, PrivilegeSet>,
    pub objects: BTreeSet<ObjectPrivilege>,
    /// The accounts it is granted to, for a role: they lose it when it is dropped.
    pub members: BTreeSet<AccountName>,
    /// What it is the definer of, which fails once it is dropped.
    pub defines: Vec<DefinedObject>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_round_trips_through_the_last_at() {
        let cases = [
            (AccountName::new("app", "%"), "app@%"),
            (
                AccountName::new("ops@example.com", "10.0.%"),
                "ops@example.com@10.0.%",
            ),
            (AccountName::new("app_read", ""), "app_read"),
        ];
        for (name, label) in cases {
            assert_eq!(name.label(), label);
            assert_eq!(AccountName::from_label(label), name);
        }
    }

    #[test]
    fn a_role_has_a_host_on_mysql_only() {
        assert_eq!(
            AccountName::role("r", MysqlFlavor::Mysql),
            AccountName::new("r", "%")
        );
        assert_eq!(
            AccountName::role("r", MysqlFlavor::MariaDb),
            AccountName::new("r", "")
        );
    }

    #[test]
    fn privileges_are_named_as_the_server_lists_them() {
        assert_eq!(Privilege::new(" create   user ").as_str(), "CREATE USER");
        assert!(Privilege::new("create user").is_administrative());
        assert!(Privilege::new("system_user").is_administrative());
        assert!(!Privilege::new("select").is_administrative());
    }
}
