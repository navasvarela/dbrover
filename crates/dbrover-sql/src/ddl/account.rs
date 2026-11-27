//! MySQL's and MariaDB's accounts and roles: `CREATE USER`, `ALTER USER`, `GRANT`, `REVOKE`,
//! `SET DEFAULT ROLE`, `RENAME USER` and `DROP USER` (W18, `docs/users-beyond-postgres.md`).
//!
//! Account names are always written with backquotes, `` `app`@`%` ``, never `'app'@'%'`:
//! the history's redaction hides every string literal after `IDENTIFIED` to the end of the
//! clause, and MySQL's grammar puts options such as `DEFAULT ROLE` after the password, so a
//! name written as a literal there would be redacted with it.

use std::collections::{BTreeMap, BTreeSet};

use dbrover_core::{
    AccountDescription, AccountKind, AccountLimits, AccountName, AdminOption, FailedLoginLock,
    LockTime, MysqlFlavor, ObjectLevel, ObjectPrivilege, Password, PasswordLifetime, Privilege,
    PrivilegeSet,
};

use super::{DdlError, require_name};
use crate::dialect::UserModel;
use crate::{AccountHash, Dialect};

/// An account's settings apart from its name and password: what New user and New role create,
/// and what Edit compares with the account's description to find what changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSpec {
    pub kind: AccountKind,
    pub locked: bool,
    /// The password must be changed at the next login. Only ever set, never cleared: a new
    /// password clears it.
    pub password_expired: bool,
    pub lifetime: PasswordLifetime,
    /// MySQL only.
    pub failed_login: Option<FailedLoginLock>,
    pub limits: AccountLimits,
    pub roles: BTreeMap<AccountName, AdminOption>,
    /// Among `roles`. MariaDB allows one.
    pub default_roles: BTreeSet<AccountName>,
    pub global: PrivilegeSet,
    pub databases: BTreeMap<String, PrivilegeSet>,
    /// Privileges on tables, columns and routines: Edit may take some away, never add one.
    pub objects: BTreeSet<ObjectPrivilege>,
}

impl AccountSpec {
    /// What a new account of `kind` starts as: no privileges, no limits, the server's
    /// defaults.
    pub fn new(kind: AccountKind) -> Self {
        Self {
            kind,
            locked: false,
            password_expired: false,
            lifetime: PasswordLifetime::ServerDefault,
            failed_login: None,
            limits: AccountLimits::default(),
            roles: BTreeMap::new(),
            default_roles: BTreeSet::new(),
            global: PrivilegeSet::default(),
            databases: BTreeMap::new(),
            objects: BTreeSet::new(),
        }
    }
}

impl From<&AccountDescription> for AccountSpec {
    fn from(account: &AccountDescription) -> Self {
        Self {
            kind: account.kind,
            locked: account.locked,
            password_expired: account.password_expired,
            lifetime: account.lifetime,
            failed_login: account.failed_login,
            limits: account.limits,
            roles: account.roles.clone(),
            default_roles: account.default_roles.clone(),
            global: account.global.clone(),
            databases: account.databases.clone(),
            objects: account.objects.clone(),
        }
    }
}

/// The password plugins DbRover writes passwords for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PasswordPlugin {
    /// MySQL's default since 8.0.
    CachingSha2,
    /// MariaDB's default; MySQL's where the server still has it on.
    MysqlNative,
    /// MariaDB's `ed25519`, where it is installed. DbRover doesn't hash for it.
    Ed25519,
}

impl PasswordPlugin {
    pub fn name(self) -> &'static str {
        match self {
            Self::CachingSha2 => "caching_sha2_password",
            Self::MysqlNative => "mysql_native_password",
            Self::Ed25519 => "ed25519",
        }
    }
}

/// A new password for an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountPassword {
    /// Hashed by DbRover in the plugin's form, so the password itself is never sent.
    Hashed(AccountHash),
    /// Sent as typed, for a server whose password checks need to read it (question 5 of the
    /// plan) or a plugin DbRover doesn't hash for. The dialog sends it only over an encrypted
    /// connection.
    AsTyped {
        plugin: PasswordPlugin,
        password: Password,
    },
}

