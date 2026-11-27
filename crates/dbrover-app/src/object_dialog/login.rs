//! SQL Server's logins, server roles, database users and database roles (W18,
//! `docs/users-beyond-postgres.md`): New, Edit, Rename, Disable or enable, and Drop.
//!
//! Every form waits for what this login may do ([`UserManagement`]); Edit, Drop and Disable
//! wait for the principal's description too.

use std::collections::BTreeSet;

use dbrover_core::{
    CatalogPath, DatabaseUserDescription, DatabaseUserKind, DatabaseUserName, FolderKind,
    LoginDescription, LoginKind, ManagedUsers, NodeKind, Password, PermissionGrant,
    PermissionState, Permit, PrincipalDescription, PrincipalScope, Securable,
    SqlServerRoleDescription, UserManagement, sql_server_is_administrative,
};
use dbrover_sql::{
    DatabaseUserSpec, DdlError, DdlOperation, Dialect, LoginPolicy, LoginSource, LoginSpec,
    SqlServerHash, SqlServerOperation, SqlServerPassword, UserMapping, UserSource,
};
use egui::{CollapsingHeader, ComboBox, Grid, TextEdit, Ui, WidgetType};

use super::{Destruction, FormContext, ObjectForm};
use crate::catalog_tree::CatalogTree;
use crate::explorer::SqlServerHelper;
use crate::widgets;

const FIELD_WIDTH: f32 = 260.0;
/// The schema a new user's objects go in when no other is named.
const DEFAULT_SCHEMA: &str = "dbo";
/// Who a dropped user's schemas go to unless another owner is picked.
const DEFAULT_OWNER: &str = "dbo";

pub(super) fn form(
    helper: SqlServerHelper,
    encrypted_by_settings: bool,
) -> Option<Box<dyn ObjectForm>> {
    let context = Shared::new(encrypted_by_settings);
    Some(match helper {
        SqlServerHelper::NewLogin(path) => {
            if !(path.is_root() || path == logins_folder()) {
                return None;
            }
            Box::new(NewLogin::new(context))
        }
        SqlServerHelper::NewServerRole(path) => {
            if path != server_roles_folder() {
                return None;
            }
            Box::new(NewRole::new(PrincipalScope::Server, context))
        }
        SqlServerHelper::NewUser(path) => Box::new(NewUser::new(
            database_of(&path, FolderKind::Users)?,
            context,
        )),
        SqlServerHelper::NewDatabaseRole(path) => Box::new(NewRole::new(
            PrincipalScope::Database(database_of(&path, FolderKind::Roles)?),
            context,
        )),
        SqlServerHelper::Edit(path) => match Principal::at(&path)? {
            Principal::Login(name) => Box::new(EditLogin::new(path, name, context)),
            Principal::User { database, name } => {
                Box::new(EditUser::new(path, database, name, context))
            }
            Principal::Role { scope, name } => Box::new(EditRole::new(path, scope, name, context)),
        },
        SqlServerHelper::Rename(path) => Box::new(Rename::new(Principal::at(&path)?, context)),
        SqlServerHelper::SetEnabled(path) => match Principal::at(&path)? {
            Principal::Login(name) => Box::new(SetEnabled::new(path, name, context)),
            _ => return None,
        },
        SqlServerHelper::Drop(path) => Box::new(DropPrincipal::new(
            path.clone(),
            Principal::at(&path)?,
            context,
        )),
    })
}

pub(super) fn logins_folder() -> CatalogPath {
    CatalogPath::root().child(
        NodeKind::Folder(FolderKind::Logins),
        FolderKind::Logins.label(),
    )
}

pub(super) fn server_roles_folder() -> CatalogPath {
    CatalogPath::root().child(
        NodeKind::Folder(FolderKind::ServerRoles),
        FolderKind::ServerRoles.label(),
    )
}

fn database_folder(database: &str, folder: FolderKind) -> CatalogPath {
    CatalogPath::root()
        .child(NodeKind::Database, database)
        .child(NodeKind::Folder(folder), folder.label())
}

/// The database whose `folder` is at `path`.
fn database_of(path: &CatalogPath, folder: FolderKind) -> Option<String> {
    match path.segments() {
        [database, at]
            if database.kind == NodeKind::Database && at.kind == NodeKind::Folder(folder) =>
        {
            Some(database.name.clone())
        }
        _ => None,
    }
}

/// The principal a node is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Principal {
    Login(String),
    User { database: String, name: String },
    Role { scope: PrincipalScope, name: String },
}

impl Principal {
    fn at(path: &CatalogPath) -> Option<Self> {
        match path.segments() {
            [folder, login]
                if folder.kind == NodeKind::Folder(FolderKind::Logins)
                    && login.kind == NodeKind::Login =>
            {
                Some(Self::Login(login.name.clone()))
            }
            [folder, role]
                if folder.kind == NodeKind::Folder(FolderKind::ServerRoles)
                    && role.kind == NodeKind::Role =>
            {
                Some(Self::Role {
                    scope: PrincipalScope::Server,
                    name: role.name.clone(),
                })
            }
            [database, folder, user]
                if database.kind == NodeKind::Database
                    && folder.kind == NodeKind::Folder(FolderKind::Users)
                    && user.kind == NodeKind::User =>
            {
                Some(Self::User {
                    database: database.name.clone(),
                    name: user.name.clone(),
                })
            }
            [database, folder, role]
                if database.kind == NodeKind::Database
                    && folder.kind == NodeKind::Folder(FolderKind::Roles)
                    && role.kind == NodeKind::Role =>
            {
                Some(Self::Role {
                    scope: PrincipalScope::Database(database.name.clone()),
                    name: role.name.clone(),
                })
            }
            _ => None,
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Login(name) | Self::User { name, .. } | Self::Role { name, .. } => name,
        }
    }

    fn noun(&self) -> &'static str {
        match self {
            Self::Login(_) => "login",
            Self::User { .. } => "user",
            Self::Role {
                scope: PrincipalScope::Server,
                ..
            } => "server role",
            Self::Role { .. } => "role",
        }
    }

    /// The folder that lists it, refreshed after a change.
    fn folder(&self) -> CatalogPath {
        match self {
            Self::Login(_) => logins_folder(),
            Self::User { database, .. } => database_folder(database, FolderKind::Users),
            Self::Role {
                scope: PrincipalScope::Server,
                ..
            } => server_roles_folder(),
            Self::Role {
                scope: PrincipalScope::Database(database),
                ..
            } => database_folder(database, FolderKind::Roles),
        }
    }
}

