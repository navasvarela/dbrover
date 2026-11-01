//! Settings › a project: whether MCP clients may use it, and what they may do there. Changes
//! are a draft, kept across pages until saved or discarded, since a row limit typed digit by
//! digit shouldn't rewrite `projects.toml` at every keystroke.

use std::collections::BTreeMap;

use dbrover_core::{DataSource, ProjectId};
use dbrover_engine::Event;
use dbrover_engine::read_only::McpServing;
use dbrover_engine::read_write;
use dbrover_store::{McpOverrides, McpPermissions, Project};
use egui::{Label, RichText, Ui};

use super::{SettingsAction, search};
use crate::form::{self, Rows};
use crate::mcp_settings::{self, SchemasField};
use crate::theme::Palette;
use crate::{explorer, icons, widgets};

/// What a project's page edits: the two things `Command::SaveProjectMcp` saves.
#[derive(Debug, Clone, PartialEq, Eq)]
struct McpChoice {
    enabled: bool,
    overrides: McpOverrides,
}

impl McpChoice {
    fn of(project: &Project) -> Self {
        Self {
            enabled: project.mcp_enabled,
            overrides: project.mcp.clone(),
        }
    }
}

/// A project's page as edited: what was saved when it started, and the changes.
#[derive(Debug)]
struct ProjectDraft {
    saved: McpChoice,
    draft: McpChoice,
    schemas: SchemasField,
    saving: bool,
    error: Option<String>,
}

impl ProjectDraft {
    fn new(project: &Project, defaults: &McpPermissions) -> Self {
        let schemas = project.mcp.schemas.as_ref().unwrap_or(&defaults.schemas);
        Self {
            saved: McpChoice::of(project),
            draft: McpChoice::of(project),
            schemas: SchemasField::new(schemas),
            saving: false,
            error: None,
        }
    }

    fn changed(&self) -> bool {
        self.draft != self.saved
    }

    /// Follows the project as `projects.toml` now has it. A draft without changes starts again
    /// from it; one with changes keeps them.
    fn follow(&mut self, project: &Project, defaults: &McpPermissions) {
        let saved = McpChoice::of(project);
        if saved == self.saved {
            return;
        }
        if self.changed() {
            self.saved = saved;
        } else {
            *self = Self::new(project, defaults);
        }
    }
}

/// Every project's draft, by id, kept while the tab shows other pages or is closed.
#[derive(Debug, Default)]
pub struct ProjectDrafts {
    drafts: BTreeMap<ProjectId, ProjectDraft>,
}

impl ProjectDrafts {
    /// Whether `project` has changes not saved yet, for the sidebar's dot.
    pub fn changed(&self, project: ProjectId) -> bool {
        self.drafts.get(&project).is_some_and(ProjectDraft::changed)
    }

    /// Takes in a save's result.
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::ProjectSaved(project) => {
                if self
                    .drafts
                    .get(&project.id)
                    .is_some_and(|draft| draft.saving)
                {
                    self.drafts.remove(&project.id);
                }
            }
            Event::ProjectSaveFailed { id, error } => {
                if let Some(draft) = self.drafts.get_mut(id) {
                    draft.saving = false;
                    draft.error = Some(error.clone());
                }
            }
            _ => {}
        }
    }
}

/// What a project's page needs of the app: the project, the defaults it starts from, and its
/// data sources, which the warnings name.
pub struct ProjectView<'a> {
    pub project: &'a Project,
    pub defaults: &'a McpPermissions,
    pub members: Vec<&'a DataSource>,
}

/// The line under the page's title.
pub const SUMMARY: &str = "Whether MCP clients may use this project, and what they may do there.";

/// Whether a search finds `project`: by its name and slug, and by the words of its page.
pub fn matches(project: &Project, query: &str) -> bool {
    let haystack = format!(
        "{} {} project mcp clients allow read write schema admin row limit timeout schemas \
         permissions agent",
        project.name, project.slug
    )
    .to_lowercase();
    search::has_every_word(&haystack, query)
}