/// A change to MySQL's or MariaDB's accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountOperation {
    Create {
        name: AccountName,
        spec: AccountSpec,
        /// `None` for a role, or a user who logs in by another plugin's means.
        password: Option<AccountPassword>,
    },
    /// Changes an account from `before`, as its description reads, to `after`.
    Alter {
        name: AccountName,
        before: AccountSpec,
        after: AccountSpec,
        password: Option<AccountPassword>,
    },
    Rename {
        name: AccountName,
        new_name: AccountName,
    },
    SetLocked {
        name: AccountName,
        locked: bool,
    },
    ExpirePassword(AccountName),
    Drop {
        name: AccountName,
        kind: AccountKind,
    },
}

/// The statements for `operation` on a server of `flavor`.
pub(super) fn statements(
    flavor: MysqlFlavor,
    operation: &AccountOperation,
    dialect: Dialect,
) -> Result<Vec<String>, DdlError> {
    if dialect.capabilities().users != UserModel::Accounts {
        return Err(DdlError::Unsupported {
            what: "accounts DbRover can create or change",
            dialect,
        });
    }
    let writer = Writer { flavor };
    match operation {
        AccountOperation::Create {
            name,
            spec,
            password,
        } => writer.create(name, spec, password.as_ref()),
        AccountOperation::Alter {
            name,
            before,
            after,
            password,
        } => writer.alter(name, before, after, password.as_ref()),
        AccountOperation::Rename { name, new_name } => {
            require_account(name)?;
            require_account(new_name)?;
            Ok(vec![format!(
                "RENAME USER {} TO {}",
                account(name),
                account(new_name)
            )])
        }
        AccountOperation::SetLocked { name, locked } => {
            require_account(name)?;
            Ok(vec![format!(
                "ALTER USER {} {}",
                account(name),
                lock_option(*locked)
            )])
        }
        AccountOperation::ExpirePassword(name) => {
            require_account(name)?;
            Ok(vec![expire_now(name)])
        }
        AccountOperation::Drop { name, kind } => {
            require_account(name)?;
            let object = match kind {
                AccountKind::User => "USER",
                AccountKind::Role => "ROLE",
            };
            Ok(vec![format!("DROP {object} {}", account(name))])
        }
    }
}

struct Writer {
    flavor: MysqlFlavor,
}

impl Writer {
    fn create(
        &self,
        name: &AccountName,
        spec: &AccountSpec,
        password: Option<&AccountPassword>,
    ) -> Result<Vec<String>, DdlError> {
        require_account(name)?;
        self.check(spec)?;
        if !spec.objects.is_empty() {
            return Err(DdlError::ObjectGrants);
        }
        let mut statements = vec![match spec.kind {
            AccountKind::Role => format!("CREATE ROLE {}", account(name)),
            AccountKind::User => {
                let mut options = Vec::new();
                if let Some(password) = password {
                    options.push(self.identified(password)?);
                }
                options.extend(limit_options(&AccountLimits::default(), &spec.limits));
                if spec.locked {
                    options.push(lock_option(true).to_owned());
                }
                if spec.lifetime != PasswordLifetime::ServerDefault {
                    options.push(lifetime_option(spec.lifetime));
                }
                if let Some(lock) = spec.failed_login {
                    options.push(failed_login_option(Some(lock)));
                }
                join_options(format!("CREATE USER {}", account(name)), &options)
            }
        }];
        if spec.kind == AccountKind::User && spec.password_expired {
            statements.push(expire_now(name));
        }
        let empty = AccountSpec::new(spec.kind);
        statements.extend(privilege_changes(name, &empty, spec)?);
        statements.extend(self.role_changes(name, &empty, spec));
        if !spec.default_roles.is_empty() {
            statements.push(self.default_roles(name, &spec.default_roles));
        }
        Ok(statements)
    }