/// What a form asked the engine for, as it arrives.
enum Loaded<T> {
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Loaded<T> {
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

/// What this login may do, as the forms use it.
struct Server {
    current: String,
    encrypted: bool,
    may_change_logins: Permit,
    may_change_users: Permit,
    /// The database the connection is in, whose containment `contained` says.
    database: String,
    contained: bool,
}

/// What every form shares: what this login may do, once known, and what the data source's
/// settings say about encryption.
struct Shared {
    server: Loaded<Server>,
    encrypted_by_settings: bool,
}

impl Shared {
    fn new(encrypted_by_settings: bool) -> Self {
        Self {
            server: Loaded::Loading,
            encrypted_by_settings,
        }
    }

    fn loaded(&mut self, result: &Result<UserManagement, String>) {
        self.server = match result {
            Ok(management) => match &management.server {
                ManagedUsers::Logins(logins) => Loaded::Ready(Server {
                    current: management.current.clone(),
                    // A login without VIEW SERVER STATE isn't told; the data source's own
                    // settings answer then.
                    encrypted: management.encrypted.unwrap_or(self.encrypted_by_settings),
                    may_change_logins: management.may_change.clone(),
                    may_change_users: logins.may_change_users.clone(),
                    database: logins.database.clone(),
                    contained: logins.contained,
                }),
                ManagedUsers::Accounts(_) => {
                    Loaded::Failed("This server's users aren't SQL Server's logins.".to_owned())
                }
            },
            Err(error) => Loaded::Failed(format!("Could not load what this login may do: {error}")),
        };
    }

    /// What this login may do to `principal`'s kind: logins and server roles need `ALTER ANY
    /// LOGIN`; a user or role in the connected database `ALTER ANY USER`; elsewhere the
    /// server answers, since only the connected database was asked.
    fn check(&self, principal: &Principal) -> Result<&Server, DdlError> {
        let server = self.server.ready().ok_or(DdlError::NothingChanged)?;
        let permit = match principal {
            Principal::Login(_)
            | Principal::Role {
                scope: PrincipalScope::Server,
                ..
            } => &server.may_change_logins,
            Principal::User { database, .. }
            | Principal::Role {
                scope: PrincipalScope::Database(database),
                ..
            } if *database == server.database => &server.may_change_users,
            Principal::User { .. } | Principal::Role { .. } => &Permit::Allowed,
        };
        match permit {
            Permit::Allowed => Ok(server),
            Permit::Needs(permission) => Err(DdlError::NotAllowed(permission.clone())),
        }
    }

    /// Shows why the form can't run yet, or what this login lacks; `true` once it can show its
    /// fields.
    fn shown(&self, ui: &mut Ui, principal: &Principal) -> bool {
        if !self.server.shown(ui, "what this login may do") {
            return false;
        }
        if let Err(DdlError::NotAllowed(permission)) = self.check(principal) {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("This login can't change this: that needs {permission}."),
            );
        }
        true
    }
}

/// A password typed twice. A SQL login's is hashed once both match (`HASHED`), unless the
/// policy must check it or it must change at the next login, which a hash rules out.
#[derive(Default)]
struct PasswordEntry {
    password: String,
    confirmation: String,
    /// The hash of `password`, made once per change: SHA-512 is quick, but not per frame.
    hash: Option<(String, SqlServerHash)>,
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
        let current = self
            .hash
            .as_ref()
            .is_some_and(|(of, _)| *of == self.password);
        if !current && !self.password.is_empty() {
            self.hash = Some((self.password.clone(), SqlServerHash::hash(&self.password)));
        }
    }

    fn typed(&self) -> bool {
        !self.password.is_empty() || !self.confirmation.is_empty()
    }

    /// The password as `as_typed` says to send it; `None` when nothing was typed.
    fn password(
        &self,
        as_typed: bool,
        server: &Server,
    ) -> Result<Option<SqlServerPassword>, DdlError> {
        if !self.typed() {
            return Ok(None);
        }
        if self.password != self.confirmation {
            return Err(DdlError::PasswordsDiffer);
        }
        if as_typed {
            if !server.encrypted {
                return Err(DdlError::UnencryptedPassword);
            }
            return Ok(Some(SqlServerPassword::AsTyped(Password::new(
                self.password.clone(),
            ))));
        }
        match &self.hash {
            Some((of, hash)) if *of == self.password => {
                Ok(Some(SqlServerPassword::Hashed(hash.clone())))
            }
            _ => Err(DdlError::PasswordsDiffer),
        }
    }
}

/// How a SQL login's password is sent, said under it (question 5 of the plan).
fn how_sent(as_typed: bool) -> &'static str {
    if as_typed {
        "Sent as typed, over an encrypted connection only, so the server's password policy can \
         check it (and Must change can be set)."
    } else {
        "Hashed on this computer (HASHED): the password itself is never sent, and the server's \
         password policy can't check it."
    }
}

/// Ticks for each of `options`, `chosen` holding the ticked ones. Ticked names missing from
/// `options` are shown too, so nothing chosen is hidden.
fn ticks(ui: &mut Ui, salt: &str, chosen: &mut BTreeSet<String>, options: Vec<String>) {
    let mut all: BTreeSet<String> = options.into_iter().collect();
    all.extend(chosen.iter().cloned());
    if all.is_empty() {
        ui.weak("None yet.");
        return;
    }
    ui.push_id(salt, |ui| {
        ui.horizontal_wrapped(|ui| {
            for name in all {
                let mut on = chosen.contains(&name);
                if ui.checkbox(&mut on, &name).changed() {
                    if on {
                        chosen.insert(name);
                    } else {
                        chosen.remove(&name);
                    }
                }
            }
        });
    });
}

/// The labels of the loaded children of `folder` of `kind`, `public` left out.
fn names_in(catalog: &CatalogTree, folder: &CatalogPath, kind: NodeKind) -> Vec<String> {
    catalog
        .loaded_children(folder)
        .into_iter()
        .flatten()
        .filter(|node| node.kind() == Some(kind) && node.label != "public")
        .map(|node| node.label.clone())
        .collect()
}

fn databases(catalog: &CatalogTree) -> Vec<String> {
    names_in(catalog, &CatalogPath::root(), NodeKind::Database)
}

fn schemas(catalog: &CatalogTree, database: &str) -> Vec<String> {
    let path = CatalogPath::root().child(NodeKind::Database, database);
    catalog
        .loaded_children(&path)
        .into_iter()
        .flatten()
        .filter(|node| node.kind() == Some(NodeKind::Schema))
        .map(|node| node.label.clone())
        .collect()
}

/// A choice among `options`, named `label` for screen readers and tests.
fn combo(ui: &mut Ui, label: &str, value: &mut String, options: Vec<String>, placeholder: &str) {
    let combo = ComboBox::from_id_salt(label)
        .selected_text(if value.is_empty() {
            placeholder
        } else {
            value.as_str()
        })
        .show_ui(ui, |ui| {
            for option in options {
                ui.selectable_value(value, option.clone(), option);
            }
        });
    widgets::name(combo.response, WidgetType::ComboBox, label, true);
}