/// What the project allows MCP clients, in a few words, for a search result.
pub fn allows(project: &Project, defaults: &McpPermissions) -> String {
    if !project.mcp_enabled {
        return "MCP off".to_owned();
    }
    let permissions = project.mcp.apply_to(defaults);
    let allowed: Vec<&str> = [
        ("read", permissions.read),
        ("write", permissions.write),
        ("schema", permissions.schema),
        ("admin", permissions.admin),
    ]
    .into_iter()
    .filter_map(|(name, allowed)| allowed.then_some(name))
    .collect();
    if allowed.is_empty() {
        "MCP on, allowed nothing".to_owned()
    } else {
        format!("MCP: {}", allowed.join(", "))
    }
}

/// A search result: the project's name, what it allows under it, and Open, which this returns
/// whether was clicked.
pub fn found_row(rows: &mut Rows<'_>, project: &Project, defaults: &McpPermissions) -> bool {
    rows.row(&project.name, Some(&allows(project, defaults)), |ui| {
        ui.link("Open").clicked()
    })
}

impl ProjectDrafts {
    /// Draws the project's page below its title.
    pub fn show(&mut self, ui: &mut Ui, view: &ProjectView<'_>, actions: &mut Vec<SettingsAction>) {
        let project = view.project;
        let draft = self
            .drafts
            .entry(project.id)
            .or_insert_with(|| ProjectDraft::new(project, view.defaults));
        draft.follow(project, view.defaults);
        form::section(ui, "MCP clients", |ui| {
            form::card(ui, |rows| clients(rows, project, &mut draft.draft.enabled));
        });
        form::section(ui, "What MCP clients may do", |ui| {
            form::card(ui, |rows| {
                rows.full(|ui| {
                    ui.add_enabled_ui(draft.draft.enabled, |ui| {
                        mcp_settings::edit_overrides(
                            ui,
                            &mut draft.draft.overrides,
                            view.defaults,
                            &mut draft.schemas,
                        );
                    });
                    if draft.draft.enabled {
                        let permissions = draft.draft.overrides.apply_to(view.defaults);
                        warnings(ui, &permissions, &view.members);
                    }
                });
            });
        });
        if let Some(error) = &draft.error {
            let palette = Palette::for_visuals(ui.visuals());
            widgets::status(
                ui,
                icons::X_CIRCLE,
                palette.error,
                &format!("Could not save: {error}"),
            );
        }
        let changed = draft.changed();
        let valid = draft.schemas.scope().is_some();
        let save = ui
            .horizontal(|ui| {
                let save = form::primary_button(
                    ui,
                    "Save project settings",
                    changed && valid && !draft.saving,
                )
                .on_disabled_hover_text(if valid {
                    "Nothing changed yet"
                } else {
                    "Name at least one schema, or allow them all"
                })
                .clicked();
                if ui
                    .add_enabled(changed && !draft.saving, egui::Button::new("Discard"))
                    .clicked()
                {
                    *draft = ProjectDraft::new(project, view.defaults);
                }
                save
            })
            .inner;
        if save {
            draft.saving = true;
            draft.error = None;
            actions.push(SettingsAction::SaveProject {
                id: project.id,
                mcp_enabled: draft.draft.enabled,
                overrides: draft.draft.overrides.clone(),
            });
        }
    }
}

/// The switch, the command an MCP client runs, and Copy MCP configuration.
fn clients(rows: &mut Rows<'_>, project: &Project, enabled: &mut bool) {
    let title = "Allow MCP clients to use this project";
    let command = format!(
        "An MCP client runs it as: dbrover mcp --project {}",
        project.slug
    );
    rows.row(title, Some(&command), |ui| {
        form::toggle(ui, enabled, title);
    });
    rows.row(
        "Copy MCP configuration",
        Some("The entry an MCP client's configuration needs to run DbRover for this project"),
        |ui| {
            if ui.button("Copy").clicked() {
                explorer::copy_mcp_configuration(ui, project);
            }
        },
    );
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
