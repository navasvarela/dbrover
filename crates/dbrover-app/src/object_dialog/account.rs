//! MySQL's and MariaDB's accounts (W18, `docs/users-beyond-postgres.md`): New user…, New
//! role…, Edit…, Rename…, Lock or unlock…, Expire password… and Drop….
//!
//! Each form waits for what the server offers and what this login may do
//! ([`UserManagement`]); Edit, Lock and Drop wait for the account's description too.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU16;

use dbrover_core::{
    AccountDescription, AccountKind, AccountLimits, AccountName, AdminOption, CatalogPath,
    FailedLoginLock, FolderKind, LockTime, ManagedUsers, MysqlFlavor, NodeKind, ObjectLevel,
    ObjectPrivilege, PasswordLifetime, Permit, PrincipalDescription, Privilege, PrivilegeSet,
    UserManagement,
};
use dbrover_sql::{
    AccountOperation, AccountPassword, AccountSpec, DdlError, DdlOperation, Dialect,
    PasswordPlugin, PrivilegeGroup, privilege_groups,
};
use egui::{CollapsingHeader, ComboBox, DragValue, Grid, TextEdit, Ui};

use super::{Destruction, FormContext, ObjectForm};
use crate::catalog_tree::CatalogTree;

const FIELD_WIDTH: f32 = 260.0;
/// Where Expires every … days starts, MySQL's own example.
const DEFAULT_LIFETIME_DAYS: u16 = 90;
const DEFAULT_FAILED_ATTEMPTS: u16 = 3;
const DEFAULT_LOCK_DAYS: u16 = 1;
/// The host a new account starts with: any.
const ANY_HOST: &str = "%";

/// The users folder at the top of a MySQL or MariaDB data source.
pub(super) fn users_folder() -> CatalogPath {
    CatalogPath::root().child(
        NodeKind::Folder(FolderKind::Users),
        FolderKind::Users.label(),
    )
}

/// The roles folder beside it.
pub(super) fn roles_folder() -> CatalogPath {
    CatalogPath::root().child(
        NodeKind::Folder(FolderKind::Roles),
        FolderKind::Roles.label(),
    )
}

fn folder_of(kind: AccountKind) -> CatalogPath {
    match kind {
        AccountKind::User => users_folder(),
        AccountKind::Role => roles_folder(),
    }
}

/// The account at `path`, a node of the users or roles folder.
pub(super) fn account_at(path: &CatalogPath) -> Option<(AccountName, AccountKind)> {
    match path.segments() {
        [folder, node]
            if folder.kind == NodeKind::Folder(FolderKind::Users)
                && node.kind == NodeKind::User =>
        {
            Some((AccountName::from_label(&node.name), AccountKind::User))
        }
        [folder, node]
            if folder.kind == NodeKind::Folder(FolderKind::Roles)
                && node.kind == NodeKind::Role =>
        {
            Some((AccountName::from_label(&node.name), AccountKind::Role))
        }
        _ => None,
    }
}

fn noun(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::User => "user",
        AccountKind::Role => "role",
    }
}

/// What a form asked the engine for, as it arrives.
enum Loaded<T> {
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Loaded<T> {
    fn from_result<E: std::fmt::Display>(result: Result<T, E>, what: &str) -> Self {
        match result {
            Ok(value) => Self::Ready(value),
            Err(error) => Self::Failed(format!("Could not load {what}: {error}")),
        }
    }

    fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            Self::Loading | Self::Failed(_) => None,
        }
    }

    /// Draws why the value isn't there yet; `true` once it is.
    fn shown(&self, ui: &mut Ui, what: &str) -> bool {
        match self {
            Self::Loading => {
                ui.weak(format!("Loading {what}…"));
                false
            }
            Self::Failed(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
                false
            }
            Self::Ready(_) => true,
        }
    }
}

/// What the server offers and this login may do, as the account forms use it.
struct Server {
    flavor: MysqlFlavor,
    /// The password plugins offered, from those the server has active.
    plugins: Vec<PasswordPlugin>,
    password_checks: bool,
    failed_login_lock: bool,
    encrypted: Option<bool>,
    may_change: Permit,
    current: AccountName,
}

impl Server {
    fn from_management(management: &UserManagement) -> Result<Self, String> {
        let ManagedUsers::Accounts(accounts) = &management.server else {
            return Err("This server's users aren't MySQL's accounts.".to_owned());
        };
        let plugins = PasswordPlugin::ALL
            .into_iter()
            .filter(|plugin| accounts.plugins.iter().any(|name| name == plugin.name()))
            .filter(|plugin| {
                // MariaDB's own `caching_sha2_password` isn't MySQL's, whose hash DbRover
                // writes.
                accounts.flavor == MysqlFlavor::Mysql || *plugin != PasswordPlugin::CachingSha2
            })
            .collect();
        Ok(Self {
            flavor: accounts.flavor,
            plugins,
            password_checks: accounts.password_checks,
            failed_login_lock: accounts.failed_login_lock,
            encrypted: management.encrypted,
            may_change: management.may_change.clone(),
            current: AccountName::from_label(&management.current),
        })
    }

