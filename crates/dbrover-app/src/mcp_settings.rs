//! Editing what MCP clients may do: a project's overrides in Project settings, and the
//! defaults in View › Settings. Both show the same rows, so a project's settings read like
//! the defaults they start from.

use std::num::{NonZeroU32, NonZeroU64};

use dbrover_store::{McpOverrides, McpPermissions, SchemaScope, StatementTimeout};
use egui::{DragValue, Grid, TextEdit, Ui, WidgetType};

use crate::{form, widgets};

/// What each permission allows, as the plan's table puts it (`docs/mcp-server.md`).
const READ: &str = "Browse the catalog, SELECT, EXPLAIN, SHOW";
const WRITE: &str = "INSERT, UPDATE, DELETE, MERGE, TRUNCATE";
const SCHEMA: &str = "CREATE, ALTER, DROP, COMMENT ON and other DDL";
const ADMIN: &str = "Grants, roles, databases, settings, files, sessions, procedures";

/// A row limit nobody would page through, and a timeout of an hour: enough for any real
/// call, low enough that a typo doesn't turn the limits off.
const MAX_ROWS_LIMIT: u32 = 100_000;
const MAX_TIMEOUT_SECONDS: u64 = 60 * 60;

/// The schema allow-list as typed: "All schemas", or a comma-separated list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemasField {
    only: bool,
    text: String,
}

impl SchemasField {
    pub fn new(scope: &SchemaScope) -> Self {
        match scope {
            SchemaScope::All => Self {
                only: false,
                text: String::new(),
            },
            SchemaScope::Only(schemas) => Self {
                only: true,
                text: schemas.join(", "),
            },
        }
    }

    /// The scope typed, or `None` while "Only these" names no schema.
    pub fn scope(&self) -> Option<SchemaScope> {
        if !self.only {
            return Some(SchemaScope::All);
        }
        let schemas: Vec<String> = self
            .text
            .split(',')
            .map(str::trim)
            .filter(|schema| !schema.is_empty())
            .map(str::to_owned)
            .collect();
        (!schemas.is_empty()).then_some(SchemaScope::Only(schemas))
    }

    fn show(&mut self, ui: &mut Ui) -> bool {
        ui.horizontal(|ui| {
            let mut changed = ui.radio_value(&mut self.only, false, "All").changed();
            changed |= ui.radio_value(&mut self.only, true, "Only").changed();
            let field = ui.add_enabled(
                self.only,
                TextEdit::singleline(&mut self.text)
                    .hint_text("public, analytics")
                    .desired_width(180.0),
            );
            let field = widgets::name(field, WidgetType::TextEdit, "Allowed schemas", self.only);
            changed | field.changed()
        })
        .inner
    }
}

/// View › Settings › MCP: a draft of the defaults, saved with its own button, since a row
/// limit typed digit by digit shouldn't rewrite `projects.toml` at every keystroke.
pub struct McpDefaultsEditor {
    saved: McpPermissions,
    draft: McpPermissions,
    schemas: SchemasField,
    saving: bool,
    error: Option<String>,
}

impl McpDefaultsEditor {
    pub fn new(saved: &McpPermissions) -> Self {
        Self {
            saved: saved.clone(),
            draft: saved.clone(),
            schemas: SchemasField::new(&saved.schemas),
            saving: false,
            error: None,
        }
    }

    /// Follows the defaults as saved: after a save, or when `projects.toml` is read. A draft
    /// with unsaved changes is kept, unless it is the one just saved.
    pub fn follow(&mut self, saved: &McpPermissions) {
        if self.draft == self.saved || self.saving {
            *self = Self::new(saved);
        } else {
            self.saved = saved.clone();
        }
    }

    pub fn failed(&mut self, error: &str) {
        self.saving = false;
        self.error = Some(error.to_owned());
    }

    /// Draws the section. Returns the defaults to save when Save is clicked.
    pub fn show(&mut self, ui: &mut Ui) -> Option<McpPermissions> {
        edit_defaults(ui, &mut self.draft, &mut self.schemas);
        if let Some(error) = &self.error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Could not save: {error}"),
            );
        }
        let changed = self.draft != self.saved;
        let valid = self.schemas.scope().is_some();
        let save = ui.horizontal(|ui| {
            let save =
                form::primary_button(ui, "Save MCP defaults", changed && valid && !self.saving)
                    .clicked();
            if ui
                .add_enabled(changed && !self.saving, egui::Button::new("Discard"))
                .clicked()
            {
                *self = Self::new(&self.saved);
            }
            save
        });
        if !save.inner {
            return None;
        }
        self.saving = true;
        self.error = None;
        Some(self.draft.clone())
    }
}

