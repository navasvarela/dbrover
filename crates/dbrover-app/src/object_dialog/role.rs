//! New user…, New role… and Edit…: one form for PostgreSQL's roles, where a user is a role
//! that may log in; and Rename… and Drop…. The password is hashed into a SCRAM verifier as it is typed, so the
//! operations only ever hold the verifier.

use std::collections::BTreeSet;

use dbrover_core::{CatalogPath, FolderKind, NodeKind, RoleAttribute, RoleDescription};
use dbrover_sql::{
    DdlError, DdlOperation, Dialect, OwnedObjects, PasswordChange, RoleSpec, ScramVerifier,
};
use egui::{DragValue, Grid, TextEdit, Ui, WidgetType};

use super::{Destruction, FormContext, ObjectForm};
use crate::catalog_tree::CatalogTree;
use crate::widgets;

const FIELD_WIDTH: f32 = 260.0;
/// Where the connection limit field starts when Limit connections is ticked.
const DEFAULT_CONNECTION_LIMIT: u32 = 10;

/// The roles folder at the top of the tree, where every role is listed.
pub(super) fn roles_folder() -> CatalogPath {
    CatalogPath::root().child(
        NodeKind::Folder(FolderKind::Roles),
        FolderKind::Roles.label(),
    )
}

/// The role at `path`, a node of the roles folder.
pub(super) fn role_name(path: &CatalogPath) -> Option<&str> {
    match path.segments() {
        [folder, role]
            if folder.kind == NodeKind::Folder(FolderKind::Roles)
                && role.kind == NodeKind::Role =>
        {
            Some(role.name.as_str())
        }
        _ => None,
    }
}

/// What New starts as: the same dialog, with LOGIN ticked for a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoleKind {
    User,
    Role,
}

impl RoleKind {
    fn noun(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Role => "role",
        }
    }

    fn of(attributes: &BTreeSet<RoleAttribute>) -> Self {
        if attributes.contains(&RoleAttribute::Login) {
            Self::User
        } else {
            Self::Role
        }
    }
}

/// The attribute as the dialog names it; its keyword and what it does are in its tooltip.
fn attribute_label(attribute: RoleAttribute) -> &'static str {
    match attribute {
        RoleAttribute::Login => "Can log in",
        RoleAttribute::Superuser => "Superuser",
        RoleAttribute::CreateDb => "Create databases",
        RoleAttribute::CreateRole => "Create roles",
        RoleAttribute::Inherit => "Inherit privileges",
        RoleAttribute::Replication => "Replication",
        RoleAttribute::BypassRls => "Bypass row security",
    }
}

/// A password typed twice, and its verifier once both match.
#[derive(Default)]
struct PasswordFields {
    password: String,
    confirmation: String,
    verifier: Option<ScramVerifier>,
}

impl PasswordFields {
    /// `label` names the first field: "Password", or "New password" when editing.
    fn show(&mut self, ui: &mut Ui, label: &str, enabled: bool) {
        let mut changed = false;
        for (text, field) in [
            (label, &mut self.password),
            ("Confirm password", &mut self.confirmation),
        ] {
            let label = ui.label(text);
            changed |= ui
                .add_enabled(
                    enabled,
                    TextEdit::singleline(field)
                        .password(true)
                        .desired_width(FIELD_WIDTH),
                )
                .labelled_by(label.id)
                .changed();
            ui.end_row();
        }
        // Hashing takes thousands of rounds, so it runs once per change, not every frame.
        if changed {
            self.verifier = (!self.password.is_empty() && self.password == self.confirmation)
                .then(|| ScramVerifier::hash(&self.password));
        }
    }

    /// `None` when nothing was typed.
    fn verifier(&self) -> Result<Option<ScramVerifier>, DdlError> {
        if self.password.is_empty() && self.confirmation.is_empty() {
            return Ok(None);
        }
        self.verifier
            .clone()
            .map(Some)
            .ok_or(DdlError::PasswordsDiffer)
    }
}