    /// The account's own change first, as the one most likely to be refused, then what it
    /// loses, then what it gains, then its default roles. MySQL commits each statement as it
    /// runs, so this order leaves the least behind when one fails.
    fn alter(
        &self,
        name: &AccountName,
        before: &AccountSpec,
        after: &AccountSpec,
        password: Option<&AccountPassword>,
    ) -> Result<Vec<String>, DdlError> {
        require_account(name)?;
        self.check(after)?;
        if !after.objects.is_subset(&before.objects) {
            return Err(DdlError::ObjectGrants);
        }
        let mut options = Vec::new();
        if let Some(password) = password {
            options.push(self.identified(password)?);
        }
        options.extend(limit_options(&before.limits, &after.limits));
        if before.locked != after.locked {
            options.push(lock_option(after.locked).to_owned());
        }
        if before.lifetime != after.lifetime {
            options.push(lifetime_option(after.lifetime));
        }
        if before.failed_login != after.failed_login {
            options.push(failed_login_option(after.failed_login));
        }
        let mut statements = Vec::new();
        if !options.is_empty() {
            statements.push(join_options(
                format!("ALTER USER {}", account(name)),
                &options,
            ));
        }
        if after.password_expired && !before.password_expired {
            statements.push(expire_now(name));
        }
        statements.extend(privilege_changes(name, before, after)?);
        statements.extend(self.role_changes(name, before, after));
        if before.default_roles != after.default_roles {
            statements.push(self.default_roles(name, &after.default_roles));
        }
        if statements.is_empty() {
            return Err(DdlError::NothingChanged);
        }
        Ok(statements)
    }

    /// What the spec asks for that this server can't do.
    fn check(&self, spec: &AccountSpec) -> Result<(), DdlError> {
        if let Some(role) = spec
            .default_roles
            .iter()
            .find(|role| !spec.roles.contains_key(*role))
        {
            return Err(DdlError::DefaultRoleNotGranted(role.label()));
        }
        if self.flavor == MysqlFlavor::MariaDb {
            if spec.default_roles.len() > 1 {
                return Err(DdlError::ServerLacks {
                    server: "MariaDB",
                    what: "more than one default role",
                });
            }
            if spec.failed_login.is_some() {
                return Err(DdlError::ServerLacks {
                    server: "MariaDB",
                    what: "a lock after failed logins per account",
                });
            }
        }
        Ok(())
    }

    /// `IDENTIFIED …` for `password`, in this server's spelling.
    fn identified(&self, password: &AccountPassword) -> Result<String, DdlError> {
        let mariadb = |what| DdlError::ServerLacks {
            server: "MariaDB",
            what,
        };
        Ok(match (self.flavor, password) {
            (MysqlFlavor::Mysql, AccountPassword::Hashed(hash)) => format!(
                "IDENTIFIED WITH {} AS {}",
                hash.plugin(),
                literal(hash.as_str())
            ),
            (MysqlFlavor::MariaDb, AccountPassword::Hashed(AccountHash::MysqlNative(hash))) => {
                format!("IDENTIFIED BY PASSWORD {}", literal(hash))
            }
            (MysqlFlavor::MariaDb, AccountPassword::Hashed(AccountHash::CachingSha2(_))) => {
                return Err(mariadb("caching_sha2_password hashes DbRover writes"));
            }
            (MysqlFlavor::Mysql, AccountPassword::AsTyped { plugin, password }) => {
                if *plugin == PasswordPlugin::Ed25519 {
                    return Err(DdlError::ServerLacks {
                        server: "MySQL",
                        what: "ed25519 plugin",
                    });
                }
                format!(
                    "IDENTIFIED WITH {} BY {}",
                    plugin.name(),
                    literal(password.expose())
                )
            }
            (MysqlFlavor::MariaDb, AccountPassword::AsTyped { plugin, password }) => match plugin {
                PasswordPlugin::MysqlNative => {
                    format!("IDENTIFIED BY {}", literal(password.expose()))
                }
                PasswordPlugin::Ed25519 => format!(
                    "IDENTIFIED VIA ed25519 USING PASSWORD({})",
                    literal(password.expose())
                ),
                PasswordPlugin::CachingSha2 => {
                    return Err(mariadb("caching_sha2_password accounts DbRover writes"));
                }
            },
        })
    }