/// The defaults' rows, each value edited directly.
fn edit_defaults(ui: &mut Ui, defaults: &mut McpPermissions, schemas: &mut SchemasField) {
    Grid::new("mcp_defaults")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            for (name, value, meaning) in [
                ("Read", &mut defaults.read, READ),
                ("Write", &mut defaults.write, WRITE),
                ("Schema", &mut defaults.schema, SCHEMA),
                ("Admin", &mut defaults.admin, ADMIN),
            ] {
                ui.checkbox(value, name);
                ui.weak(meaning);
                ui.end_row();
            }
            ui.label("Row limit");
            edit_max_rows(ui, &mut defaults.max_rows);
            ui.end_row();
            ui.label("Statement timeout");
            edit_timeout(ui, &mut defaults.statement_timeout);
            ui.end_row();
            ui.label("Schemas");
            schemas.show(ui);
            ui.end_row();
        });
    if let Some(scope) = schemas.scope() {
        defaults.schemas = scope;
    }
}

/// A project's rows: each starts at the default, and once changed, can be reset to it.
pub fn edit_overrides(
    ui: &mut Ui,
    overrides: &mut McpOverrides,
    defaults: &McpPermissions,
    schemas: &mut SchemasField,
) {
    Grid::new("mcp_overrides")
        .num_columns(3)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            for (name, value, default, meaning) in [
                ("Read", &mut overrides.read, defaults.read, READ),
                ("Write", &mut overrides.write, defaults.write, WRITE),
                ("Schema", &mut overrides.schema, defaults.schema, SCHEMA),
                ("Admin", &mut overrides.admin, defaults.admin, ADMIN),
            ] {
                let mut allowed = value.unwrap_or(default);
                if ui
                    .checkbox(&mut allowed, name)
                    .on_hover_text(meaning)
                    .changed()
                {
                    *value = Some(allowed);
                }
                ui.weak(meaning);
                reset_or_default(ui, name, value, on_off(default));
                ui.end_row();
            }
            ui.label("Row limit");
            let mut max_rows = overrides.max_rows.unwrap_or(defaults.max_rows);
            if edit_max_rows(ui, &mut max_rows) {
                overrides.max_rows = Some(max_rows);
            }
            reset_or_default(
                ui,
                "Row limit",
                &mut overrides.max_rows,
                defaults.max_rows.to_string(),
            );
            ui.end_row();
            ui.label("Statement timeout");
            let mut timeout = overrides
                .statement_timeout
                .unwrap_or(defaults.statement_timeout);
            if edit_timeout(ui, &mut timeout) {
                overrides.statement_timeout = Some(timeout);
            }
            reset_or_default(
                ui,
                "Statement timeout",
                &mut overrides.statement_timeout,
                defaults.statement_timeout.to_string(),
            );
            ui.end_row();
            ui.label("Schemas");
            if schemas.show(ui)
                && let Some(scope) = schemas.scope()
            {
                overrides.schemas = Some(scope);
            }
            let reset = reset_or_default(
                ui,
                "Schemas",
                &mut overrides.schemas,
                describe_schemas(&defaults.schemas),
            );
            if reset {
                *schemas = SchemasField::new(&defaults.schemas);
            }
            ui.end_row();
        });
}

/// "Reset" for a value the project sets, or a weak "default" for one it doesn't. Returns
/// whether it was reset.
fn reset_or_default<T>(ui: &mut Ui, name: &str, value: &mut Option<T>, default: String) -> bool {
    if value.is_none() {
        ui.weak("default");
        return false;
    }
    let button = ui
        .small_button("Reset")
        .on_hover_text(format!("Back to the default: {default}"));
    let button = widgets::name(button, WidgetType::Button, &format!("Reset {name}"), true);
    if button.clicked() {
        *value = None;
    }
    button.clicked()
}

fn edit_max_rows(ui: &mut Ui, max_rows: &mut NonZeroU32) -> bool {
    let mut rows = max_rows.get();
    let changed = ui
        .add(
            DragValue::new(&mut rows)
                .range(1..=MAX_ROWS_LIMIT)
                .suffix(" rows"),
        )
        .changed();
    if let Some(rows) = NonZeroU32::new(rows) {
        *max_rows = rows;
    }
    changed
}

fn edit_timeout(ui: &mut Ui, timeout: &mut StatementTimeout) -> bool {
    let mut seconds = timeout.duration().as_secs().max(1);
    let changed = ui
        .add(
            DragValue::new(&mut seconds)
                .range(1..=MAX_TIMEOUT_SECONDS)
                .suffix(" s"),
        )
        .changed();
    if changed && let Some(seconds) = NonZeroU64::new(seconds) {
        *timeout = StatementTimeout::from_secs(seconds);
    }
    changed
}

fn on_off(allowed: bool) -> String {
    if allowed { "on" } else { "off" }.to_owned()
}

fn describe_schemas(scope: &SchemaScope) -> String {
    match scope {
        SchemaScope::All => "all".to_owned(),
        SchemaScope::Only(schemas) => schemas.join(", "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_typed_schema_list() {
        let mut field = SchemasField::new(&SchemaScope::All);
        assert_eq!(field.scope(), Some(SchemaScope::All));

        field.only = true;
        assert_eq!(field.scope(), None, "Only, with nothing named");
        field.text = " public, ,Reporting ".to_owned();
        assert_eq!(
            field.scope(),
            Some(SchemaScope::Only(vec![
                "public".to_owned(),
                "Reporting".to_owned()
            ]))
        );
        assert_eq!(
            SchemasField::new(&field.scope().unwrap()).text,
            "public, Reporting"
        );
    }
}