/// Everything about a role but its name and password.
struct RoleFields {
    attributes: BTreeSet<RoleAttribute>,
    connection_limit: Option<u32>,
    valid_until: String,
    member_of: BTreeSet<String>,
}

impl RoleFields {
    fn new(kind: RoleKind) -> Self {
        let mut attributes: BTreeSet<RoleAttribute> = RoleAttribute::ALL
            .into_iter()
            .filter(|attribute| attribute.is_default())
            .collect();
        if kind == RoleKind::User {
            attributes.insert(RoleAttribute::Login);
        }
        Self {
            attributes,
            connection_limit: None,
            valid_until: String::new(),
            member_of: BTreeSet::new(),
        }
    }

    fn described(role: &RoleDescription) -> Self {
        Self {
            attributes: role.attributes.clone(),
            connection_limit: role.connection_limit,
            valid_until: role.valid_until.clone().unwrap_or_default(),
            member_of: role.member_of.clone(),
        }
    }

    fn spec(&self) -> RoleSpec {
        let valid_until = self.valid_until.trim();
        RoleSpec {
            attributes: self.attributes.clone(),
            connection_limit: self.connection_limit,
            valid_until: (!valid_until.is_empty()).then(|| valid_until.to_owned()),
            member_of: self.member_of.clone(),
        }
    }

    /// The rows after the name and password, in the same grid.
    fn show_rows(&mut self, ui: &mut Ui, catalog: &CatalogTree, own_name: &str) {
        ui.label("Connections");
        ui.horizontal(|ui| {
            let mut limited = self.connection_limit.is_some();
            if ui.checkbox(&mut limited, "Limit to").changed() {
                self.connection_limit = limited.then_some(DEFAULT_CONNECTION_LIMIT);
            }
            if let Some(limit) = &mut self.connection_limit {
                ui.add(DragValue::new(limit));
            }
        });
        ui.end_row();
        let label = ui.label("Valid until");
        ui.add(
            TextEdit::singleline(&mut self.valid_until)
                .desired_width(FIELD_WIDTH)
                .hint_text("never, or e.g. 2027-01-01"),
        )
        .labelled_by(label.id);
        ui.end_row();
        ui.label("Member of");
        let roles = other_roles(catalog, own_name);
        membership_picker(ui, &mut self.member_of, &roles);
        ui.end_row();
    }

    fn show_attributes(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            for attribute in RoleAttribute::ALL {
                let mut on = self.attributes.contains(&attribute);
                let checkbox = ui
                    .checkbox(&mut on, attribute_label(attribute))
                    .on_hover_text(format!(
                        "{}: {}",
                        attribute.keyword(),
                        attribute.description()
                    ));
                if checkbox.changed() {
                    if on {
                        self.attributes.insert(attribute);
                    } else {
                        self.attributes.remove(&attribute);
                    }
                }
            }
        });
    }
}

/// Every role in the loaded roles folder but `own_name`, PostgreSQL's own `pg_` roles
/// included: granting one (`pg_read_all_data`, say) is how a role gets its privileges.
fn other_roles(catalog: &CatalogTree, own_name: &str) -> Vec<String> {
    catalog
        .loaded_children(&roles_folder())
        .into_iter()
        .flatten()
        .filter(|node| node.kind() == Some(NodeKind::Role) && node.label != own_name)
        .map(|node| node.label.clone())
        .collect()
}

