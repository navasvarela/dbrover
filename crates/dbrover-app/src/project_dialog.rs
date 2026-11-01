//! Naming a project (New project…, Rename…) and confirming its deletion.

use dbrover_core::{DataSourceId, ProjectId};
use dbrover_engine::Event;
use dbrover_store::Project;
use egui::{Align, Id, Layout, TextEdit, Ui};

use crate::form;
use crate::theme::Palette;
use crate::widgets;

const FIELD_WIDTH: f32 = 280.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectDialogAction {
    Create {
        id: ProjectId,
        name: String,
        /// A data source to move into the project once it exists, when the dialog was opened
        /// from that data source's Move to project menu.
        then_move: Option<DataSourceId>,
    },
    Rename {
        id: ProjectId,
        name: String,
    },
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Create { then_move: Option<DataSourceId> },
    Rename,
}

/// Asks for a project's name, and stays open until the engine has saved it, so a failure
/// shows where the name was typed.
pub struct ProjectNameDialog {
    id: ProjectId,
    purpose: Purpose,
    name: String,
    error: Option<String>,
    focus_requested: bool,
}

impl ProjectNameDialog {
    pub fn create(then_move: Option<DataSourceId>) -> Self {
        Self {
            id: ProjectId::generate(),
            purpose: Purpose::Create { then_move },
            name: String::new(),
            error: None,
            focus_requested: false,
        }
    }

    pub fn rename(project: &Project) -> Self {
        Self {
            id: project.id,
            purpose: Purpose::Rename,
            name: project.name.clone(),
            error: None,
            focus_requested: false,
        }
    }

    /// Updates the dialog from an engine event. Returns `true` once its project has been
    /// saved, meaning the dialog is done.
    pub fn apply(&mut self, event: &Event) -> bool {
        match event {
            Event::ProjectSaved(project) => project.id == self.id,
            Event::ProjectSaveFailed { id, error } if *id == self.id => {
                self.error = Some(error.clone());
                false
            }
            _ => false,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<ProjectDialogAction> {
        let modal =
            widgets::dialog(ctx, Id::new("project_name_dialog")).show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(ProjectDialogAction::Close);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut Ui) -> Option<ProjectDialogAction> {
        let (heading, confirm) = match self.purpose {
            Purpose::Create { .. } => ("New project", "Create"),
            Purpose::Rename => ("Rename project", "Rename"),
        };
        ui.heading(heading);
        let mut submitted = false;
        ui.horizontal(|ui| {
            let label = ui.label("Name");
            let field = ui
                .add(TextEdit::singleline(&mut self.name).desired_width(FIELD_WIDTH))
                .labelled_by(label.id);
            if !std::mem::replace(&mut self.focus_requested, true) {
                field.request_focus();
            }
            submitted = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        });
        if matches!(self.purpose, Purpose::Create { .. }) {
            ui.weak("New projects are hidden from MCP clients until you allow them.");
        }
        if let Some(error) = &self.error {
            let palette = &Palette::for_visuals(ui.visuals());
            ui.colored_label(palette.error, format!("Could not save: {error}"));
        }
        ui.separator();
        let name = self.name.trim();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let confirmed = form::primary_button(ui, confirm, !name.is_empty()).clicked();
            if ui.button("Cancel").clicked() {
                return Some(ProjectDialogAction::Close);
            }
            ((confirmed || submitted) && !name.is_empty()).then(|| self.action(name.to_owned()))
        })
        .inner
    }

    fn action(&self, name: String) -> ProjectDialogAction {
        match self.purpose {
            Purpose::Create { then_move } => ProjectDialogAction::Create {
                id: self.id,
                name,
                then_move,
            },
            Purpose::Rename => ProjectDialogAction::Rename { id: self.id, name },
        }
    }
}

pub enum DeleteProjectChoice {
    Confirmed(ProjectId),
    Cancelled,
}

/// Asks before deleting a project, saying that its data sources stay.
pub struct DeleteProjectConfirmation {
    id: ProjectId,
    name: String,
    data_sources: usize,
}

impl DeleteProjectConfirmation {
    pub fn new(project: &Project, data_sources: usize) -> Self {
        Self {
            id: project.id,
            name: project.name.clone(),
            data_sources,
        }
    }

    pub fn show(&self, ctx: &egui::Context) -> Option<DeleteProjectChoice> {
        let modal = widgets::dialog(ctx, Id::new("delete_project")).show(ctx, |ui| {
            ui.heading("Delete project");
            ui.label(self.message());
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if form::danger_button(ui, "Delete", true).clicked() {
                    Some(DeleteProjectChoice::Confirmed(self.id))
                } else if ui.button("Cancel").clicked() {
                    Some(DeleteProjectChoice::Cancelled)
                } else {
                    None
                }
            })
            .inner
        });
        if modal.should_close() {
            return Some(DeleteProjectChoice::Cancelled);
        }
        modal.inner
    }

    fn message(&self) -> String {
        let moved = match self.data_sources {
            0 => "It has no data sources.".to_owned(),
            1 => "Its data source moves to No project; it isn't deleted.".to_owned(),
            n => format!("Its {n} data sources move to No project; none is deleted."),
        };
        format!("Delete \"{}\"? {moved}", self.name)
    }
}