/// Permissions as ticks, each unticked one revoked (question 4 of the plan: granting is a
/// console's).
fn permission_ticks(
    ui: &mut Ui,
    permissions: &BTreeSet<PermissionGrant>,
    revoked: &mut BTreeSet<PermissionGrant>,
) {
    if permissions.is_empty() {
        return;
    }
    CollapsingHeader::new(format!("Permissions ({})", permissions.len()))
        .id_salt("permissions")
        .show(ui, |ui| {
            ui.weak("Untick one to revoke it; new ones are granted from a console.");
            for grant in permissions {
                let mut kept = !revoked.contains(grant);
                if ui.checkbox(&mut kept, permission_label(grant)).changed() {
                    if kept {
                        revoked.remove(grant);
                    } else {
                        revoked.insert(grant.clone());
                    }
                }
            }
        });
}

fn permission_label(grant: &PermissionGrant) -> String {
    let state = match grant.state {
        PermissionState::Grant => "granted",
        PermissionState::GrantWithGrantOption => "granted with grant option",
        PermissionState::Deny => "denied",
    };
    let on = match &grant.securable {
        Securable::Server => "the server".to_owned(),
        Securable::Database => "the database".to_owned(),
        Securable::Schema(schema) => format!("schema {schema}"),
        Securable::Object { schema, name } => format!("{schema}.{name}"),
        Securable::Column {
            schema,
            object,
            column,
        } => format!("{schema}.{object} ({column})"),
        Securable::Other { class, name } => format!("{} {name}", class.to_ascii_lowercase()),
    };
    format!("{} on {on}, {state}", grant.permission)
}

fn revokes(
    scope: &PrincipalScope,
    principal: &str,
    revoked: &BTreeSet<PermissionGrant>,
) -> Vec<DdlOperation> {
    revoked
        .iter()
        .map(|grant| {
            DdlOperation::SqlServer(SqlServerOperation::Revoke {
                scope: scope.clone(),
                principal: principal.to_owned(),
                grant: grant.clone(),
            })
        })
        .collect()
}

/// The roles in `added` that give administration, said before Execute.
fn administration(added: impl Iterator<Item = String>, who: &str) -> Option<String> {
    let given: Vec<String> = added
        .filter(|role| sql_server_is_administrative(role))
        .collect();
    (!given.is_empty()).then(|| format!("This gives {who} administration: {}.", given.join(", ")))
}

/// What kind of login New login makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewLoginKind {
    Sql,
    Windows,
    External,
}

/// New login…: a SQL login with its password and policy, or a Windows or Microsoft Entra one
/// by name; its server roles; and a user in each database ticked.
struct NewLogin {
    shared: Shared,
    kind: NewLoginKind,
    name: String,
    password: PasswordEntry,
    policy: LoginPolicy,
    must_change: bool,
    default_database: String,
    default_language: String,
    enabled: bool,
    server_roles: BTreeSet<String>,
    /// Each database the login gets a user in, with the user's roles.
    mappings: Vec<(String, BTreeSet<String>)>,
    new_mapping: String,
}

impl NewLogin {
    fn new(shared: Shared) -> Self {
        Self {
            shared,
            kind: NewLoginKind::Sql,
            name: String::new(),
            password: PasswordEntry::default(),
            policy: LoginPolicy::default(),
            must_change: false,
            default_database: "master".to_owned(),
            default_language: String::new(),
            enabled: true,
            server_roles: BTreeSet::new(),
            mappings: Vec::new(),
            new_mapping: String::new(),
        }
    }

    fn as_typed(&self) -> bool {
        self.policy.check_policy || self.must_change
    }

    fn principal(&self) -> Principal {
        Principal::Login(self.name.trim().to_owned())
    }
}