fn membership_picker(ui: &mut Ui, chosen: &mut BTreeSet<String>, roles: &[String]) {
    let text = if chosen.is_empty() {
        "None".to_owned()
    } else {
        chosen.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    let menu = widgets::menu_button(ui, text, |ui| {
        if roles.is_empty() {
            ui.weak("No other roles yet");
        }
        for role in roles {
            let mut ticked = chosen.contains(role);
            if ui.checkbox(&mut ticked, role).changed() {
                if ticked {
                    chosen.insert(role.clone());
                } else {
                    chosen.remove(role);
                }
            }
        }
    });
    widgets::name(
        menu.response,
        WidgetType::Button,
        "Roles it belongs to",
        true,
    );
}

pub(crate) struct NewRole {
    kind: RoleKind,
    name: String,
    password: PasswordFields,
    fields: RoleFields,
}

impl NewRole {
    /// On the data source (the root) or its roles folder.
    pub(crate) fn new(path: &CatalogPath, kind: RoleKind) -> Option<Self> {
        (path.is_root() || *path == roles_folder()).then(|| Self {
            kind,
            name: String::new(),
            password: PasswordFields::default(),
            fields: RoleFields::new(kind),
        })
    }
}

impl ObjectForm for NewRole {
    fn title(&self) -> String {
        format!("New {}", self.kind.noun())
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        Grid::new("new_role").num_columns(2).show(ui, |ui| {
            let label = ui.label("Name");
            ui.add(TextEdit::singleline(&mut self.name).desired_width(FIELD_WIDTH))
                .labelled_by(label.id);
            ui.end_row();
            self.password.show(ui, "Password", true);
            self.fields.show_rows(ui, context.catalog, self.name.trim());
        });
        self.fields.show_attributes(ui);
    }

    fn catalog_requests(&self) -> Vec<CatalogPath> {
        vec![roles_folder()]
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        Ok(vec![DdlOperation::CreateRole {
            name: self.name.trim().to_owned(),
            spec: self.fields.spec(),
            password: self.password.verifier()?,
        }])
    }

    fn changed(&self) -> CatalogPath {
        roles_folder()
    }
}

enum Described {
    Loading,
    /// The role as it was described, to compare the fields with.
    Loaded(RoleSpec),
    Failed(String),
}

pub(crate) struct EditRole {
    path: CatalogPath,
    name: String,
    described: Described,
    fields: RoleFields,
    password: PasswordFields,
    remove_password: bool,
}

impl EditRole {
    /// On a role.
    pub(crate) fn new(path: CatalogPath) -> Option<Self> {
        let name = role_name(&path)?.to_owned();
        Some(Self {
            path,
            name,
            described: Described::Loading,
            fields: RoleFields::new(RoleKind::Role),
            password: PasswordFields::default(),
            remove_password: false,
        })
    }

    fn password_change(&self) -> Result<PasswordChange, DdlError> {
        if self.remove_password {
            return Ok(PasswordChange::Remove);
        }
        Ok(match self.password.verifier()? {
            Some(verifier) => PasswordChange::Set(verifier),
            None => PasswordChange::Keep,
        })
    }
}

impl ObjectForm for EditRole {
    fn title(&self) -> String {
        let kind = match &self.described {
            Described::Loaded(before) => RoleKind::of(&before.attributes),
            Described::Loading | Described::Failed(_) => RoleKind::Role,
        };
        format!("Edit {} {}", kind.noun(), self.name)
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        match &self.described {
            Described::Loading => {
                ui.weak("Loading the role…");
                return;
            }
            Described::Failed(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
                return;
            }
            Described::Loaded(_) => {}
        }
        Grid::new("edit_role").num_columns(2).show(ui, |ui| {
            self.password
                .show(ui, "New password", !self.remove_password);
            ui.label("");
            ui.checkbox(&mut self.remove_password, "Remove password")
                .on_hover_text("PASSWORD NULL: password logins fail until a new one is set");
            ui.end_row();
            self.fields.show_rows(ui, context.catalog, &self.name);
        });
        self.fields.show_attributes(ui);
        ui.weak("Leave the password empty to keep the one it has.");
    }

    fn catalog_requests(&self) -> Vec<CatalogPath> {
        vec![roles_folder()]
    }

    fn role_request(&self) -> Option<CatalogPath> {
        Some(self.path.clone())
    }

    fn role_described(&mut self, result: &Result<RoleDescription, String>) {
        match result {
            Ok(role) => {
                self.fields = RoleFields::described(role);
                self.described = Described::Loaded(RoleSpec::from(role));
            }
            Err(error) => {
                self.described = Described::Failed(format!("Could not load the role: {error}"));
            }
        }
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let Described::Loaded(before) = &self.described else {
            return Err(DdlError::NothingChanged);
        };
        Ok(vec![DdlOperation::AlterRole {
            name: self.name.clone(),
            before: before.clone(),
            after: self.fields.spec(),
            password: self.password_change()?,
        }])
    }

    fn changed(&self) -> CatalogPath {
        roles_folder()
    }
}

/// Gives a role another name.
pub(crate) struct RenameRole {
    name: String,
    new_name: String,
}

impl RenameRole {
    /// On a role.
    pub(crate) fn new(path: &CatalogPath) -> Option<Self> {
        let name = role_name(path)?.to_owned();
        Some(Self {
            new_name: name.clone(),
            name,
        })
    }
}

impl ObjectForm for RenameRole {
    fn title(&self) -> String {
        format!("Rename role {}", self.name)
    }

    fn show(&mut self, ui: &mut Ui, _context: &FormContext<'_>) {
        ui.horizontal(|ui| {
            let label = ui.label("New name");
            ui.add(TextEdit::singleline(&mut self.new_name))
                .labelled_by(label.id);
        });
        ui.weak(
            "A password stored as MD5 is cleared, since PostgreSQL salts those with the name; \
             SCRAM passwords, which DbRover sets, are kept.",
        );
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        Ok(vec![DdlOperation::RenameRole {
            name: self.name.clone(),
            new_name: self.new_name.trim().to_owned(),
        }])
    }

    fn changed(&self) -> CatalogPath {
        roles_folder()
    }
}

/// What Drop does with what the role owns, as the dialog offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnedChoice {
    Keep,
    Reassign,
    Drop,
}