    /// `REVOKE` for the roles taken away (or whose admin option is), then `GRANT` for the
    /// roles given, those with admin option in a statement of their own. A role's admin
    /// option is taken away by revoking the role and granting it again, which both servers
    /// accept (MySQL has no `REVOKE ADMIN OPTION FOR`).
    fn role_changes(
        &self,
        name: &AccountName,
        before: &AccountSpec,
        after: &AccountSpec,
    ) -> Vec<String> {
        let revoked: Vec<&AccountName> = before
            .roles
            .iter()
            .filter(|(role, admin)| {
                after
                    .roles
                    .get(*role)
                    .is_none_or(|now| **admin == AdminOption::With && *now == AdminOption::Without)
            })
            .map(|(role, _)| role)
            .collect();
        let granted = |option: AdminOption| -> Vec<&AccountName> {
            after
                .roles
                .iter()
                .filter(|(role, admin)| {
                    **admin == option
                        && (before.roles.get(*role) != Some(admin) || revoked.contains(role))
                })
                .map(|(role, _)| role)
                .collect()
        };
        let mut statements = Vec::new();
        let target = account(name);
        if !revoked.is_empty() {
            statements.push(format!("REVOKE {} FROM {target}", self.roles(&revoked)));
        }
        for (option, suffix) in [
            (AdminOption::Without, ""),
            (AdminOption::With, " WITH ADMIN OPTION"),
        ] {
            let roles = granted(option);
            if !roles.is_empty() {
                statements.push(format!("GRANT {} TO {target}{suffix}", self.roles(&roles)));
            }
        }
        statements
    }

