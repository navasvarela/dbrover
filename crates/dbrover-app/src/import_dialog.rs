//! File › Import data sources…: a preview of what the chosen file would change, entry by
//! entry, with a choice for each, before anything is saved.

use std::path::PathBuf;

use dbrover_engine::{Command, Event};
use dbrover_store::{
    DataSourceImport, Difference, EntryStatus, ImportChoice, ImportChoices, ImportPlan,
    ImportPreview, MissingPath, ProjectImport,
};
use egui::{Align, Id, Layout, RichText, ScrollArea, Ui};

use crate::form;
use crate::icons;
use crate::theme::Palette;
use crate::widgets;

const DIALOG_WIDTH: f32 = 560.0;
const LIST_HEIGHT: f32 = 380.0;
const CHOICE_WIDTH: f32 = 140.0;

pub enum ImportAction {
    Apply(ImportPlan),
    Close,
}

pub struct ImportDialog {
    path: PathBuf,
    state: State,
}

enum State {
    Reading,
    Unreadable(String),
    Previewing {
        preview: Box<ImportPreview>,
        choices: ImportChoices,
    },
    Applying {
        projects: usize,
        data_sources: usize,
    },
    Applied(Result<(usize, usize), String>),
}

impl ImportDialog {
    /// The dialog for `path`, and the command that reads it.
    pub fn open(path: PathBuf) -> (Self, Command) {
        let command = Command::PreviewImport(path.clone());
        (
            Self {
                path,
                state: State::Reading,
            },
            command,
        )
    }

    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::ImportPreviewed { path, result } if *path == self.path => {
                self.state = match result {
                    Ok(preview) => State::Previewing {
                        choices: preview.default_choices(),
                        preview: preview.clone(),
                    },
                    Err(error) => State::Unreadable(error.clone()),
                };
            }
            Event::ImportApplied(result) => {
                if let State::Applying {
                    projects,
                    data_sources,
                } = self.state
                {
                    self.state = State::Applied(result.clone().map(|()| (projects, data_sources)));
                }
            }
            _ => {}
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<ImportAction> {
        let modal =
            widgets::dialog(ctx, Id::new("import_data_sources")).show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(ImportAction::Close);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut Ui) -> Option<ImportAction> {
        ui.set_width(DIALOG_WIDTH);
        let palette = &Palette::for_visuals(ui.visuals());
        ui.heading("Import data sources");
        ui.label(RichText::new(self.path.display().to_string()).color(palette.text_weak));
        ui.add_space(ui.spacing().item_spacing.y);
        let mut import = None;
        match &mut self.state {
            State::Reading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading the file…");
                });
            }
            State::Unreadable(error) => {
                ui.colored_label(palette.error, format!("Could not import: {error}"));
            }
            State::Previewing { preview, choices } => {
                ScrollArea::vertical()
                    .max_height(LIST_HEIGHT)
                    .show(ui, |ui| entries(ui, preview, choices, palette));
                ui.add_space(ui.spacing().item_spacing.y);
                let plan = preview.plan(choices);
                ui.label(summary(plan.projects.len(), plan.data_sources.len()));
                import = Some(plan);
            }
            State::Applying { .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Importing…");
                });
            }
            State::Applied(Ok((projects, data_sources))) => {
                widgets::status(
                    ui,
                    icons::CHECK_CIRCLE,
                    palette.success,
                    &format!(
                        "Imported {}. Each data source asks for its password when it first \
                         connects.",
                        counted(*projects, *data_sources)
                    ),
                );
            }
            State::Applied(Err(error)) => {
                ui.colored_label(palette.error, format!("Could not import: {error}"));
            }
        }
        ui.separator();
        let (import_clicked, close) = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                let import_clicked = import.as_ref().is_some_and(|plan| {
                    form::primary_button(ui, "Import", !plan.is_empty())
                        .on_disabled_hover_text("Nothing would change")
                        .clicked()
                });
                let close_label = if import.is_some() { "Cancel" } else { "Close" };
                (import_clicked, ui.button(close_label).clicked())
            })
            .inner;
        if close {
            return Some(ImportAction::Close);
        }
        let plan = import.filter(|_| import_clicked)?;
        self.state = State::Applying {
            projects: plan.projects.len(),
            data_sources: plan.data_sources.len(),
        };
        Some(ImportAction::Apply(plan))
    }
}