    /// Whether a password for `plugin` goes as typed: when the server checks passwords, which
    /// it can't do with a hash, or for a plugin DbRover doesn't hash for (question 5 of the
    /// plan).
    fn as_typed(&self, plugin: PasswordPlugin) -> bool {
        self.password_checks || !plugin.hashed()
    }

    /// The sentence under the password saying how it is sent.
    fn how_sent(&self, plugin: PasswordPlugin) -> String {
        if !self.as_typed(plugin) {
            return format!(
                "Hashed on this computer for {}: the password itself is never sent.",
                plugin.name()
            );
        }
        let why = if self.password_checks {
            "the server checks passwords, which it can't do with a hash"
        } else {
            "DbRover doesn't hash for this plugin"
        };
        match self.encrypted {
            Some(false) => format!(
                "Would be sent as typed, since {why}; this connection isn't encrypted, so it \
                 can't be."
            ),
            _ => format!("Sent as typed over this encrypted connection, since {why}."),
        }
    }

    fn check_may_change(&self) -> Result<(), DdlError> {
        match &self.may_change {
            Permit::Allowed => Ok(()),
            Permit::Needs(privilege) => Err(DdlError::NotAllowed(privilege.clone())),
        }
    }
}

/// A password typed twice, and what it is sent as once both match. Hashing takes thousands
/// of rounds, so it runs once per change, not every frame.
#[derive(Default)]
struct PasswordEntry {
    password: String,
    confirmation: String,
    made: Option<(PasswordPlugin, bool, AccountPassword)>,
}

impl PasswordEntry {
    fn show(&mut self, ui: &mut Ui, label: &str) {
        for (text, field) in [
            (label, &mut self.password),
            ("Confirm password", &mut self.confirmation),
        ] {
            let label = ui.label(text);
            ui.add(
                TextEdit::singleline(field)
                    .password(true)
                    .desired_width(FIELD_WIDTH),
            )
            .labelled_by(label.id);
            ui.end_row();
        }
    }

    /// Makes the password again if it, the plugin or how it is sent changed.
    fn refresh(&mut self, plugin: PasswordPlugin, as_typed: bool) {
        let matches = !self.password.is_empty() && self.password == self.confirmation;
        let current = self.made.as_ref().is_some_and(|(made_for, typed, made)| {
            *made_for == plugin && *typed == as_typed && Self::holds(made, &self.password)
        });
        if !matches {
            self.made = None;
        } else if !current {
            let password = if as_typed {
                Some(AccountPassword::AsTyped {
                    plugin,
                    password: dbrover_core::Password::new(self.password.clone()),
                })
            } else {
                plugin.hash(&self.password).map(AccountPassword::Hashed)
            };
            self.made = password.map(|password| (plugin, as_typed, password));
        }
    }

    /// Whether `made` was made from `password`: a typed one is compared; a hash is trusted,
    /// since `refresh` clears it whenever the text changes.
    fn holds(made: &AccountPassword, password: &str) -> bool {
        match made {
            AccountPassword::AsTyped {
                password: typed, ..
            } => typed.expose() == password,
            AccountPassword::Hashed(_) => true,
        }
    }

    fn typed(&self) -> bool {
        !self.password.is_empty() || !self.confirmation.is_empty()
    }

    /// `None` when nothing was typed.
    fn password(&self) -> Result<Option<AccountPassword>, DdlError> {
        if !self.typed() {
            return Ok(None);
        }
        self.made
            .as_ref()
            .map(|(_, _, password)| Some(password.clone()))
            .ok_or(DdlError::PasswordsDiffer)
    }

    /// Clears what was made, so a change of text is noticed even when it hashes the same.
    fn changed(&mut self) {
        self.made = None;
    }
}

/// How long a password lasts, as the dialog asks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifetimeChoice {
    ServerDefault,
    Never,
    Days,
}

/// Everything about an account but its name and password.
struct AccountFields {
    kind: AccountKind,
    plugin: Option<PasswordPlugin>,
    locked: bool,
    expire_now: bool,
    lifetime: LifetimeChoice,
    lifetime_days: u16,
    failed_login: bool,
    failed_attempts: u16,
    lock_days: u16,
    lock_unbounded: bool,
    limits: AccountLimits,
    roles: BTreeMap<AccountName, AdminOption>,
    default_roles: BTreeSet<AccountName>,
    global: PrivilegeSet,
    databases: BTreeMap<String, PrivilegeSet>,
    objects: BTreeSet<ObjectPrivilege>,
    /// A dynamic privilege being typed, to add on `*.*`.
    other_privilege: String,
    /// The database a new row of privileges is for.
    new_database: String,
}

impl AccountFields {
    fn new(kind: AccountKind) -> Self {
        Self::from_spec(&AccountSpec::new(kind), None)
    }