    fn roles(&self, roles: &[&AccountName]) -> String {
        roles
            .iter()
            .map(|role| account(role))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `SET DEFAULT ROLE`, with `NONE` for none: MySQL's `… TO`, MariaDB's `… FOR`.
    fn default_roles(&self, name: &AccountName, roles: &BTreeSet<AccountName>) -> String {
        let list = if roles.is_empty() {
            "NONE".to_owned()
        } else {
            self.roles(&roles.iter().collect::<Vec<_>>())
        };
        let preposition = match self.flavor {
            MysqlFlavor::Mysql => "TO",
            MysqlFlavor::MariaDb => "FOR",
        };
        format!("SET DEFAULT ROLE {list} {preposition} {}", account(name))
    }
}

/// `REVOKE` for every privilege taken away and `GRANT` for every one given, level by level:
/// global, then each database, then (revoked only) tables, columns and routines.
fn privilege_changes(
    name: &AccountName,
    before: &AccountSpec,
    after: &AccountSpec,
) -> Result<Vec<String>, DdlError> {
    let target = account(name);
    let mut revokes = Vec::new();
    let mut grants = Vec::new();
    let empty = PrivilegeSet::default();
    let mut levels: Vec<(String, &PrivilegeSet, &PrivilegeSet)> =
        vec![("*.*".to_owned(), &before.global, &after.global)];
    let databases: BTreeSet<&String> = before
        .databases
        .keys()
        .chain(after.databases.keys())
        .collect();
    for database in databases {
        if database.trim().is_empty() {
            return Err(DdlError::MissingName("database"));
        }
        levels.push((
            format!("{}.*", identifier(database)),
            before.databases.get(database).unwrap_or(&empty),
            after.databases.get(database).unwrap_or(&empty),
        ));
    }
    for (on, before, after) in levels {
        let taken = privilege_list(before.privileges.difference(&after.privileges));
        if !taken.is_empty() {
            revokes.push(format!("REVOKE {taken} ON {on} FROM {target}"));
        }
        if before.grant_option && !after.grant_option {
            revokes.push(format!("REVOKE GRANT OPTION ON {on} FROM {target}"));
        }
        let given = privilege_list(after.privileges.difference(&before.privileges));
        let grant_option = after.grant_option && !before.grant_option;
        if !given.is_empty() || grant_option {
            // `USAGE` grants nothing, so it gives the grant option alone.
            let given = if given.is_empty() {
                "USAGE".to_owned()
            } else {
                given
            };
            // A grant option already held at this level isn't lost by a grant without it, and
            // covers what is granted now too, so it is written only when it is new.
            let suffix = if grant_option {
                " WITH GRANT OPTION"
            } else {
                ""
            };
            grants.push(format!("GRANT {given} ON {on} TO {target}{suffix}"));
        }
    }
    for object in before.objects.difference(&after.objects) {
        revokes.push(object_revoke(object, &target));
    }
    revokes.extend(grants);
    Ok(revokes)
}

fn object_revoke(object: &ObjectPrivilege, target: &str) -> String {
    let privilege = &object.privilege;
    match &object.level {
        ObjectLevel::Table { database, table } => format!(
            "REVOKE {privilege} ON {}.{} FROM {target}",
            identifier(database),
            identifier(table)
        ),
        ObjectLevel::Column {
            database,
            table,
            column,
        } => format!(
            "REVOKE {privilege} ({}) ON {}.{} FROM {target}",
            identifier(column),
            identifier(database),
            identifier(table)
        ),
        ObjectLevel::Routine {
            database,
            name,
            kind,
        } => format!(
            "REVOKE {privilege} ON {} {}.{} FROM {target}",
            kind.keyword(),
            identifier(database),
            identifier(name)
        ),
    }
}

fn privilege_list<'a>(privileges: impl Iterator<Item = &'a Privilege>) -> String {
    privileges
        .map(Privilege::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `WITH MAX_… n` for each limit that differs from `before`.
fn limit_options(before: &AccountLimits, after: &AccountLimits) -> Vec<String> {
    let limits = [
        (
            "MAX_QUERIES_PER_HOUR",
            before.max_queries_per_hour,
            after.max_queries_per_hour,
        ),
        (
            "MAX_UPDATES_PER_HOUR",
            before.max_updates_per_hour,
            after.max_updates_per_hour,
        ),
        (
            "MAX_CONNECTIONS_PER_HOUR",
            before.max_connections_per_hour,
            after.max_connections_per_hour,
        ),
        (
            "MAX_USER_CONNECTIONS",
            before.max_user_connections,
            after.max_user_connections,
        ),
    ];
    let changed: Vec<String> = limits
        .into_iter()
        .filter(|(_, before, after)| before != after)
        .map(|(keyword, _, after)| format!("{keyword} {after}"))
        .collect();
    if changed.is_empty() {
        Vec::new()
    } else {
        vec![format!("WITH {}", changed.join(" "))]
    }
}

fn lock_option(locked: bool) -> &'static str {
    if locked {
        "ACCOUNT LOCK"
    } else {
        "ACCOUNT UNLOCK"
    }
}

fn lifetime_option(lifetime: PasswordLifetime) -> String {
    match lifetime {
        PasswordLifetime::ServerDefault => "PASSWORD EXPIRE DEFAULT".to_owned(),
        PasswordLifetime::Never => "PASSWORD EXPIRE NEVER".to_owned(),
        PasswordLifetime::Days(days) => format!("PASSWORD EXPIRE INTERVAL {days} DAY"),
    }
}

/// `0` attempts turns the lock off.
fn failed_login_option(lock: Option<FailedLoginLock>) -> String {
    match lock {
        None => "FAILED_LOGIN_ATTEMPTS 0 PASSWORD_LOCK_TIME 0".to_owned(),
        Some(FailedLoginLock { attempts, lock }) => {
            let time = match lock {
                LockTime::Days(days) => days.to_string(),
                LockTime::Unbounded => "UNBOUNDED".to_owned(),
            };
            format!("FAILED_LOGIN_ATTEMPTS {attempts} PASSWORD_LOCK_TIME {time}")
        }
    }
}

fn expire_now(name: &AccountName) -> String {
    format!("ALTER USER {} PASSWORD EXPIRE", account(name))
}

fn join_options(head: String, options: &[String]) -> String {
    if options.is_empty() {
        head
    } else {
        format!("{head} {}", options.join(" "))
    }
}

/// `` `user`@`host` ``, or `` `role` `` for a MariaDB role, which has no host.
pub(crate) fn account(name: &AccountName) -> String {
    if name.host.is_empty() {
        identifier(&name.user)
    } else {
        format!("{}@{}", identifier(&name.user), identifier(&name.host))
    }
}

fn identifier(name: &str) -> String {
    Dialect::Mysql.quoting().identifier(name)
}

fn literal(text: &str) -> String {
    Dialect::Mysql.quoting().string_literal(text)
}

fn require_account(name: &AccountName) -> Result<(), DdlError> {
    require_name(&name.user, "account")
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use super::*;
    use crate::DdlOperation;

    fn days(days: u16) -> NonZeroU16 {
        NonZeroU16::new(days).unwrap()
    }

    fn app() -> AccountName {
        AccountName::new("app", "%")
    }

    fn privileges(names: &[&str], grant_option: bool) -> PrivilegeSet {
        PrivilegeSet {
            privileges: names.iter().map(|name| Privilege::new(name)).collect(),
            grant_option,
        }
    }

    fn run(flavor: MysqlFlavor, operation: AccountOperation) -> Result<Vec<String>, DdlError> {
        DdlOperation::Account { flavor, operation }.statements(Dialect::Mysql)
    }

    fn user_with_everything(flavor: MysqlFlavor) -> AccountSpec {
        let reader = AccountName::role("app_read", flavor);
        AccountSpec {
            roles: BTreeMap::from([
                (reader.clone(), AdminOption::Without),
                (AccountName::role("app_admin", flavor), AdminOption::With),
            ]),
            default_roles: BTreeSet::from([reader]),
            global: privileges(&["process"], false),
            databases: BTreeMap::from([(
                "shop".to_owned(),
                privileges(&["select", "insert"], true),
            )]),
            ..AccountSpec::new(AccountKind::User)
        }
    }

    #[test]
    fn creates_a_user_with_a_hash_privileges_and_roles_on_each_flavor() {
        let hash = AccountHash::mysql_native("pw");
        let cases = [
            (
                MysqlFlavor::Mysql,
                vec![
                    "CREATE USER `app`@`%` IDENTIFIED WITH mysql_native_password AS \
                     '*D821809F681A40A6E379B50D0463EFAE20BDD122'",
                    "GRANT PROCESS ON *.* TO `app`@`%`",
                    "GRANT INSERT, SELECT ON `shop`.* TO `app`@`%` WITH GRANT OPTION",
                    "GRANT `app_read`@`%` TO `app`@`%`",
                    "GRANT `app_admin`@`%` TO `app`@`%` WITH ADMIN OPTION",
                    "SET DEFAULT ROLE `app_read`@`%` TO `app`@`%`",
                ],
            ),
            (
                MysqlFlavor::MariaDb,
                vec![
                    "CREATE USER `app`@`%` IDENTIFIED BY PASSWORD \
                     '*D821809F681A40A6E379B50D0463EFAE20BDD122'",
                    "GRANT PROCESS ON *.* TO `app`@`%`",
                    "GRANT INSERT, SELECT ON `shop`.* TO `app`@`%` WITH GRANT OPTION",
                    "GRANT `app_read` TO `app`@`%`",
                    "GRANT `app_admin` TO `app`@`%` WITH ADMIN OPTION",
                    "SET DEFAULT ROLE `app_read` FOR `app`@`%`",
                ],
            ),
        ];
        for (flavor, expected) in cases {
            let statements = run(
                flavor,
                AccountOperation::Create {
                    name: app(),
                    spec: user_with_everything(flavor),
                    password: Some(AccountPassword::Hashed(hash.clone())),
                },
            );
            assert_eq!(statements.unwrap(), expected, "{flavor:?}");
        }
    }

    #[test]
    fn writes_only_the_options_that_differ_from_the_servers_defaults() {
        let spec = AccountSpec {
            locked: true,
            password_expired: true,
            lifetime: PasswordLifetime::Days(days(90)),
            failed_login: Some(FailedLoginLock {
                attempts: days(3),
                lock: LockTime::Unbounded,
            }),
            limits: AccountLimits {
                max_user_connections: 5,
                max_queries_per_hour: 100,
                ..AccountLimits::default()
            },
            ..AccountSpec::new(AccountKind::User)
        };
        let statements = run(
            MysqlFlavor::Mysql,
            AccountOperation::Create {
                name: app(),
                spec,
                password: Some(AccountPassword::AsTyped {
                    plugin: PasswordPlugin::CachingSha2,
                    password: Password::new("it's"),
                }),
            },
        );
        assert_eq!(
            statements.unwrap(),
            [
                "CREATE USER `app`@`%` IDENTIFIED WITH caching_sha2_password BY 'it''s' \
                 WITH MAX_QUERIES_PER_HOUR 100 MAX_USER_CONNECTIONS 5 ACCOUNT LOCK \
                 PASSWORD EXPIRE INTERVAL 90 DAY FAILED_LOGIN_ATTEMPTS 3 PASSWORD_LOCK_TIME UNBOUNDED",
                "ALTER USER `app`@`%` PASSWORD EXPIRE",
            ]
        );
        let bare = run(
            MysqlFlavor::Mysql,
            AccountOperation::Create {
                name: app(),
                spec: AccountSpec::new(AccountKind::User),
                password: None,
            },
        );
        assert_eq!(bare.unwrap(), ["CREATE USER `app`@`%`"]);
    }

    #[test]
    fn creates_a_role_with_a_host_on_mysql_only() {
        for (flavor, expected) in [
            (MysqlFlavor::Mysql, "CREATE ROLE `reporting`@`%`"),
            (MysqlFlavor::MariaDb, "CREATE ROLE `reporting`"),
        ] {
            let statements = run(
                flavor,
                AccountOperation::Create {
                    name: AccountName::role("reporting", flavor),
                    spec: AccountSpec::new(AccountKind::Role),
                    password: None,
                },
            );
            assert_eq!(statements.unwrap(), [expected]);
        }
    }

    #[test]
    fn alters_the_account_first_then_revokes_then_grants() {
        let flavor = MysqlFlavor::Mysql;
        let column = ObjectPrivilege {
            level: ObjectLevel::Column {
                database: "shop".to_owned(),
                table: "orders".to_owned(),
                column: "total".to_owned(),
            },
            privilege: Privilege::new("update"),
        };
        let routine = ObjectPrivilege {
            level: ObjectLevel::Routine {
                database: "shop".to_owned(),
                name: "refund".to_owned(),
                kind: dbrover_core::RoutineKind::Procedure,
            },
            privilege: Privilege::new("execute"),
        };
        let before = AccountSpec {
            objects: BTreeSet::from([column, routine.clone()]),
            ..user_with_everything(flavor)
        };
        let mut after = before.clone();
        after.locked = true;
        after.lifetime = PasswordLifetime::Never;
        after.global = privileges(&[], true);
        after.databases.remove("shop");
        after
            .databases
            .insert("sales".to_owned(), privileges(&["select"], false));
        after
            .roles
            .insert(AccountName::role("app_admin", flavor), AdminOption::Without);
        after.default_roles.clear();
        after.objects = BTreeSet::from([routine]);
        let statements = run(
            flavor,
            AccountOperation::Alter {
                name: app(),
                before,
                after,
                password: None,
            },
        );
        assert_eq!(
            statements.unwrap(),
            [
                "ALTER USER `app`@`%` ACCOUNT LOCK PASSWORD EXPIRE NEVER",
                "REVOKE PROCESS ON *.* FROM `app`@`%`",
                "REVOKE INSERT, SELECT ON `shop`.* FROM `app`@`%`",
                "REVOKE GRANT OPTION ON `shop`.* FROM `app`@`%`",
                "REVOKE UPDATE (`total`) ON `shop`.`orders` FROM `app`@`%`",
                "GRANT USAGE ON *.* TO `app`@`%` WITH GRANT OPTION",
                "GRANT SELECT ON `sales`.* TO `app`@`%`",
                "REVOKE `app_admin`@`%` FROM `app`@`%`",
                "GRANT `app_admin`@`%` TO `app`@`%`",
                "SET DEFAULT ROLE NONE TO `app`@`%`",
            ]
        );
    }

    #[test]
    fn revokes_privileges_on_tables_and_routines() {
        let target = "`app`@`%`";
        let table = ObjectPrivilege {
            level: ObjectLevel::Table {
                database: "shop".to_owned(),
                table: "orders".to_owned(),
            },
            privilege: Privilege::new("select"),
        };
        assert_eq!(
            object_revoke(&table, target),
            "REVOKE SELECT ON `shop`.`orders` FROM `app`@`%`"
        );
        let function = ObjectPrivilege {
            level: ObjectLevel::Routine {
                database: "shop".to_owned(),
                name: "tax".to_owned(),
                kind: dbrover_core::RoutineKind::Function,
            },
            privilege: Privilege::new("execute"),
        };
        assert_eq!(
            object_revoke(&function, target),
            "REVOKE EXECUTE ON FUNCTION `shop`.`tax` FROM `app`@`%`"
        );
    }

    #[test]
    fn changes_the_password_and_the_plugin_with_it() {
        let spec = AccountSpec::new(AccountKind::User);
        let alter = |flavor, plugin| {
            run(
                flavor,
                AccountOperation::Alter {
                    name: app(),
                    before: spec.clone(),
                    after: spec.clone(),
                    password: Some(AccountPassword::AsTyped {
                        plugin,
                        password: Password::new("pw"),
                    }),
                },
            )
        };
        assert_eq!(
            alter(MysqlFlavor::MariaDb, PasswordPlugin::Ed25519).unwrap(),
            ["ALTER USER `app`@`%` IDENTIFIED VIA ed25519 USING PASSWORD('pw')"]
        );
        assert_eq!(
            alter(MysqlFlavor::MariaDb, PasswordPlugin::MysqlNative).unwrap(),
            ["ALTER USER `app`@`%` IDENTIFIED BY 'pw'"]
        );
        assert!(matches!(
            alter(MysqlFlavor::Mysql, PasswordPlugin::Ed25519),
            Err(DdlError::ServerLacks { .. })
        ));
        assert!(matches!(
            alter(MysqlFlavor::MariaDb, PasswordPlugin::CachingSha2),
            Err(DdlError::ServerLacks { .. })
        ));
    }

    #[test]
    fn refuses_what_the_server_or_w18_cant_do() {
        let flavor = MysqlFlavor::MariaDb;
        let mut two_defaults = user_with_everything(flavor);
        two_defaults.default_roles = two_defaults.roles.keys().cloned().collect();
        let mut not_granted = AccountSpec::new(AccountKind::User);
        not_granted
            .default_roles
            .insert(AccountName::role("ghost", flavor));
        let mut object = AccountSpec::new(AccountKind::User);
        object.objects.insert(ObjectPrivilege {
            level: ObjectLevel::Table {
                database: "shop".to_owned(),
                table: "t".to_owned(),
            },
            privilege: Privilege::new("select"),
        });
        let create = |spec| {
            run(
                flavor,
                AccountOperation::Create {
                    name: app(),
                    spec,
                    password: None,
                },
            )
        };
        assert!(matches!(
            create(two_defaults),
            Err(DdlError::ServerLacks { .. })
        ));
        assert_eq!(
            create(not_granted),
            Err(DdlError::DefaultRoleNotGranted("ghost".to_owned()))
        );
        assert_eq!(create(object), Err(DdlError::ObjectGrants));
        let unchanged = AccountSpec::new(AccountKind::User);
        assert_eq!(
            run(
                flavor,
                AccountOperation::Alter {
                    name: app(),
                    before: unchanged.clone(),
                    after: unchanged,
                    password: None,
                },
            ),
            Err(DdlError::NothingChanged)
        );
        assert_eq!(
            run(
                flavor,
                AccountOperation::ExpirePassword(AccountName::new(" ", "%"))
            ),
            Err(DdlError::MissingName("account"))
        );
    }

    #[test]
    fn renames_locks_expires_and_drops() {
        let flavor = MysqlFlavor::Mysql;
        let odd = AccountName::new("o`brien@corp", "10.0.%");
        let cases = [
            (
                AccountOperation::Rename {
                    name: app(),
                    new_name: odd.clone(),
                },
                "RENAME USER `app`@`%` TO `o``brien@corp`@`10.0.%`",
            ),
            (
                AccountOperation::SetLocked {
                    name: app(),
                    locked: false,
                },
                "ALTER USER `app`@`%` ACCOUNT UNLOCK",
            ),
            (
                AccountOperation::ExpirePassword(app()),
                "ALTER USER `app`@`%` PASSWORD EXPIRE",
            ),
            (
                AccountOperation::Drop {
                    name: odd,
                    kind: AccountKind::User,
                },
                "DROP USER `o``brien@corp`@`10.0.%`",
            ),
            (
                AccountOperation::Drop {
                    name: AccountName::role("r", MysqlFlavor::MariaDb),
                    kind: AccountKind::Role,
                },
                "DROP ROLE `r`",
            ),
        ];
        for (operation, expected) in cases {
            assert_eq!(run(flavor, operation).unwrap(), [expected]);
        }
    }

    #[test]
    fn only_mysqls_dialect_has_accounts() {
        let operation = DdlOperation::Account {
            flavor: MysqlFlavor::Mysql,
            operation: AccountOperation::ExpirePassword(app()),
        };
        for dialect in Dialect::ALL {
            assert_eq!(
                operation.statements(dialect).is_ok(),
                dialect == Dialect::Mysql,
                "{dialect:?}"
            );
        }
    }
}