impl ObjectForm for NewLogin {
    fn title(&self) -> String {
        "New login".to_owned()
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        if !self.shared.shown(ui, &self.principal()) {
            return;
        }
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.kind, NewLoginKind::Sql, "SQL login");
            ui.radio_value(
                &mut self.kind,
                NewLoginKind::Windows,
                "Windows login or group",
            );
            ui.radio_value(&mut self.kind, NewLoginKind::External, "Microsoft Entra");
        });
        Grid::new("new_login").num_columns(2).show(ui, |ui| {
            let label = ui.label("Name");
            let hint = match self.kind {
                NewLoginKind::Sql => "",
                NewLoginKind::Windows => "DOMAIN\\name",
                NewLoginKind::External => "user@contoso.com, or a group's name",
            };
            ui.add(
                TextEdit::singleline(&mut self.name)
                    .desired_width(FIELD_WIDTH)
                    .hint_text(hint),
            )
            .labelled_by(label.id);
            ui.end_row();
            if self.kind == NewLoginKind::Sql {
                self.password.show(ui, "Password");
                ui.label("");
                ui.weak(how_sent(self.as_typed()));
                ui.end_row();
            }
            if self.kind != NewLoginKind::External {
                ui.label("Default database");
                combo(
                    ui,
                    "Default database",
                    &mut self.default_database,
                    databases(context.catalog),
                    "master",
                );
                ui.end_row();
                let label = ui.label("Default language");
                ui.add(
                    TextEdit::singleline(&mut self.default_language)
                        .desired_width(FIELD_WIDTH)
                        .hint_text("the server's"),
                )
                .labelled_by(label.id);
                ui.end_row();
            }
        });
        if self.kind == NewLoginKind::Sql {
            ui.checkbox(&mut self.policy.check_policy, "Enforce password policy")
                .on_hover_text("CHECK_POLICY: Windows's password policy applies; the password is then sent as typed");
            ui.checkbox(&mut self.policy.check_expiration, "Enforce expiration")
                .on_hover_text("CHECK_EXPIRATION");
            ui.checkbox(&mut self.must_change, "Must change at next login")
                .on_hover_text("MUST_CHANGE: needs the policy and expiration enforced");
        }
        ui.checkbox(&mut self.enabled, "Enabled");
        CollapsingHeader::new("Server roles").show(ui, |ui| {
            ticks(
                ui,
                "login_server_roles",
                &mut self.server_roles,
                names_in(context.catalog, &server_roles_folder(), NodeKind::Role),
            );
        });
        CollapsingHeader::new(format!("Map to databases ({})", self.mappings.len()))
            .id_salt("map to databases")
            .show(ui, |ui| {
                let mut removed = None;
                for (index, (database, roles)) in self.mappings.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.strong(database.as_str());
                        if ui.small_button("Remove").clicked() {
                            removed = Some(index);
                        }
                    });
                    let options = names_in(
                        context.catalog,
                        &database_folder(database, FolderKind::Roles),
                        NodeKind::Role,
                    );
                    ticks(ui, &format!("mapping_{database}"), roles, options);
                }
                if let Some(index) = removed {
                    self.mappings.remove(index);
                }
                ui.horizontal(|ui| {
                    let options = databases(context.catalog)
                        .into_iter()
                        .filter(|database| {
                            !self.mappings.iter().any(|(mapped, _)| mapped == database)
                        })
                        .collect();
                    combo(
                        ui,
                        "Database to map",
                        &mut self.new_mapping,
                        options,
                        "Database…",
                    );
                    if ui
                        .add_enabled(
                            !self.new_mapping.is_empty(),
                            egui::Button::new("Add a user there"),
                        )
                        .clicked()
                    {
                        self.mappings
                            .push((std::mem::take(&mut self.new_mapping), BTreeSet::new()));
                    }
                });
            });
    }

    fn catalog_requests(&self) -> Vec<CatalogPath> {
        let mut requests = vec![CatalogPath::root(), server_roles_folder()];
        requests.extend(
            self.mappings
                .iter()
                .map(|(database, _)| database_folder(database, FolderKind::Roles)),
        );
        requests
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.shared.loaded(result);
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let server = self.shared.check(&self.principal())?;
        let source = match self.kind {
            NewLoginKind::Sql => LoginSource::Sql {
                password: self
                    .password
                    .password(self.as_typed(), server)?
                    .ok_or(DdlError::MissingName("password"))?,
                must_change: self.must_change,
            },
            NewLoginKind::Windows => LoginSource::Windows,
            NewLoginKind::External => LoginSource::External,
        };
        let name = self.name.trim().to_owned();
        let language = self.default_language.trim();
        let spec = LoginSpec {
            default_database: self.default_database.clone(),
            default_language: (!language.is_empty()).then(|| language.to_owned()),
            enabled: self.enabled,
            policy: (self.kind == NewLoginKind::Sql).then_some(self.policy),
            server_roles: self.server_roles.clone(),
        };
        let mappings = self
            .mappings
            .iter()
            .map(|(database, roles)| UserMapping {
                database: database.clone(),
                user: name.clone(),
                spec: DatabaseUserSpec {
                    default_schema: Some(DEFAULT_SCHEMA.to_owned()),
                    roles: roles.clone(),
                },
            })
            .collect();
        Ok(vec![DdlOperation::SqlServer(
            SqlServerOperation::CreateLogin {
                name,
                source,
                spec,
                mappings,
            },
        )])
    }

    fn changed(&self) -> CatalogPath {
        logins_folder()
    }

    fn administration(&self) -> Option<String> {
        let roles = self.server_roles.iter().cloned();
        let mapped = self
            .mappings
            .iter()
            .flat_map(|(_, roles)| roles.iter().cloned());
        administration(roles.chain(mapped), self.name.trim())
    }
}

/// Edit login…: its defaults, policy, password, state and server roles, and its permissions.
struct EditLogin {
    path: CatalogPath,
    name: String,
    shared: Shared,
    described: Loaded<LoginDescription>,
    after: LoginSpec,
    password: PasswordEntry,
    must_change: bool,
    unlock: bool,
    revoked: BTreeSet<PermissionGrant>,
}

impl EditLogin {
    fn new(path: CatalogPath, name: String, shared: Shared) -> Self {
        Self {
            path,
            name,
            shared,
            described: Loaded::Loading,
            after: LoginSpec::new(None),
            password: PasswordEntry::default(),
            must_change: false,
            unlock: false,
            revoked: BTreeSet::new(),
        }
    }

    fn as_typed(&self) -> bool {
        self.after.policy.is_some_and(|policy| policy.check_policy) || self.must_change
    }
}

impl ObjectForm for EditLogin {
    fn title(&self) -> String {
        format!("Edit login {}", self.name)
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        if !self.shared.shown(ui, &Principal::Login(self.name.clone()))
            || !self.described.shown(ui, "the login")
        {
            return;
        }
        let Some(login) = self.described.ready().cloned() else {
            return;
        };
        ui.weak(login.kind.label());
        if self
            .shared
            .server
            .ready()
            .is_some_and(|server| server.current == self.name)
        {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "This is the login DbRover is connected as.",
            );
        }
        if let Some(state) = login.sql {
            let mut facts = Vec::new();
            if state.locked {
                facts.push("locked".to_owned());
            }
            if state.expired {
                facts.push("password expired".to_owned());
            }
            if state.must_change {
                facts.push("must change its password".to_owned());
            }
            if state.bad_password_count > 0 {
                facts.push(format!("{} bad passwords", state.bad_password_count));
            }
            if let Some(days) = state.days_until_expiration {
                facts.push(format!("password expires in {days} days"));
            }
            if !facts.is_empty() {
                ui.label(facts.join(", "));
            }
        }
        let sql = login.kind == LoginKind::Sql;
        Grid::new("edit_login").num_columns(2).show(ui, |ui| {
            if sql {
                self.password.show(ui, "New password");
                ui.label("");
                ui.weak(how_sent(self.as_typed()));
                ui.end_row();
            }
            ui.label("Default database");
            combo(
                ui,
                "Default database",
                &mut self.after.default_database,
                databases(context.catalog),
                "master",
            );
            ui.end_row();
        });
        if let Some(policy) = &mut self.after.policy {
            ui.checkbox(&mut policy.check_policy, "Enforce password policy");
            ui.checkbox(&mut policy.check_expiration, "Enforce expiration");
            ui.checkbox(
                &mut self.must_change,
                "Must change at next login (with a new password)",
            );
            ui.checkbox(&mut self.unlock, "Unlock (with a new password)");
        }
        ui.checkbox(&mut self.after.enabled, "Enabled");
        CollapsingHeader::new(format!("Server roles ({})", self.after.server_roles.len()))
            .id_salt("server roles")
            .show(ui, |ui| {
                ticks(
                    ui,
                    "edit_login_roles",
                    &mut self.after.server_roles,
                    names_in(context.catalog, &server_roles_folder(), NodeKind::Role),
                );
            });
        permission_ticks(ui, &login.permissions, &mut self.revoked);
        if !login.users.is_empty() {
            let users: Vec<String> = login
                .users
                .iter()
                .map(|user| format!("{} in {}", user.user, user.database))
                .collect();
            ui.weak(format!("Its users: {}.", users.join(", ")));
        }
    }

    fn catalog_requests(&self) -> Vec<CatalogPath> {
        vec![CatalogPath::root(), server_roles_folder()]
    }

    fn principal_request(&self) -> Option<CatalogPath> {
        Some(self.path.clone())
    }

    fn principal_described(&mut self, result: &Result<PrincipalDescription, String>) {
        self.described = match result {
            Ok(PrincipalDescription::Login(login)) => {
                self.after = LoginSpec::from(login);
                Loaded::Ready(login.clone())
            }
            Ok(other) => Loaded::Failed(format!("Not a login: {other:?}")),
            Err(error) => Loaded::Failed(format!("Could not load the login: {error}")),
        };
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.shared.loaded(result);
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let server = self.shared.check(&Principal::Login(self.name.clone()))?;
        let login = self.described.ready().ok_or(DdlError::NothingChanged)?;
        let before = LoginSpec::from(login);
        let password = self.password.password(self.as_typed(), server)?;
        let mut operations = Vec::new();
        if before != self.after || password.is_some() || self.unlock || self.must_change {
            operations.push(DdlOperation::SqlServer(SqlServerOperation::AlterLogin {
                name: self.name.clone(),
                before,
                after: self.after.clone(),
                password,
                must_change: self.must_change,
                unlock: self.unlock,
            }));
        }
        operations.extend(revokes(&PrincipalScope::Server, &self.name, &self.revoked));
        if operations.is_empty() {
            return Err(DdlError::NothingChanged);
        }
        Ok(operations)
    }

    fn changed(&self) -> CatalogPath {
        logins_folder()
    }

    fn administration(&self) -> Option<String> {
        let before = &self.described.ready()?.server_roles;
        administration(
            self.after.server_roles.difference(before).cloned(),
            &self.name,
        )
    }
}