    fn from_spec(spec: &AccountSpec, plugin: Option<PasswordPlugin>) -> Self {
        let (lifetime, lifetime_days) = match spec.lifetime {
            PasswordLifetime::ServerDefault => {
                (LifetimeChoice::ServerDefault, DEFAULT_LIFETIME_DAYS)
            }
            PasswordLifetime::Never => (LifetimeChoice::Never, DEFAULT_LIFETIME_DAYS),
            PasswordLifetime::Days(days) => (LifetimeChoice::Days, days.get()),
        };
        let (failed_attempts, lock_days, lock_unbounded) = match spec.failed_login {
            Some(FailedLoginLock { attempts, lock }) => match lock {
                LockTime::Days(days) => (attempts.get(), days.get(), false),
                LockTime::Unbounded => (attempts.get(), DEFAULT_LOCK_DAYS, true),
            },
            None => (DEFAULT_FAILED_ATTEMPTS, DEFAULT_LOCK_DAYS, false),
        };
        Self {
            kind: spec.kind,
            plugin,
            locked: spec.locked,
            expire_now: spec.password_expired,
            lifetime,
            lifetime_days,
            failed_login: spec.failed_login.is_some(),
            failed_attempts,
            lock_days,
            lock_unbounded,
            limits: spec.limits,
            roles: spec.roles.clone(),
            default_roles: spec.default_roles.clone(),
            global: spec.global.clone(),
            databases: spec.databases.clone(),
            objects: spec.objects.clone(),
            other_privilege: String::new(),
            new_database: String::new(),
        }
    }

