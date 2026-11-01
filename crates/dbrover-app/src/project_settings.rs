//! Project settings…: whether MCP clients may use a project, and what they may do there.

use dbrover_core::{DataSource, ProjectId};
use dbrover_engine::Event;
use dbrover_engine::read_only::McpServing;
use dbrover_engine::read_write;
use dbrover_store::{McpOverrides, McpPermissions, Project, ProjectSlug};
use egui::{Align, Id, Label, Layout, Modal, RichText, Ui};

use crate::form;
use crate::icons;
use crate::mcp_settings::{self, SchemasField};
use crate::theme::Palette;
use crate::widgets;

const DIALOG_WIDTH: f32 = 560.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectSettingsAction {
    Save {
        id: ProjectId,
        mcp_enabled: bool,
        overrides: McpOverrides,
    },
    Close,
}

/// Edits a copy of the project's MCP settings, and stays open until the engine has saved
/// them, so a failure shows where they were changed.
pub struct ProjectSettingsDialog {
    id: ProjectId,
    name: String,
    slug: ProjectSlug,
    mcp_enabled: bool,
    overrides: McpOverrides,
    schemas: SchemasField,
    saving: bool,
    error: Option<String>,
}

impl ProjectSettingsDialog {
    pub fn new(project: &Project, defaults: &McpPermissions) -> Self {
        let schemas = project.mcp.schemas.as_ref().unwrap_or(&defaults.schemas);
        Self {
            id: project.id,
            name: project.name.clone(),
            slug: project.slug.clone(),
            mcp_enabled: project.mcp_enabled,
            overrides: project.mcp.clone(),
            schemas: SchemasField::new(schemas),
            saving: false,
            error: None,
        }
    }

    pub fn project(&self) -> ProjectId {
        self.id
    }

    /// Updates the dialog from an engine event. Returns `true` once its project has been
    /// saved, meaning the dialog is done.
    pub fn apply(&mut self, event: &Event) -> bool {
        match event {
            Event::ProjectSaved(project) => self.saving && project.id == self.id,
            Event::ProjectSaveFailed { id, error } if *id == self.id => {
                self.saving = false;
                self.error = Some(error.clone());
                false
            }
            _ => false,
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        defaults: &McpPermissions,
        members: &[&DataSource],
    ) -> Option<ProjectSettingsAction> {
        let modal = Modal::new(Id::new("project_settings"))
            .show(ctx, |ui| self.contents(ui, defaults, members));
        if modal.should_close() {
            return Some(ProjectSettingsAction::Close);
        }
        modal.inner
    }

    fn contents(
        &mut self,
        ui: &mut Ui,
        defaults: &McpPermissions,
        members: &[&DataSource],
    ) -> Option<ProjectSettingsAction> {
        ui.set_width(DIALOG_WIDTH);
        ui.heading(format!("{} — project settings", self.name));
        ui.add_space(ui.spacing().item_spacing.y);
        ui.checkbox(
            &mut self.mcp_enabled,
            "Allow MCP clients to use this project",
        );
        ui.weak(format!(
            "An MCP client runs it as: dbrover mcp --project {}",
            self.slug
        ));
        ui.add_space(ui.spacing().item_spacing.y);
        ui.add_enabled_ui(self.mcp_enabled, |ui| {
            ui.strong("What MCP clients may do");
            mcp_settings::edit_overrides(ui, &mut self.overrides, defaults, &mut self.schemas);
        });
        let permissions = self.overrides.apply_to(defaults);
        if self.mcp_enabled {
            warnings(ui, &permissions, members);
        }
        if let Some(error) = &self.error {
            let palette = &Palette::for_visuals(ui.visuals());
            ui.colored_label(palette.error, format!("Could not save: {error}"));
        }
        ui.separator();
        self.buttons(ui)
    }

    fn buttons(&mut self, ui: &mut Ui) -> Option<ProjectSettingsAction> {
        let schemas_valid = self.schemas.scope().is_some();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let saved = form::primary_button(ui, "Save", !self.saving && schemas_valid)
                .on_disabled_hover_text("Name at least one schema, or allow them all")
                .clicked();
            if ui.button("Cancel").clicked() {
                return Some(ProjectSettingsAction::Close);
            }
            if !saved {
                return None;
            }
            self.saving = true;
            self.error = None;
            Some(ProjectSettingsAction::Save {
                id: self.id,
                mcp_enabled: self.mcp_enabled,
                overrides: self.overrides.clone(),
            })
        })
        .inner
    }
}

/// What the plan asks the settings to say plainly: beyond reading, only a dedicated login's
/// grants are airtight, and some data sources aren't served at all yet.
fn warnings(ui: &mut Ui, permissions: &McpPermissions, members: &[&DataSource]) {
    let palette = &Palette::for_visuals(ui.visuals());
    let changes = permissions.write || permissions.schema || permissions.admin;
    let served = |data_source: &&&DataSource| McpServing::of(data_source) == McpServing::Served;
    let without_login: Vec<&str> = members
        .iter()
        .filter(served)
        .filter(|data_source| {
            data_source.config.driver().uses_password() && data_source.mcp_login.is_none()
        })
        .map(|data_source| data_source.name.as_str())
        .collect();
    if changes && !without_login.is_empty() {
        let text = format!(
            "No MCP login on {}. Beyond reading, DbRover's own checks can be fooled (a function \
             can do anything), so only a dedicated login's grants are airtight. Set one in the \
             data source's settings.",
            without_login.join(", ")
        );
        // A plain status line doesn't wrap, and this one is long enough to widen the dialog.
        ui.horizontal_top(|ui| {
            ui.label(widgets::glyph(icons::WARNING).color(palette.warning));
            ui.add(Label::new(RichText::new(text).color(palette.warning)).wrap());
        });
    }
    // DuckDB files have no logins either, but the MCP server runs no changes on them.
    let without_logins = members.iter().filter(served).any(|data_source| {
        let driver = data_source.config.driver();
        !driver.uses_password() && read_write::mode(driver).is_some()
    });
    if changes && without_logins {
        ui.weak("SQLite files have no logins: for them, DbRover's own checks are all there is.");
    }
    let unserved = |serving: McpServing| -> Vec<&str> {
        members
            .iter()
            .filter(|data_source| McpServing::of(data_source) == serving)
            .map(|data_source| data_source.name.as_str())
            .collect()
    };
    let without_mcp_login = unserved(McpServing::NeedsMcpLogin);
    if !without_mcp_login.is_empty() {
        ui.weak(format!(
            "Not served to MCP clients until they have an MCP login, since their engine has no \
             read-only mode: {}.",
            without_mcp_login.join(", ")
        ));
    }
    let not_served = unserved(McpServing::NotServed);
    if !not_served.is_empty() {
        ui.weak(format!(
            "Not served to MCP clients, since their database can't be opened read-only (a \
             DuckDB database in memory): {}.",
            not_served.join(", ")
        ));
    }
}