/// What a new database user is for, as the dialog asks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewUserKind {
    Login,
    WithoutLogin,
    Password,
    External,
}

/// New user… in a database.
struct NewUser {
    database: String,
    shared: Shared,
    kind: NewUserKind,
    name: String,
    login: String,
    password: PasswordEntry,
    default_schema: String,
    roles: BTreeSet<String>,
}

impl NewUser {
    fn new(database: String, shared: Shared) -> Self {
        Self {
            database,
            shared,
            kind: NewUserKind::Login,
            name: String::new(),
            login: String::new(),
            password: PasswordEntry::default(),
            default_schema: DEFAULT_SCHEMA.to_owned(),
            roles: BTreeSet::new(),
        }
    }

    fn principal(&self) -> Principal {
        Principal::User {
            database: self.database.clone(),
            name: self.name.trim().to_owned(),
        }
    }
}

impl ObjectForm for NewUser {
    fn title(&self) -> String {
        format!("New user in {}", self.database)
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        if !self.shared.shown(ui, &self.principal()) {
            return;
        }
        let contained = self
            .shared
            .server
            .ready()
            .is_some_and(|server| server.contained && server.database == self.database);
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.kind, NewUserKind::Login, "For a login");
            ui.radio_value(&mut self.kind, NewUserKind::WithoutLogin, "Without login");
            ui.add_enabled_ui(contained, |ui| {
                ui.radio_value(&mut self.kind, NewUserKind::Password, "With a password")
                    .on_disabled_hover_text("Only in a contained database");
            });
            ui.radio_value(&mut self.kind, NewUserKind::External, "Microsoft Entra");
        });
        Grid::new("new_user").num_columns(2).show(ui, |ui| {
            if self.kind == NewUserKind::Login {
                ui.label("Login");
                let before = self.login.clone();
                combo(ui, "Login", &mut self.login, names_in(context.catalog, &logins_folder(), NodeKind::Login), "Choose…");
                if self.login != before && (self.name.is_empty() || self.name == before) {
                    self.name.clone_from(&self.login);
                }
                ui.end_row();
            }
            let label = ui.label("Name");
            ui.add(TextEdit::singleline(&mut self.name).desired_width(FIELD_WIDTH))
                .labelled_by(label.id);
            ui.end_row();
            if self.kind == NewUserKind::Password {
                self.password.show(ui, "Password");
                ui.label("");
                ui.weak("A contained user's password has no hashed form: sent as typed, over an encrypted connection only.");
                ui.end_row();
            }
            ui.label("Default schema");
            combo(ui, "Default schema", &mut self.default_schema, schemas(context.catalog, &self.database), DEFAULT_SCHEMA);
            ui.end_row();
        });
        CollapsingHeader::new("Roles")
            .default_open(true)
            .show(ui, |ui| {
                ticks(
                    ui,
                    "new_user_roles",
                    &mut self.roles,
                    names_in(
                        context.catalog,
                        &database_folder(&self.database, FolderKind::Roles),
                        NodeKind::Role,
                    ),
                );
            });
    }

    fn catalog_requests(&self) -> Vec<CatalogPath> {
        vec![
            logins_folder(),
            CatalogPath::root().child(NodeKind::Database, &self.database),
            database_folder(&self.database, FolderKind::Roles),
        ]
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.shared.loaded(result);
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let server = self.shared.check(&self.principal())?;
        let source = match self.kind {
            NewUserKind::Login => {
                if self.login.trim().is_empty() {
                    return Err(DdlError::MissingName("login"));
                }
                UserSource::Login(self.login.clone())
            }
            NewUserKind::WithoutLogin => UserSource::WithoutLogin,
            NewUserKind::Password => match self.password.password(true, server)? {
                Some(SqlServerPassword::AsTyped(password)) => UserSource::Password(password),
                _ => return Err(DdlError::MissingName("password")),
            },
            NewUserKind::External => UserSource::External,
        };
        let schema = self.default_schema.trim();
        Ok(vec![DdlOperation::SqlServer(
            SqlServerOperation::CreateUser {
                database: self.database.clone(),
                name: self.name.trim().to_owned(),
                source,
                spec: DatabaseUserSpec {
                    default_schema: (!schema.is_empty()).then(|| schema.to_owned()),
                    roles: self.roles.clone(),
                },
            },
        )])
    }

    fn changed(&self) -> CatalogPath {
        database_folder(&self.database, FolderKind::Users)
    }

    fn administration(&self) -> Option<String> {
        administration(self.roles.iter().cloned(), self.name.trim())
    }
}

/// Edit user…: its default schema, roles and permissions; an orphaned user's login; a
/// contained user's password.
struct EditUser {
    path: CatalogPath,
    database: String,
    name: String,
    shared: Shared,
    described: Loaded<DatabaseUserDescription>,
    after: DatabaseUserSpec,
    login: String,
    password: PasswordEntry,
    revoked: BTreeSet<PermissionGrant>,
}

impl EditUser {
    fn new(path: CatalogPath, database: String, name: String, shared: Shared) -> Self {
        Self {
            path,
            database,
            name,
            shared,
            described: Loaded::Loading,
            after: DatabaseUserSpec::default(),
            login: String::new(),
            password: PasswordEntry::default(),
            revoked: BTreeSet::new(),
        }
    }