    fn spec(&self) -> AccountSpec {
        let positive = |n: u16| NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN);
        let lifetime = match self.lifetime {
            LifetimeChoice::ServerDefault => PasswordLifetime::ServerDefault,
            LifetimeChoice::Never => PasswordLifetime::Never,
            LifetimeChoice::Days => PasswordLifetime::Days(positive(self.lifetime_days)),
        };
        let failed_login = self.failed_login.then(|| FailedLoginLock {
            attempts: positive(self.failed_attempts),
            lock: if self.lock_unbounded {
                LockTime::Unbounded
            } else {
                LockTime::Days(positive(self.lock_days))
            },
        });
        AccountSpec {
            kind: self.kind,
            locked: self.locked,
            password_expired: self.expire_now,
            lifetime,
            failed_login,
            limits: self.limits,
            roles: self.roles.clone(),
            default_roles: self.default_roles.clone(),
            global: self.global.clone(),
            databases: self
                .databases
                .iter()
                .filter(|(_, set)| !set.is_empty())
                .map(|(database, set)| (database.clone(), set.clone()))
                .collect(),
            objects: self.objects.clone(),
        }
    }

    /// The sections after the name, host and password.
    fn show_sections(
        &mut self,
        ui: &mut Ui,
        server: &Server,
        catalog: &CatalogTree,
        own_name: &AccountName,
    ) {
        if self.kind == AccountKind::User {
            CollapsingHeader::new("Password expiry and lock")
                .default_open(true)
                .show(ui, |ui| self.show_expiry(ui, server));
            CollapsingHeader::new("Limits").show(ui, |ui| self.show_limits(ui));
        }
        CollapsingHeader::new(format!("Roles ({})", self.roles.len()))
            .default_open(!self.roles.is_empty())
            .show(ui, |ui| self.show_roles(ui, server, catalog, own_name));
        CollapsingHeader::new("Privileges")
            .default_open(true)
            .show(ui, |ui| self.show_privileges(ui, server, catalog));
    }

    fn show_expiry(&mut self, ui: &mut Ui, server: &Server) {
        ui.checkbox(&mut self.locked, "Locked")
            .on_hover_text("ACCOUNT LOCK: nobody may log in as it until it is unlocked");
        ui.horizontal(|ui| {
            ui.label("Password expires");
            ui.radio_value(
                &mut self.lifetime,
                LifetimeChoice::ServerDefault,
                "as the server's default",
            );
            ui.radio_value(&mut self.lifetime, LifetimeChoice::Never, "never");
            ui.radio_value(&mut self.lifetime, LifetimeChoice::Days, "every");
            ui.add_enabled(
                self.lifetime == LifetimeChoice::Days,
                DragValue::new(&mut self.lifetime_days).range(1..=u16::MAX),
            );
            ui.label("days");
        });
        ui.checkbox(
            &mut self.expire_now,
            "Expire it now, so the next login must change it",
        )
        .on_hover_text("PASSWORD EXPIRE");
        if server.failed_login_lock {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.failed_login, "Lock after");
                ui.add_enabled(
                    self.failed_login,
                    DragValue::new(&mut self.failed_attempts).range(1..=32_767),
                );
                ui.label("failed logins, for");
                ui.add_enabled(
                    self.failed_login && !self.lock_unbounded,
                    DragValue::new(&mut self.lock_days).range(1..=32_767),
                );
                ui.label("days");
                ui.add_enabled(
                    self.failed_login,
                    egui::Checkbox::new(&mut self.lock_unbounded, "until unlocked"),
                );
            });
        }
    }

    fn show_limits(&mut self, ui: &mut Ui) {
        Grid::new("account_limits").num_columns(2).show(ui, |ui| {
            for (label, value) in [
                ("Connections at once", &mut self.limits.max_user_connections),
                (
                    "Connections per hour",
                    &mut self.limits.max_connections_per_hour,
                ),
                ("Queries per hour", &mut self.limits.max_queries_per_hour),
                ("Updates per hour", &mut self.limits.max_updates_per_hour),
            ] {
                ui.label(label);
                ui.add(DragValue::new(value))
                    .on_hover_text("0 for no limit");
                ui.end_row();
            }
        });
    }

    fn show_roles(
        &mut self,
        ui: &mut Ui,
        server: &Server,
        catalog: &CatalogTree,
        own_name: &AccountName,
    ) {
        let mut roles: BTreeSet<AccountName> = catalog
            .loaded_children(&roles_folder())
            .into_iter()
            .flatten()
            .filter(|node| node.kind() == Some(NodeKind::Role))
            .map(|node| AccountName::from_label(&node.label))
            .filter(|role| role != own_name)
            .collect();
        roles.extend(self.roles.keys().cloned());
        if roles.is_empty() {
            ui.weak("No roles yet.");
            return;
        }
        Grid::new("account_roles").num_columns(3).show(ui, |ui| {
            for role in roles {
                let mut granted = self.roles.contains_key(&role);
                if ui.checkbox(&mut granted, role.label()).changed() {
                    if granted {
                        self.roles.insert(role.clone(), AdminOption::Without);
                    } else {
                        self.roles.remove(&role);
                        self.default_roles.remove(&role);
                    }
                }
                let Some(admin) = self.roles.get_mut(&role) else {
                    ui.end_row();
                    continue;
                };
                let mut with_admin = *admin == AdminOption::With;
                if ui
                    .checkbox(&mut with_admin, "Admin option")
                    .on_hover_text("WITH ADMIN OPTION: may grant the role to others")
                    .changed()
                {
                    *admin = if with_admin {
                        AdminOption::With
                    } else {
                        AdminOption::Without
                    };
                }
                if self.kind == AccountKind::User {
                    let mut default = self.default_roles.contains(&role);
                    let hover = match server.flavor {
                        MysqlFlavor::Mysql => "SET DEFAULT ROLE: active when it logs in",
                        MysqlFlavor::MariaDb => {
                            "SET DEFAULT ROLE: active when it logs in; MariaDB has one"
                        }
                    };
                    if ui
                        .checkbox(&mut default, "Default")
                        .on_hover_text(hover)
                        .changed()
                    {
                        if !default {
                            self.default_roles.remove(&role);
                        } else {
                            if server.flavor == MysqlFlavor::MariaDb {
                                self.default_roles.clear();
                            }
                            self.default_roles.insert(role.clone());
                        }
                    }
                }
                ui.end_row();
            }
        });
    }

    fn show_privileges(&mut self, ui: &mut Ui, server: &Server, catalog: &CatalogTree) {
        ui.strong("On every database (*.*)");
        privilege_ticks(
            ui,
            "global",
            &mut self.global,
            &privilege_groups(server.flavor, true),
        );
        ui.horizontal(|ui| {
            let label = ui.label("Another privilege");
            ui.add(
                TextEdit::singleline(&mut self.other_privilege)
                    .desired_width(180.0)
                    .hint_text("e.g. BACKUP_ADMIN"),
            )
            .labelled_by(label.id);
            let name = self.other_privilege.trim();
            if ui
                .add_enabled(!name.is_empty(), egui::Button::new("Add"))
                .clicked()
            {
                self.global.privileges.insert(Privilege::new(name));
                self.other_privilege.clear();
            }
        });
        ui.separator();
        let groups = privilege_groups(server.flavor, false);
        let mut removed = None;
        for (database, set) in &mut self.databases {
            CollapsingHeader::new(format!("On {database}"))
                .id_salt(("account_database", database.as_str()))
                .default_open(true)
                .show(ui, |ui| {
                    privilege_ticks(ui, database, set, &groups);
                    if ui.button("Remove").clicked() {
                        removed = Some(database.clone());
                    }
                });
        }
        if let Some(database) = removed {
            self.databases.remove(&database);
        }
        self.show_new_database(ui, catalog);
        if !self.objects.is_empty() {
            ui.separator();
            ui.strong("On tables, columns and routines");
            ui.weak("Untick one to revoke it; new ones are granted from a console.");
            let mut revoked = None;
            for object in &self.objects {
                let mut kept = true;
                if ui.checkbox(&mut kept, object_label(object)).changed() {
                    revoked = Some(object.clone());
                }
            }
            if let Some(object) = revoked {
                self.objects.remove(&object);
            }
        }
    }

    fn show_new_database(&mut self, ui: &mut Ui, catalog: &CatalogTree) {
        let databases: Vec<String> = catalog
            .loaded_children(&CatalogPath::root())
            .into_iter()
            .flatten()
            .filter(|node| node.kind() == Some(NodeKind::Schema) && !node.system)
            .map(|node| node.label.clone())
            .filter(|name| !self.databases.contains_key(name))
            .collect();
        ui.horizontal(|ui| {
            ComboBox::from_id_salt("account_new_database")
                .selected_text(if self.new_database.is_empty() {
                    "Database…"
                } else {
                    self.new_database.as_str()
                })
                .show_ui(ui, |ui| {
                    for database in databases {
                        ui.selectable_value(&mut self.new_database, database.clone(), database);
                    }
                });
            if ui
                .add_enabled(
                    !self.new_database.is_empty(),
                    egui::Button::new("Add privileges on it"),
                )
                .clicked()
            {
                self.databases
                    .entry(std::mem::take(&mut self.new_database))
                    .or_default();
            }
        });
    }
}