/// Drops a role, after giving away or dropping what it owns in this database if asked. The
/// confirmation counts what it owns, from its description.
pub(crate) struct DropRole {
    path: CatalogPath,
    name: String,
    /// `None` until it is described; the drop can go ahead meanwhile, and the database
    /// refuses it if the role still owns anything.
    described: Option<Result<RoleDescription, String>>,
    owned: OwnedChoice,
    new_owner: String,
}

impl DropRole {
    /// On a role.
    pub(crate) fn new(path: CatalogPath) -> Option<Self> {
        let name = role_name(&path)?.to_owned();
        Some(Self {
            path,
            name,
            described: None,
            owned: OwnedChoice::Keep,
            new_owner: String::new(),
        })
    }

    fn role(&self) -> Option<&RoleDescription> {
        self.described.as_ref()?.as_ref().ok()
    }

    /// What goes with the role, for the confirmation.
    fn consequences(&self, role: &RoleDescription) -> Vec<String> {
        let mut consequences = Vec::new();
        let objects = count(role.owned_objects, "object");
        if role.owned_objects > 0 {
            consequences.push(match self.owned {
                OwnedChoice::Keep => format!(
                    "It owns {objects} in this database, so PostgreSQL refuses to drop it until \
                     they are given away or dropped."
                ),
                OwnedChoice::Reassign => format!(
                    "REASSIGN OWNED gives the {objects} it owns in this database to {}.",
                    owner_or_placeholder(&self.new_owner)
                ),
                OwnedChoice::Drop => format!(
                    "DROP OWNED drops the {objects} it owns in this database, with their data."
                ),
            });
        }
        if role.privileges > 0 {
            let objects = count(role.privileges, "object");
            consequences.push(match self.owned {
                OwnedChoice::Keep => format!(
                    "It holds privileges on {objects} in this database, which stop the drop too."
                ),
                OwnedChoice::Reassign | OwnedChoice::Drop => {
                    format!("DROP OWNED revokes its privileges on {objects}.")
                }
            });
        }
        if !role.owned_databases.is_empty() {
            let databases = role.owned_databases.join(", ");
            consequences.push(match self.owned {
                OwnedChoice::Reassign => format!(
                    "It owns the databases {databases}, which REASSIGN OWNED gives away too."
                ),
                OwnedChoice::Keep | OwnedChoice::Drop => format!(
                    "It owns the databases {databases}, which PostgreSQL won't drop with it: \
                     give them to another role, or drop them first."
                ),
            });
        }
        if !role.members.is_empty() {
            let members: Vec<&str> = role.members.iter().map(String::as_str).collect();
            consequences.push(format!(
                "Its members lose its privileges: {}.",
                members.join(", ")
            ));
        }
        consequences
    }
}