    fn principal(&self) -> Principal {
        Principal::User {
            database: self.database.clone(),
            name: self.name.clone(),
        }
    }
}

impl ObjectForm for EditUser {
    fn title(&self) -> String {
        format!("Edit user {} in {}", self.name, self.database)
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        if !self.shared.shown(ui, &self.principal()) || !self.described.shown(ui, "the user") {
            return;
        }
        let Loaded::Ready(user) = &self.described else {
            return;
        };
        Grid::new("edit_user").num_columns(2).show(ui, |ui| {
            ui.label("For");
            match &user.kind {
                DatabaseUserKind::Login(login) => ui.label(format!("login {login}")),
                DatabaseUserKind::Orphaned => {
                    ui.colored_label(ui.visuals().warn_fg_color, "a login that no longer exists")
                }
                DatabaseUserKind::WithoutLogin => ui.label("no login"),
                DatabaseUserKind::Contained => ui.label("its own password (contained)"),
                DatabaseUserKind::External => ui.label("Microsoft Entra"),
                DatabaseUserKind::Key => ui.label("a certificate or key"),
            };
            ui.end_row();
            if matches!(
                user.kind,
                DatabaseUserKind::Orphaned | DatabaseUserKind::Login(_)
            ) {
                ui.label("Map to login");
                combo(
                    ui,
                    "Map to login",
                    &mut self.login,
                    names_in(context.catalog, &logins_folder(), NodeKind::Login),
                    "Keep",
                );
                ui.end_row();
            }
            if user.kind == DatabaseUserKind::Contained {
                self.password.show(ui, "New password");
            }
            ui.label("Default schema");
            let mut schema = self.after.default_schema.clone().unwrap_or_default();
            combo(
                ui,
                "Default schema",
                &mut schema,
                schemas(context.catalog, &self.database),
                DEFAULT_SCHEMA,
            );
            self.after.default_schema = (!schema.is_empty()).then_some(schema);
            ui.end_row();
        });
        CollapsingHeader::new(format!("Roles ({})", self.after.roles.len()))
            .id_salt("roles")
            .default_open(true)
            .show(ui, |ui| {
                ticks(
                    ui,
                    "edit_user_roles",
                    &mut self.after.roles,
                    names_in(
                        context.catalog,
                        &database_folder(&self.database, FolderKind::Roles),
                        NodeKind::Role,
                    ),
                );
            });
        permission_ticks(ui, &user.permissions, &mut self.revoked);
    }

    fn catalog_requests(&self) -> Vec<CatalogPath> {
        vec![
            logins_folder(),
            CatalogPath::root().child(NodeKind::Database, &self.database),
            database_folder(&self.database, FolderKind::Roles),
        ]
    }

    fn principal_request(&self) -> Option<CatalogPath> {
        Some(self.path.clone())
    }

    fn principal_described(&mut self, result: &Result<PrincipalDescription, String>) {
        self.described = match result {
            Ok(PrincipalDescription::DatabaseUser(user)) => {
                self.after = DatabaseUserSpec::from(user);
                Loaded::Ready(user.clone())
            }
            Ok(other) => Loaded::Failed(format!("Not a database user: {other:?}")),
            Err(error) => Loaded::Failed(format!("Could not load the user: {error}")),
        };
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.shared.loaded(result);
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let server = self.shared.check(&self.principal())?;
        let user = self.described.ready().ok_or(DdlError::NothingChanged)?;
        let before = DatabaseUserSpec::from(user);
        let login = (!self.login.is_empty()).then(|| self.login.clone());
        let password = match self.password.password(true, server)? {
            Some(SqlServerPassword::AsTyped(password)) => Some(password),
            _ => None,
        };
        let mut operations = Vec::new();
        if before != self.after || login.is_some() || password.is_some() {
            operations.push(DdlOperation::SqlServer(SqlServerOperation::AlterUser {
                database: self.database.clone(),
                name: self.name.clone(),
                before,
                after: self.after.clone(),
                login,
                password,
            }));
        }
        operations.extend(revokes(
            &PrincipalScope::Database(self.database.clone()),
            &self.name,
            &self.revoked,
        ));
        if operations.is_empty() {
            return Err(DdlError::NothingChanged);
        }
        Ok(operations)
    }

    fn changed(&self) -> CatalogPath {
        database_folder(&self.database, FolderKind::Users)
    }

    fn administration(&self) -> Option<String> {
        let before = &self.described.ready()?.roles;
        administration(self.after.roles.difference(before).cloned(), &self.name)
    }
}

/// New server role… or New role… in a database: its name, owner and members.
struct NewRole {
    scope: PrincipalScope,
    shared: Shared,
    name: String,
    owner: String,
    members: BTreeSet<String>,
}

impl NewRole {
    fn new(scope: PrincipalScope, shared: Shared) -> Self {
        Self {
            scope,
            shared,
            name: String::new(),
            owner: String::new(),
            members: BTreeSet::new(),
        }
    }

    fn principal(&self) -> Principal {
        Principal::Role {
            scope: self.scope.clone(),
            name: self.name.trim().to_owned(),
        }
    }
}

/// The folder a role's members are picked from: the logins, or the database's users.
fn member_options(scope: &PrincipalScope, catalog: &CatalogTree) -> Vec<String> {
    match scope {
        PrincipalScope::Server => names_in(catalog, &logins_folder(), NodeKind::Login),
        PrincipalScope::Database(database) => names_in(
            catalog,
            &database_folder(database, FolderKind::Users),
            NodeKind::User,
        ),
    }
}

fn member_folder(scope: &PrincipalScope) -> CatalogPath {
    match scope {
        PrincipalScope::Server => logins_folder(),
        PrincipalScope::Database(database) => database_folder(database, FolderKind::Users),
    }
}

impl ObjectForm for NewRole {
    fn title(&self) -> String {
        match &self.scope {
            PrincipalScope::Server => "New server role".to_owned(),
            PrincipalScope::Database(database) => format!("New role in {database}"),
        }
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        if !self.shared.shown(ui, &self.principal()) {
            return;
        }
        Grid::new("new_sqlserver_role")
            .num_columns(2)
            .show(ui, |ui| {
                let label = ui.label("Name");
                ui.add(TextEdit::singleline(&mut self.name).desired_width(FIELD_WIDTH))
                    .labelled_by(label.id);
                ui.end_row();
                let label = ui.label("Owner");
                ui.add(
                    TextEdit::singleline(&mut self.owner)
                        .desired_width(FIELD_WIDTH)
                        .hint_text("you"),
                )
                .labelled_by(label.id);
                ui.end_row();
            });
        CollapsingHeader::new("Members")
            .default_open(true)
            .show(ui, |ui| {
                ticks(
                    ui,
                    "new_role_members",
                    &mut self.members,
                    member_options(&self.scope, context.catalog),
                );
            });
    }