/// The privileges of `groups` as ticks, then any other the set holds, then the grant option.
fn privilege_ticks(ui: &mut Ui, salt: &str, set: &mut PrivilegeSet, groups: &[PrivilegeGroup]) {
    let offered: BTreeSet<Privilege> = groups
        .iter()
        .flat_map(|group| group.privileges.iter().map(|name| Privilege::new(name)))
        .collect();
    let others: Vec<Privilege> = set
        .privileges
        .iter()
        .filter(|privilege| !offered.contains(*privilege))
        .cloned()
        .collect();
    let mut tick = |ui: &mut Ui, privilege: Privilege| {
        let mut on = set.privileges.contains(&privilege);
        if ui.checkbox(&mut on, privilege.as_str()).changed() {
            if on {
                set.privileges.insert(privilege);
            } else {
                set.privileges.remove(&privilege);
            }
        }
    };
    for group in groups {
        ui.push_id((salt, group.label), |ui| {
            ui.weak(group.label);
            ui.horizontal_wrapped(|ui| {
                for name in group.privileges {
                    tick(ui, Privilege::new(name));
                }
            });
        });
    }
    if !others.is_empty() {
        ui.push_id((salt, "others"), |ui| {
            ui.weak("Others");
            ui.horizontal_wrapped(|ui| {
                for privilege in others {
                    tick(ui, privilege);
                }
            });
        });
    }
    ui.push_id((salt, "grant option"), |ui| {
        ui.checkbox(&mut set.grant_option, "With grant option")
            .on_hover_text("WITH GRANT OPTION: may grant these to others");
    });
}

fn object_label(object: &ObjectPrivilege) -> String {
    match &object.level {
        ObjectLevel::Table { database, table } => {
            format!("{} on {database}.{table}", object.privilege)
        }
        ObjectLevel::Column {
            database,
            table,
            column,
        } => format!("{} on {database}.{table} ({column})", object.privilege),
        ObjectLevel::Routine {
            database,
            name,
            kind,
        } => format!(
            "{} on {} {database}.{name}",
            object.privilege,
            kind.keyword().to_ascii_lowercase()
        ),
    }
}

/// What a change gives that administers the server: privileges on `*.*` that do, and a grant
/// option newly given anywhere.
fn administration(name: &AccountName, before: &AccountSpec, after: &AccountSpec) -> Option<String> {
    let mut given: Vec<String> = after
        .global
        .privileges
        .difference(&before.global.privileges)
        .filter(|privilege| privilege.is_administrative())
        .map(|privilege| privilege.as_str().to_owned())
        .collect();
    let new_grant_option = (after.global.grant_option && !before.global.grant_option)
        || after.databases.iter().any(|(database, set)| {
            set.grant_option
                && !before
                    .databases
                    .get(database)
                    .is_some_and(|before| before.grant_option)
        });
    if new_grant_option {
        given.push("GRANT OPTION".to_owned());
    }
    (!given.is_empty()).then(|| {
        format!(
            "This gives {} administration of the server: {}.",
            name.label(),
            given.join(", ")
        )
    })
}

/// The plugin, password and how it is sent, in the form's grid.
fn show_password(
    ui: &mut Ui,
    fields: &mut AccountFields,
    password: &mut PasswordEntry,
    server: &Server,
    label: &str,
) {
    if server.plugins.is_empty() {
        ui.label("Authentication");
        ui.weak("The server has no password plugin DbRover writes passwords for.");
        ui.end_row();
        return;
    }
    let plugin = *fields.plugin.get_or_insert(server.plugins[0]);
    ui.label("Authentication");
    ComboBox::from_id_salt("account_plugin")
        .selected_text(plugin.name())
        .show_ui(ui, |ui| {
            for offered in &server.plugins {
                if ui
                    .selectable_value(&mut fields.plugin, Some(*offered), offered.name())
                    .changed()
                {
                    password.changed();
                }
            }
        });
    ui.end_row();
    let before = (password.password.clone(), password.confirmation.clone());
    password.show(ui, label);
    if before != (password.password.clone(), password.confirmation.clone()) {
        password.changed();
    }
    let plugin = fields.plugin.unwrap_or(plugin);
    password.refresh(plugin, server.as_typed(plugin));
    ui.label("");
    ui.weak(server.how_sent(plugin));
    ui.end_row();
}

/// Refuses a password that would go as typed over a connection that isn't encrypted.
fn check_password(password: Option<&AccountPassword>, server: &Server) -> Result<(), DdlError> {
    match password {
        Some(AccountPassword::AsTyped { .. }) if server.encrypted == Some(false) => {
            Err(DdlError::UnencryptedPassword)
        }
        _ => Ok(()),
    }
}

/// New user… and New role….
pub(crate) struct NewAccount {
    kind: AccountKind,
    user: String,
    host: String,
    password: PasswordEntry,
    fields: AccountFields,
    server: Loaded<Server>,
}

impl NewAccount {
    /// On the data source (the root), or its users or roles folder.
    pub(crate) fn new(path: &CatalogPath, kind: AccountKind) -> Option<Self> {
        (path.is_root() || *path == users_folder() || *path == roles_folder()).then(|| Self {
            kind,
            user: String::new(),
            host: ANY_HOST.to_owned(),
            password: PasswordEntry::default(),
            fields: AccountFields::new(kind),
            server: Loaded::Loading,
        })
    }