fn entries(ui: &mut Ui, preview: &ImportPreview, choices: &mut ImportChoices, palette: &Palette) {
    if !preview.projects.is_empty() {
        ui.label(RichText::new("Projects").strong());
        for (index, (entry, choice)) in preview
            .projects
            .iter()
            .zip(&mut choices.projects)
            .enumerate()
        {
            project_entry(ui, index, entry, choice, palette);
        }
        ui.add_space(ui.spacing().item_spacing.y);
    }
    if preview.data_sources.is_empty() {
        ui.label(RichText::new("The file holds no data sources.").color(palette.text_weak));
        return;
    }
    ui.label(RichText::new("Data sources").strong());
    for (index, (entry, choice)) in preview
        .data_sources
        .iter()
        .zip(&mut choices.data_sources)
        .enumerate()
    {
        data_source_entry(ui, index, entry, choice, palette);
    }
}

fn project_entry(
    ui: &mut Ui,
    index: usize,
    entry: &ProjectImport,
    choice: &mut ImportChoice,
    palette: &Palette,
) {
    let name = &entry.incoming.name;
    entry_row(ui, ("project", index), name, &entry.status, choice, palette);
    if let Some(local) = entry
        .local
        .as_ref()
        .filter(|local| local.id != entry.incoming.id)
    {
        detail(
            ui,
            &format!("Matches the project {:?} here by its slug.", local.name),
            palette.text_weak,
        );
    }
    differences(ui, &entry.status, palette);
}

fn data_source_entry(
    ui: &mut Ui,
    index: usize,
    entry: &DataSourceImport,
    choice: &mut ImportChoice,
    palette: &Palette,
) {
    let name = &entry.incoming.name;
    entry_row(
        ui,
        ("data source", index),
        name,
        &entry.status,
        choice,
        palette,
    );
    differences(ui, &entry.status, palette);
    for MissingPath {
        setting,
        path,
        problem,
    } in &entry.missing
    {
        detail(
            ui,
            &format!("{setting} {}: {problem}", path.display()),
            palette.warning,
        );
    }
}

/// The entry's name and status, and its choice on the right.
fn entry_row(
    ui: &mut Ui,
    id: (&str, usize),
    name: &str,
    status: &EntryStatus,
    choice: &mut ImportChoice,
    palette: &Palette,
) {
    ui.horizontal(|ui| {
        ui.label(name);
        let (text, color) = match status {
            EntryStatus::New => ("new", palette.success),
            EntryStatus::Unchanged => ("unchanged", palette.text_weak),
            EntryStatus::Differs(_) => ("differs", palette.warning),
        };
        ui.label(RichText::new(text).small().color(color));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let combo = egui::ComboBox::from_id_salt(id)
                .width(CHOICE_WIDTH)
                .selected_text(choice_label(status, *choice))
                .show_ui(ui, |ui| {
                    for option in status.choices() {
                        ui.selectable_value(choice, *option, choice_label(status, *option));
                    }
                });
            widgets::name(
                combo.response,
                egui::WidgetType::ComboBox,
                &format!("Import choice for {name}"),
                true,
            );
        });
    });
}

fn choice_label(status: &EntryStatus, choice: ImportChoice) -> &'static str {
    match (choice, status) {
        (ImportChoice::Import, EntryStatus::New) => "Import",
        (ImportChoice::Import, _) => "Update",
        (ImportChoice::Skip, _) => "Skip",
        (ImportChoice::Copy, _) => "Import as a copy",
    }
}

fn differences(ui: &mut Ui, status: &EntryStatus, palette: &Palette) {
    let EntryStatus::Differs(differences) = status else {
        return;
    };
    for Difference {
        setting,
        current,
        incoming,
    } in differences
    {
        detail(
            ui,
            &format!("{setting}: {current} → {incoming}"),
            palette.text_weak,
        );
    }
}

fn detail(ui: &mut Ui, text: &str, color: egui::Color32) {
    ui.indent(text, |ui| {
        ui.label(RichText::new(text).small().color(color));
    });
}

fn summary(projects: usize, data_sources: usize) -> String {
    if projects == 0 && data_sources == 0 {
        "Nothing would change.".to_owned()
    } else {
        format!("Imports {}.", counted(projects, data_sources))
    }
}

/// "2 projects and 1 data source", leaving out a count of zero.
fn counted(projects: usize, data_sources: usize) -> String {
    let plural = |count: usize, one: &str, many: &str| {
        format!("{count} {}", if count == 1 { one } else { many })
    };
    let projects = (projects > 0).then(|| plural(projects, "project", "projects"));
    let data_sources =
        (data_sources > 0).then(|| plural(data_sources, "data source", "data sources"));
    match (projects, data_sources) {
        (Some(projects), Some(data_sources)) => format!("{projects} and {data_sources}"),
        (Some(one), None) | (None, Some(one)) => one,
        (None, None) => "nothing".to_owned(),
    }
}