    fn catalog_requests(&self) -> Vec<CatalogPath> {
        vec![member_folder(&self.scope)]
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.shared.loaded(result);
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        self.shared.check(&self.principal())?;
        let owner = self.owner.trim();
        Ok(vec![DdlOperation::SqlServer(
            SqlServerOperation::CreateRole {
                scope: self.scope.clone(),
                name: self.name.trim().to_owned(),
                owner: (!owner.is_empty()).then(|| owner.to_owned()),
                members: self.members.clone(),
            },
        )])
    }

    fn changed(&self) -> CatalogPath {
        self.principal().folder()
    }
}

/// Edit… on a server or database role: its members, fixed roles included.
struct EditRole {
    path: CatalogPath,
    principal: Principal,
    shared: Shared,
    described: Loaded<SqlServerRoleDescription>,
    members: BTreeSet<String>,
}

impl EditRole {
    fn new(path: CatalogPath, scope: PrincipalScope, name: String, shared: Shared) -> Self {
        Self {
            path,
            principal: Principal::Role { scope, name },
            shared,
            described: Loaded::Loading,
            members: BTreeSet::new(),
        }
    }

    fn scope(&self) -> PrincipalScope {
        match &self.principal {
            Principal::Role { scope, .. } => scope.clone(),
            Principal::Login(_) | Principal::User { .. } => PrincipalScope::Server,
        }
    }
}

impl ObjectForm for EditRole {
    fn title(&self) -> String {
        format!("Edit {} {}", self.principal.noun(), self.principal.name())
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        if !self.shared.shown(ui, &self.principal) || !self.described.shown(ui, "the role") {
            return;
        }
        if let Some(role) = self.described.ready() {
            if role.fixed {
                ui.weak("One of SQL Server's own roles: only its members change.");
            }
            if let Some(owner) = &role.owner {
                ui.label(format!("Owned by {owner}"));
            }
        }
        let options = member_options(&self.scope(), context.catalog);
        CollapsingHeader::new(format!("Members ({})", self.members.len()))
            .id_salt("members")
            .default_open(true)
            .show(ui, |ui| {
                ticks(ui, "edit_role_members", &mut self.members, options);
            });
    }

    fn catalog_requests(&self) -> Vec<CatalogPath> {
        vec![member_folder(&self.scope())]
    }

    fn principal_request(&self) -> Option<CatalogPath> {
        Some(self.path.clone())
    }

    fn principal_described(&mut self, result: &Result<PrincipalDescription, String>) {
        self.described = match result {
            Ok(PrincipalDescription::SqlServerRole(role)) => {
                self.members.clone_from(&role.members);
                Loaded::Ready(role.clone())
            }
            Ok(other) => Loaded::Failed(format!("Not a role: {other:?}")),
            Err(error) => Loaded::Failed(format!("Could not load the role: {error}")),
        };
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.shared.loaded(result);
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        self.shared.check(&self.principal)?;
        let role = self.described.ready().ok_or(DdlError::NothingChanged)?;
        Ok(vec![DdlOperation::SqlServer(
            SqlServerOperation::AlterRoleMembers {
                scope: role.scope.clone(),
                name: role.name.clone(),
                before: role.members.clone(),
                after: self.members.clone(),
            },
        )])
    }

    fn changed(&self) -> CatalogPath {
        self.principal.folder()
    }

    fn administration(&self) -> Option<String> {
        let role = self.described.ready()?;
        let added: Vec<&String> = self.members.difference(&role.members).collect();
        (sql_server_is_administrative(&role.name) && !added.is_empty()).then(|| {
            let added: Vec<&str> = added.iter().map(|member| member.as_str()).collect();
            format!(
                "This gives {} administration through {}.",
                added.join(", "),
                role.name
            )
        })
    }
}

/// Rename… a login, a user or a role of one's own.
struct Rename {
    principal: Principal,
    shared: Shared,
    new_name: String,
}

impl Rename {
    fn new(principal: Principal, shared: Shared) -> Self {
        Self {
            new_name: principal.name().to_owned(),
            principal,
            shared,
        }
    }
}

impl ObjectForm for Rename {
    fn title(&self) -> String {
        format!("Rename {} {}", self.principal.noun(), self.principal.name())
    }

    fn show(&mut self, ui: &mut Ui, _context: &FormContext<'_>) {
        if !self.shared.shown(ui, &self.principal) {
            return;
        }
        ui.horizontal(|ui| {
            let label = ui.label("New name");
            ui.add(TextEdit::singleline(&mut self.new_name))
                .labelled_by(label.id);
        });
        if matches!(self.principal, Principal::Login(_)) {
            ui.weak("Its users keep their names: they are found by the login's SID, not its name.");
        }
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.shared.loaded(result);
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        self.shared.check(&self.principal)?;
        let new_name = self.new_name.trim().to_owned();
        if new_name == self.principal.name() {
            return Err(DdlError::NothingChanged);
        }
        let operation = match &self.principal {
            Principal::Login(name) => SqlServerOperation::RenameLogin {
                name: name.clone(),
                new_name,
            },
            Principal::User { database, name } => SqlServerOperation::RenameUser {
                database: database.clone(),
                name: name.clone(),
                new_name,
            },
            Principal::Role { scope, name } => SqlServerOperation::RenameRole {
                scope: scope.clone(),
                name: name.clone(),
                new_name,
            },
        };
        Ok(vec![DdlOperation::SqlServer(operation)])
    }

    fn changed(&self) -> CatalogPath {
        self.principal.folder()
    }
}

/// Disables a login, or enables a disabled one.
struct SetEnabled {
    path: CatalogPath,
    name: String,
    shared: Shared,
    disabled: Loaded<bool>,
}

impl SetEnabled {
    fn new(path: CatalogPath, name: String, shared: Shared) -> Self {
        Self {
            path,
            name,
            shared,
            disabled: Loaded::Loading,
        }
    }
}

impl ObjectForm for SetEnabled {
    fn title(&self) -> String {
        match self.disabled {
            Loaded::Ready(true) => format!("Enable login {}", self.name),
            _ => format!("Disable login {}", self.name),
        }
    }

    fn show(&mut self, ui: &mut Ui, _context: &FormContext<'_>) {
        if !self.shared.shown(ui, &Principal::Login(self.name.clone()))
            || !self.disabled.shown(ui, "the login")
        {
            return;
        }
        ui.label(match self.disabled {
            Loaded::Ready(true) => "It is disabled: nobody may log in as it. Enable it?",
            _ => "Disable it, so nobody may log in as it? Sessions already open stay open.",
        });
    }