    fn name(&self, flavor: MysqlFlavor) -> AccountName {
        let user = self.user.trim();
        match (self.kind, flavor) {
            // A MariaDB role has no host.
            (AccountKind::Role, MysqlFlavor::MariaDb) => AccountName::role(user, flavor),
            _ => AccountName::new(user, self.host.trim()),
        }
    }
}

impl ObjectForm for NewAccount {
    fn title(&self) -> String {
        format!("New {}", noun(self.kind))
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        if !self.server.shown(ui, "what the server offers") {
            return;
        }
        let Loaded::Ready(server) = &self.server else {
            return;
        };
        if let Permit::Needs(privilege) = &server.may_change {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("This login can't create accounts: that needs {privilege}."),
            );
        }
        let flavor = server.flavor;
        Grid::new("new_account").num_columns(2).show(ui, |ui| {
            let label = ui.label("Name");
            ui.add(TextEdit::singleline(&mut self.user).desired_width(FIELD_WIDTH))
                .labelled_by(label.id);
            ui.end_row();
            if !(self.kind == AccountKind::Role && flavor == MysqlFlavor::MariaDb) {
                let label = ui.label("Host");
                ui.add(
                    TextEdit::singleline(&mut self.host)
                        .desired_width(FIELD_WIDTH)
                        .hint_text("% for any host"),
                )
                .on_hover_text("% for any host, or a name, an IP address or a mask")
                .labelled_by(label.id);
                ui.end_row();
            }
            if self.kind == AccountKind::User {
                show_password(ui, &mut self.fields, &mut self.password, server, "Password");
            }
        });
        if self.kind == AccountKind::Role && flavor == MysqlFlavor::MariaDb {
            ui.weak("MariaDB grants a new role to the account that creates it, with admin option.");
        }
        let name = self.name(flavor);
        self.fields
            .show_sections(ui, server, context.catalog, &name);
    }

    fn catalog_requests(&self) -> Vec<CatalogPath> {
        vec![roles_folder(), CatalogPath::root()]
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.server = Loaded::from_result(
            result
                .clone()
                .and_then(|management| Server::from_management(&management)),
            "what the server offers",
        );
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let Some(server) = self.server.ready() else {
            return Err(DdlError::NothingChanged);
        };
        server.check_may_change()?;
        let password = match self.kind {
            AccountKind::User => self.password.password()?,
            AccountKind::Role => None,
        };
        check_password(password.as_ref(), server)?;
        Ok(vec![DdlOperation::Account {
            flavor: server.flavor,
            operation: AccountOperation::Create {
                name: self.name(server.flavor),
                spec: self.fields.spec(),
                password,
            },
        }])
    }

    fn changed(&self) -> CatalogPath {
        folder_of(self.kind)
    }

    fn administration(&self) -> Option<String> {
        let server = self.server.ready()?;
        administration(
            &self.name(server.flavor),
            &AccountSpec::new(self.kind),
            &self.fields.spec(),
        )
    }
}

/// Edit…: the same sections, filled from the account's description.
pub(crate) struct EditAccount {
    path: CatalogPath,
    name: AccountName,
    kind: AccountKind,
    password: PasswordEntry,
    fields: AccountFields,
    before: Loaded<AccountSpec>,
    server: Loaded<Server>,
    /// The plugin the account has, by name, which may be one DbRover doesn't write.
    plugin: String,
}

impl EditAccount {
    /// On a user or role.
    pub(crate) fn new(path: CatalogPath) -> Option<Self> {
        let (name, kind) = account_at(&path)?;
        Some(Self {
            path,
            name,
            kind,
            password: PasswordEntry::default(),
            fields: AccountFields::new(kind),
            before: Loaded::Loading,
            server: Loaded::Loading,
            plugin: String::new(),
        })
    }
}

impl ObjectForm for EditAccount {
    fn title(&self) -> String {
        format!("Edit {} {}", noun(self.kind), self.name.label())
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        if !self.server.shown(ui, "what the server offers")
            || !self.before.shown(ui, &format!("the {}", noun(self.kind)))
        {
            return;
        }
        let Loaded::Ready(server) = &self.server else {
            return;
        };
        if let Permit::Needs(privilege) = &server.may_change {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("This login can't change accounts: that needs {privilege}."),
            );
        }
        if self.name == server.current {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "This is the account DbRover is connected as.",
            );
        }
        if self.kind == AccountKind::User {
            Grid::new("edit_account").num_columns(2).show(ui, |ui| {
                if !self.plugin.is_empty() && PasswordPlugin::from_name(&self.plugin).is_none() {
                    ui.label("Authentication");
                    ui.weak(format!(
                        "{}, kept unless a password is typed for another plugin",
                        self.plugin
                    ));
                    ui.end_row();
                }
                show_password(
                    ui,
                    &mut self.fields,
                    &mut self.password,
                    server,
                    "New password",
                );
            });
            ui.weak("Leave the password empty to keep the one it has.");
        }
        self.fields
            .show_sections(ui, server, context.catalog, &self.name);
    }

    fn catalog_requests(&self) -> Vec<CatalogPath> {
        vec![roles_folder(), CatalogPath::root()]
    }

    fn principal_request(&self) -> Option<CatalogPath> {
        Some(self.path.clone())
    }

    fn principal_described(&mut self, result: &Result<PrincipalDescription, String>) {
        match result {
            Ok(PrincipalDescription::Account(account)) => {
                let spec = AccountSpec::from(account);
                self.plugin.clone_from(&account.plugin);
                self.fields =
                    AccountFields::from_spec(&spec, PasswordPlugin::from_name(&account.plugin));
                // Expiring now is a choice to make, not a state to keep.
                self.fields.expire_now = false;
                self.before = Loaded::Ready(AccountSpec {
                    password_expired: false,
                    ..spec
                });
            }
            Ok(other) => {
                self.before = Loaded::Failed(format!("Not an account: {other:?}"));
            }
            Err(error) => {
                self.before = Loaded::Failed(format!("Could not load the account: {error}"));
            }
        }
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.server = Loaded::from_result(
            result
                .clone()
                .and_then(|management| Server::from_management(&management)),
            "what the server offers",
        );
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let (Some(server), Some(before)) = (self.server.ready(), self.before.ready()) else {
            return Err(DdlError::NothingChanged);
        };
        server.check_may_change()?;
        let password = self.password.password()?;
        check_password(password.as_ref(), server)?;
        Ok(vec![DdlOperation::Account {
            flavor: server.flavor,
            operation: AccountOperation::Alter {
                name: self.name.clone(),
                before: before.clone(),
                after: self.fields.spec(),
                password,
            },
        }])
    }

    fn changed(&self) -> CatalogPath {
        folder_of(self.kind)
    }

    fn administration(&self) -> Option<String> {
        administration(&self.name, self.before.ready()?, &self.fields.spec())
    }
}