fn count(n: u64, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

fn owner_or_placeholder(owner: &str) -> &str {
    if owner.trim().is_empty() {
        "the role picked above"
    } else {
        owner
    }
}

impl ObjectForm for DropRole {
    fn title(&self) -> String {
        format!("Drop role {}", self.name)
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        match &self.described {
            None => {
                ui.weak("Counting what the role owns…");
            }
            Some(Err(error)) => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Could not count what the role owns: {error}"),
                );
            }
            Some(Ok(_)) => {}
        }
        ui.label("What it owns in this database:");
        ui.radio_value(
            &mut self.owned,
            OwnedChoice::Keep,
            "Keep it (the drop fails if there is any)",
        );
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.owned, OwnedChoice::Reassign, "Give it to");
            let roles = other_roles(context.catalog, &self.name);
            let combo = egui::ComboBox::from_id_salt("drop_role_new_owner")
                .selected_text(if self.new_owner.is_empty() {
                    "Choose…"
                } else {
                    self.new_owner.as_str()
                })
                .show_ui(ui, |ui| {
                    for role in roles {
                        let picked = ui.selectable_value(&mut self.new_owner, role.clone(), &role);
                        if picked.clicked() {
                            self.owned = OwnedChoice::Reassign;
                        }
                    }
                });
            widgets::name(combo.response, WidgetType::ComboBox, "New owner", true);
        });
        ui.radio_value(&mut self.owned, OwnedChoice::Drop, "Drop it");
        if self.owned != OwnedChoice::Keep {
            ui.weak(
                "REASSIGN OWNED and DROP OWNED act in this database only: what the role owns \
                 in another one still stops the drop.",
            );
        }
    }

    fn catalog_requests(&self) -> Vec<CatalogPath> {
        vec![roles_folder()]
    }

    fn role_request(&self) -> Option<CatalogPath> {
        Some(self.path.clone())
    }

    fn role_described(&mut self, result: &Result<RoleDescription, String>) {
        if let Ok(role) = result
            && (role.owned_objects > 0 || role.privileges > 0)
        {
            self.owned = OwnedChoice::Reassign;
        }
        self.described = Some(result.clone());
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let owned = match self.owned {
            OwnedChoice::Keep => OwnedObjects::Keep,
            OwnedChoice::Reassign => OwnedObjects::ReassignTo(self.new_owner.clone()),
            OwnedChoice::Drop => OwnedObjects::Drop,
        };
        Ok(vec![DdlOperation::DropRole {
            name: self.name.clone(),
            owned,
        }])
    }

    fn changed(&self) -> CatalogPath {
        roles_folder()
    }

    fn destruction(&self, _dialect: Dialect) -> Option<Destruction> {
        let noun = match self.role() {
            Some(role) => RoleKind::of(&role.attributes).noun(),
            None => RoleKind::Role.noun(),
        };
        Some(Destruction {
            name: self.name.clone(),
            summary: format!("Drops the {noun} {}.", self.name),
            consequences: self
                .role()
                .map(|role| self.consequences(role))
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice(owned_objects: u64, privileges: u64, owned_databases: &[&str]) -> RoleDescription {
        RoleDescription {
            name: "alice".to_owned(),
            attributes: [RoleAttribute::Login].into(),
            connection_limit: None,
            valid_until: None,
            member_of: BTreeSet::new(),
            members: ["auditors".to_owned()].into(),
            owned_objects,
            privileges,
            owned_databases: owned_databases
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
        }
    }

    fn drop_alice(role: RoleDescription) -> DropRole {
        let path = roles_folder().child(NodeKind::Role, "alice");
        let mut form = DropRole::new(path).unwrap();
        form.role_described(&Ok(role));
        form
    }

    #[test]
    fn opens_on_the_data_source_the_roles_folder_or_a_role() {
        let role = roles_folder().child(NodeKind::Role, "alice");
        assert!(NewRole::new(&CatalogPath::root(), RoleKind::User).is_some());
        assert!(NewRole::new(&roles_folder(), RoleKind::Role).is_some());
        assert!(NewRole::new(&role, RoleKind::Role).is_none());
        assert!(EditRole::new(role.clone()).is_some());
        assert!(RenameRole::new(&roles_folder()).is_none());
        assert!(DropRole::new(CatalogPath::root().child(NodeKind::Database, "alice")).is_none());
    }

    #[test]
    fn a_role_that_owns_nothing_is_dropped_alone() {
        let form = drop_alice(alice(0, 0, &[]));
        assert_eq!(form.owned, OwnedChoice::Keep);
        let destruction = form.destruction(Dialect::Postgres).unwrap();
        assert_eq!(destruction.summary, "Drops the user alice.");
        assert_eq!(
            destruction.consequences,
            ["Its members lose its privileges: auditors."]
        );
    }

    #[test]
    fn the_confirmation_names_what_goes_with_the_role() {
        let mut form = drop_alice(alice(3, 1, &["reports"]));
        assert_eq!(
            form.owned,
            OwnedChoice::Reassign,
            "a role that owns something starts out giving it away"
        );
        assert_eq!(
            form.operations()
                .and_then(|operations| dbrover_sql::ddl_statements(&operations, Dialect::Postgres)),
            Err(DdlError::NoNewOwner),
            "Execute waits for the new owner"
        );
        form.new_owner = "bob".to_owned();
        let cases = [
            (
                OwnedChoice::Keep,
                [
                    "It owns 3 objects in this database, so PostgreSQL refuses to drop it until \
                     they are given away or dropped.",
                    "It holds privileges on 1 object in this database, which stop the drop too.",
                    "It owns the databases reports, which PostgreSQL won't drop with it: give \
                     them to another role, or drop them first.",
                ],
            ),
            (
                OwnedChoice::Reassign,
                [
                    "REASSIGN OWNED gives the 3 objects it owns in this database to bob.",
                    "DROP OWNED revokes its privileges on 1 object.",
                    "It owns the databases reports, which REASSIGN OWNED gives away too.",
                ],
            ),
            (
                OwnedChoice::Drop,
                [
                    "DROP OWNED drops the 3 objects it owns in this database, with their data.",
                    "DROP OWNED revokes its privileges on 1 object.",
                    "It owns the databases reports, which PostgreSQL won't drop with it: give \
                     them to another role, or drop them first.",
                ],
            ),
        ];
        for (choice, expected) in cases {
            form.owned = choice;
            let consequences = form.destruction(Dialect::Postgres).unwrap().consequences;
            assert_eq!(consequences[..3], expected, "{choice:?}");
        }
        form.owned = OwnedChoice::Reassign;
        assert_eq!(
            form.operations(),
            Ok(vec![DdlOperation::DropRole {
                name: "alice".to_owned(),
                owned: OwnedObjects::ReassignTo("bob".to_owned()),
            }])
        );
    }
}