    fn principal_request(&self) -> Option<CatalogPath> {
        Some(self.path.clone())
    }

    fn principal_described(&mut self, result: &Result<PrincipalDescription, String>) {
        self.disabled = match result {
            Ok(PrincipalDescription::Login(login)) => Loaded::Ready(login.disabled),
            Ok(other) => Loaded::Failed(format!("Not a login: {other:?}")),
            Err(error) => Loaded::Failed(format!("Could not load the login: {error}")),
        };
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.shared.loaded(result);
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        self.shared.check(&Principal::Login(self.name.clone()))?;
        let disabled = self.disabled.ready().ok_or(DdlError::NothingChanged)?;
        Ok(vec![DdlOperation::SqlServer(
            SqlServerOperation::SetLoginEnabled {
                name: self.name.clone(),
                enabled: *disabled,
            },
        )])
    }

    fn changed(&self) -> CatalogPath {
        logins_folder()
    }
}

/// Drop…: a login with its users if asked; a user after giving away what it owns; a role
/// after taking its members out.
struct DropPrincipal {
    path: CatalogPath,
    principal: Principal,
    shared: Shared,
    described: Loaded<PrincipalDescription>,
    /// A login's users are dropped with it, rather than left orphaned.
    drop_users: bool,
    /// Who a user's schemas and roles go to.
    new_owner: String,
}

impl DropPrincipal {
    fn new(path: CatalogPath, principal: Principal, shared: Shared) -> Self {
        Self {
            path,
            principal,
            shared,
            described: Loaded::Loading,
            drop_users: true,
            new_owner: DEFAULT_OWNER.to_owned(),
        }
    }

    fn consequences(&self) -> Vec<String> {
        let mut consequences = Vec::new();
        match self.described.ready() {
            Some(PrincipalDescription::Login(login)) => {
                if !login.owned_databases.is_empty() {
                    consequences.push(format!(
                        "It owns the databases {}, which SQL Server won't drop it for: give them to \
                         another login first.",
                        login.owned_databases.join(", ")
                    ));
                }
                if !login.users.is_empty() {
                    let users = user_list(&login.users);
                    consequences.push(if self.drop_users {
                        format!("Its users are dropped first: {users}.")
                    } else {
                        format!("Its users are left orphaned, with no login: {users}.")
                    });
                }
            }
            Some(PrincipalDescription::DatabaseUser(user)) => {
                let owned: Vec<String> = user
                    .owned_schemas
                    .iter()
                    .map(|schema| format!("schema {schema}"))
                    .chain(user.owned_roles.iter().map(|role| format!("role {role}")))
                    .collect();
                if !owned.is_empty() {
                    consequences.push(format!(
                        "{} go to {}.",
                        owned.join(", "),
                        self.new_owner.trim()
                    ));
                }
            }
            Some(PrincipalDescription::SqlServerRole(role)) if !role.members.is_empty() => {
                let members: Vec<&str> = role.members.iter().map(String::as_str).collect();
                consequences.push(format!(
                    "Its members are taken out first: {}.",
                    members.join(", ")
                ));
            }
            _ => {}
        }
        if self.shared.server.ready().is_some_and(
            |server| matches!(&self.principal, Principal::Login(name) if *name == server.current),
        ) {
            consequences.push("It is the login DbRover is connected as.".to_owned());
        }
        consequences
    }
}

fn user_list(users: &BTreeSet<DatabaseUserName>) -> String {
    users
        .iter()
        .map(|user| format!("{} in {}", user.user, user.database))
        .collect::<Vec<_>>()
        .join(", ")
}

impl ObjectForm for DropPrincipal {
    fn title(&self) -> String {
        format!("Drop {} {}", self.principal.noun(), self.principal.name())
    }

    fn show(&mut self, ui: &mut Ui, _context: &FormContext<'_>) {
        if !self.shared.shown(ui, &self.principal) || !self.described.shown(ui, "what goes with it")
        {
            return;
        }
        match self.described.ready() {
            Some(PrincipalDescription::Login(login)) if !login.users.is_empty() => {
                ui.checkbox(
                    &mut self.drop_users,
                    "Drop its users too, rather than leave them orphaned",
                );
            }
            Some(PrincipalDescription::DatabaseUser(user))
                if !user.owned_schemas.is_empty() || !user.owned_roles.is_empty() =>
            {
                ui.horizontal(|ui| {
                    let label = ui.label("Give what it owns to");
                    ui.add(TextEdit::singleline(&mut self.new_owner))
                        .labelled_by(label.id);
                });
            }
            _ => {}
        }
    }

    fn principal_request(&self) -> Option<CatalogPath> {
        Some(self.path.clone())
    }

    fn principal_described(&mut self, result: &Result<PrincipalDescription, String>) {
        self.described = match result {
            Ok(described) => Loaded::Ready(described.clone()),
            Err(error) => Loaded::Failed(format!("Could not load it: {error}")),
        };
    }

    fn needs_user_management(&self) -> bool {
        true
    }

    fn user_management_loaded(&mut self, result: &Result<UserManagement, String>) {
        self.shared.loaded(result);
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        self.shared.check(&self.principal)?;
        let described = self.described.ready().ok_or(DdlError::NothingChanged)?;
        let operation = match described {
            PrincipalDescription::Login(login) => SqlServerOperation::DropLogin {
                name: login.name.clone(),
                users: if self.drop_users {
                    login.users.iter().cloned().collect()
                } else {
                    Vec::new()
                },
            },
            PrincipalDescription::DatabaseUser(user) => {
                let owns = !user.owned_schemas.is_empty() || !user.owned_roles.is_empty();
                SqlServerOperation::DropUser {
                    database: user.database.clone(),
                    name: user.name.clone(),
                    owned_schemas: user.owned_schemas.clone(),
                    owned_roles: user.owned_roles.clone(),
                    give_to: owns.then(|| self.new_owner.clone()),
                }
            }
            PrincipalDescription::SqlServerRole(role) => {
                if role.fixed {
                    return Err(DdlError::FixedRole);
                }
                SqlServerOperation::DropRole {
                    scope: role.scope.clone(),
                    name: role.name.clone(),
                    members: role.members.clone(),
                }
            }
            PrincipalDescription::Account(_) => return Err(DdlError::NothingChanged),
        };
        Ok(vec![DdlOperation::SqlServer(operation)])
    }

    fn changed(&self) -> CatalogPath {
        self.principal.folder()
    }

    fn destruction(&self, _dialect: Dialect) -> Option<Destruction> {
        Some(Destruction {
            name: self.principal.name().to_owned(),
            summary: format!(
                "Drops the {} {}.",
                self.principal.noun(),
                self.principal.name()
            ),
            consequences: self.consequences(),
        })
    }
}

#[cfg(test)]
mod tests;