/// Gives an account another name, its host included.
pub(crate) struct RenameAccount {
    name: AccountName,
    kind: AccountKind,
    user: String,
    host: String,
    server: Loaded<Server>,
}

impl RenameAccount {
    /// On a user or role.
    pub(crate) fn new(path: &CatalogPath) -> Option<Self> {
        let (name, kind) = account_at(path)?;
        Some(Self {
            user: name.user.clone(),
            host: name.host.clone(),
            name,
            kind,
            server: Loaded::Loading,
        })
    }
}

impl ObjectForm for RenameAccount {
    fn title(&self) -> String {
        format!("Rename {} {}", noun(self.kind), self.name.label())
    }

    fn show(&mut self, ui: &mut Ui, _context: &FormContext<'_>) {
        if !self.server.shown(ui, "what the server offers") {
            return;
        }
        Grid::new("rename_account").num_columns(2).show(ui, |ui| {
            let label = ui.label("New name");
            ui.add(TextEdit::singleline(&mut self.user).desired_width(FIELD_WIDTH))
                .labelled_by(label.id);
            ui.end_row();
            if !self.name.host.is_empty() {
                let label = ui.label("Host");
                ui.add(TextEdit::singleline(&mut self.host).desired_width(FIELD_WIDTH))
                    .labelled_by(label.id);
                ui.end_row();
            }
        });
        ui.weak("Its password, privileges and roles go with it.");
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.server = Loaded::from_result(
            result
                .clone()
                .and_then(|management| Server::from_management(&management)),
            "what the server offers",
        );
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let Some(server) = self.server.ready() else {
            return Err(DdlError::NothingChanged);
        };
        server.check_may_change()?;
        let new_name = AccountName::new(self.user.trim(), self.host.trim());
        if new_name == self.name {
            return Err(DdlError::NothingChanged);
        }
        Ok(vec![DdlOperation::Account {
            flavor: server.flavor,
            operation: AccountOperation::Rename {
                name: self.name.clone(),
                new_name,
            },
        }])
    }

    fn changed(&self) -> CatalogPath {
        folder_of(self.kind)
    }
}

/// Locks an unlocked account, or unlocks a locked one: one statement, shown before it runs.
pub(crate) struct LockAccount {
    path: CatalogPath,
    name: AccountName,
    locked: Loaded<bool>,
    server: Loaded<Server>,
}

impl LockAccount {
    /// On a user.
    pub(crate) fn new(path: CatalogPath) -> Option<Self> {
        let (name, kind) = account_at(&path)?;
        (kind == AccountKind::User).then_some(Self {
            path,
            name,
            locked: Loaded::Loading,
            server: Loaded::Loading,
        })
    }
}

impl ObjectForm for LockAccount {
    fn title(&self) -> String {
        match self.locked {
            Loaded::Ready(true) => format!("Unlock user {}", self.name.label()),
            _ => format!("Lock user {}", self.name.label()),
        }
    }

    fn show(&mut self, ui: &mut Ui, _context: &FormContext<'_>) {
        if !self.server.shown(ui, "what the server offers") || !self.locked.shown(ui, "the account")
        {
            return;
        }
        ui.label(match self.locked {
            Loaded::Ready(true) => "It is locked: nobody may log in as it. Unlock it?",
            _ => "Lock it, so nobody may log in as it? Sessions already open stay open.",
        });
    }

    fn principal_request(&self) -> Option<CatalogPath> {
        Some(self.path.clone())
    }

    fn principal_described(&mut self, result: &Result<PrincipalDescription, String>) {
        self.locked = match result {
            Ok(PrincipalDescription::Account(account)) => Loaded::Ready(account.locked),
            Ok(other) => Loaded::Failed(format!("Not an account: {other:?}")),
            Err(error) => Loaded::Failed(format!("Could not load the account: {error}")),
        };
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.server = Loaded::from_result(
            result
                .clone()
                .and_then(|management| Server::from_management(&management)),
            "what the server offers",
        );
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let (Some(server), Some(locked)) = (self.server.ready(), self.locked.ready()) else {
            return Err(DdlError::NothingChanged);
        };
        server.check_may_change()?;
        Ok(vec![DdlOperation::Account {
            flavor: server.flavor,
            operation: AccountOperation::SetLocked {
                name: self.name.clone(),
                locked: !locked,
            },
        }])
    }

    fn changed(&self) -> CatalogPath {
        users_folder()
    }
}

/// Expires a user's password, so its next login must set a new one.
pub(crate) struct ExpirePassword {
    name: AccountName,
    server: Loaded<Server>,
}

impl ExpirePassword {
    /// On a user.
    pub(crate) fn new(path: &CatalogPath) -> Option<Self> {
        let (name, kind) = account_at(path)?;
        (kind == AccountKind::User).then_some(Self {
            name,
            server: Loaded::Loading,
        })
    }
}

impl ObjectForm for ExpirePassword {
    fn title(&self) -> String {
        format!("Expire the password of {}", self.name.label())
    }

    fn show(&mut self, ui: &mut Ui, _context: &FormContext<'_>) {
        if self.server.shown(ui, "what the server offers") {
            ui.label(
                "Its next login must set a new password before running anything else. \
                 Sessions already open aren't affected.",
            );
        }
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.server = Loaded::from_result(
            result
                .clone()
                .and_then(|management| Server::from_management(&management)),
            "what the server offers",
        );
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let Some(server) = self.server.ready() else {
            return Err(DdlError::NothingChanged);
        };
        server.check_may_change()?;
        Ok(vec![DdlOperation::Account {
            flavor: server.flavor,
            operation: AccountOperation::ExpirePassword(self.name.clone()),
        }])
    }

    fn changed(&self) -> CatalogPath {
        users_folder()
    }
}

/// Drops an account, saying what runs as it and who loses it first.
pub(crate) struct DropAccount {
    path: CatalogPath,
    name: AccountName,
    kind: AccountKind,
    described: Loaded<AccountDescription>,
    server: Loaded<Server>,
}

impl DropAccount {
    /// On a user or role.
    pub(crate) fn new(path: CatalogPath) -> Option<Self> {
        let (name, kind) = account_at(&path)?;
        Some(Self {
            path,
            name,
            kind,
            described: Loaded::Loading,
            server: Loaded::Loading,
        })
    }

    fn consequences(&self) -> Vec<String> {
        let mut consequences = Vec::new();
        if let Some(account) = self.described.ready() {
            if !account.defines.is_empty() {
                let objects: Vec<String> = account
                    .defines
                    .iter()
                    .map(|object| {
                        format!(
                            "{}.{} ({})",
                            object.database,
                            object.name,
                            object.kind.noun()
                        )
                    })
                    .collect();
                consequences.push(format!(
                    "These run as it, and fail once it is dropped: {}.",
                    objects.join(", ")
                ));
            }
            if !account.members.is_empty() {
                let members: Vec<String> = account.members.iter().map(AccountName::label).collect();
                consequences.push(format!(
                    "These lose its privileges: {}.",
                    members.join(", ")
                ));
            }
        }
        if self
            .server
            .ready()
            .is_some_and(|server| server.current == self.name)
        {
            consequences.push(
                "It is the account DbRover is connected as: this data source can't connect \
                 as it again."
                    .to_owned(),
            );
        }
        consequences
    }
}

impl ObjectForm for DropAccount {
    fn title(&self) -> String {
        format!("Drop {} {}", noun(self.kind), self.name.label())
    }

    fn show(&mut self, ui: &mut Ui, _context: &FormContext<'_>) {
        self.server.shown(ui, "what the server offers");
        self.described.shown(ui, "what runs as it");
    }

    fn principal_request(&self) -> Option<CatalogPath> {
        Some(self.path.clone())
    }

    fn principal_described(&mut self, result: &Result<PrincipalDescription, String>) {
        self.described = match result {
            Ok(PrincipalDescription::Account(account)) => Loaded::Ready(account.clone()),
            Ok(other) => Loaded::Failed(format!("Not an account: {other:?}")),
            Err(error) => Loaded::Failed(format!("Could not load the account: {error}")),
        };
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.server = Loaded::from_result(
            result
                .clone()
                .and_then(|management| Server::from_management(&management)),
            "what the server offers",
        );
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let Some(server) = self.server.ready() else {
            return Err(DdlError::NothingChanged);
        };
        server.check_may_change()?;
        Ok(vec![DdlOperation::Account {
            flavor: server.flavor,
            operation: AccountOperation::Drop {
                name: self.name.clone(),
                kind: self.kind,
            },
        }])
    }

    fn changed(&self) -> CatalogPath {
        folder_of(self.kind)
    }

    fn destruction(&self, _dialect: Dialect) -> Option<Destruction> {
        Some(Destruction {
            name: self.name.label(),
            summary: format!("Drops the {} {}.", noun(self.kind), self.name.label()),
            consequences: self.consequences(),
        })
    }
}

#[cfg(test)]
mod tests;
