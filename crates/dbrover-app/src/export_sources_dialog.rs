//! File › Export data sources…: choosing data sources, or whole projects, to write to a file
//! another machine can import.

use std::collections::BTreeSet;
use std::path::PathBuf;

use dbrover_core::{DataSource, DataSourceId, ProjectId};
use dbrover_engine::{Command, Event};
use dbrover_store::Project;
use egui::{Align, Id, Layout, RichText, ScrollArea, Ui};

use crate::form;
use crate::icons;
use crate::theme::Palette;
use crate::widgets;

const DIALOG_WIDTH: f32 = 420.0;
const LIST_HEIGHT: f32 = 320.0;

pub enum ExportAction {
    /// Ask where to write, then send [`ExportSourcesDialog::command`].
    ChooseFile,
    Close,
}

pub struct ExportSourcesDialog {
    data_sources: BTreeSet<DataSourceId>,
    /// Projects chosen as a whole, which an empty project can only be.
    projects: BTreeSet<ProjectId>,
    status: Status,
}

enum Status {
    Choosing,
    Exporting(PathBuf),
    Exported { path: PathBuf, count: usize },
    Failed(String),
}

impl ExportSourcesDialog {
    pub fn new() -> Self {
        Self {
            data_sources: BTreeSet::new(),
            projects: BTreeSet::new(),
            status: Status::Choosing,
        }
    }

    /// What to send once the user has chosen where the file goes.
    pub fn command(&mut self, path: PathBuf) -> Command {
        self.status = Status::Exporting(path.clone());
        Command::ExportDataSources {
            path,
            data_sources: self.data_sources.iter().copied().collect(),
            projects: self.projects.iter().copied().collect(),
        }
    }

    pub fn apply(&mut self, event: &Event) {
        let Event::DataSourcesExported { path, result } = event else {
            return;
        };
        if !matches!(&self.status, Status::Exporting(exporting) if exporting == path) {
            return;
        }
        self.status = match result {
            Ok(count) => Status::Exported {
                path: path.clone(),
                count: *count,
            },
            Err(error) => Status::Failed(error.clone()),
        };
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        projects: &[Project],
        data_sources: &[DataSource],
    ) -> Option<ExportAction> {
        let modal = widgets::dialog(ctx, Id::new("export_data_sources"))
            .show(ctx, |ui| self.contents(ui, projects, data_sources));
        if modal.should_close() {
            return Some(ExportAction::Close);
        }
        modal.inner
    }

    fn contents(
        &mut self,
        ui: &mut Ui,
        projects: &[Project],
        data_sources: &[DataSource],
    ) -> Option<ExportAction> {
        ui.set_width(DIALOG_WIDTH);
        let palette = &Palette::for_visuals(ui.visuals());
        ui.heading("Export data sources");
        ui.label(
            RichText::new(
                "Writes their settings, and their projects', to a file to import on another \
                 machine. Passwords stay in this machine's keychain.",
            )
            .color(palette.text_weak),
        );
        ui.add_space(ui.spacing().item_spacing.y);
        ScrollArea::vertical()
            .max_height(LIST_HEIGHT)
            .show(ui, |ui| self.choices(ui, projects, data_sources));
        ui.add_space(ui.spacing().item_spacing.y);
        self.status_line(ui, palette);
        ui.separator();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let exporting = matches!(self.status, Status::Exporting(_));
            let chosen = !self.data_sources.is_empty() || !self.projects.is_empty();
            if form::primary_button(ui, "Export…", chosen && !exporting).clicked() {
                return Some(ExportAction::ChooseFile);
            }
            ui.button("Close").clicked().then_some(ExportAction::Close)
        })
        .inner
    }

    /// A checkbox per project, with its data sources under it, then the data sources in no
    /// project.
    fn choices(&mut self, ui: &mut Ui, projects: &[Project], data_sources: &[DataSource]) {
        let members = |project: Option<ProjectId>| {
            data_sources
                .iter()
                .filter(move |ds| ds.project == project)
                .collect::<Vec<_>>()
        };
        for project in projects {
            let in_project = members(Some(project.id));
            self.project_checkbox(ui, project, &in_project);
            ui.indent(project.id, |ui| {
                for data_source in in_project {
                    self.data_source_checkbox(ui, data_source);
                }
            });
        }
        let ungrouped = members(None);
        if ungrouped.is_empty() {
            return;
        }
        if !projects.is_empty() {
            ui.label(RichText::new("No project").strong());
        }
        for data_source in ungrouped {
            self.data_source_checkbox(ui, data_source);
        }
    }

    /// Ticked when the project is chosen and so is each of its data sources. Ticking it
    /// chooses them all; unticking it, none.
    fn project_checkbox(&mut self, ui: &mut Ui, project: &Project, members: &[&DataSource]) {
        let chosen = |id| self.data_sources.contains(&id);
        let mut all = self.projects.contains(&project.id) && members.iter().all(|ds| chosen(ds.id));
        let some = members.iter().any(|ds| chosen(ds.id));
        let label = widgets::icon_text(
            ui,
            icons::FOLDER_SIMPLE,
            Palette::for_visuals(ui.visuals()).text_weak,
            &project.name,
        );
        let partly = some && !all;
        let checkbox = egui::Checkbox::new(&mut all, label).indeterminate(partly);
        if widgets::name(
            ui.add(checkbox),
            egui::WidgetType::Checkbox,
            &project.name,
            all,
        )
        .changed()
        {
            for data_source in members {
                self.set(data_source.id, all);
            }
            if all {
                self.projects.insert(project.id);
            } else {
                self.projects.remove(&project.id);
            }
        }
    }

    fn data_source_checkbox(&mut self, ui: &mut Ui, data_source: &DataSource) {
        let mut chosen = self.data_sources.contains(&data_source.id);
        if ui.checkbox(&mut chosen, &data_source.name).changed() {
            self.set(data_source.id, chosen);
        }
    }

    fn set(&mut self, id: DataSourceId, chosen: bool) {
        if chosen {
            self.data_sources.insert(id);
        } else {
            self.data_sources.remove(&id);
        }
    }

    fn status_line(&self, ui: &mut Ui, palette: &Palette) {
        match &self.status {
            Status::Choosing => {
                let count = self.data_sources.len();
                let text = match count {
                    0 => "Choose data sources or projects to export.".to_owned(),
                    1 => "1 data source chosen.".to_owned(),
                    n => format!("{n} data sources chosen."),
                };
                ui.label(RichText::new(text).color(palette.text_weak));
            }
            Status::Exporting(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Exporting…");
                });
            }
            Status::Exported { path, count } => {
                let noun = if *count == 1 {
                    "data source"
                } else {
                    "data sources"
                };
                widgets::status(
                    ui,
                    icons::CHECK_CIRCLE,
                    palette.success,
                    &format!("Exported {count} {noun} to {}.", path.display()),
                );
            }
            Status::Failed(error) => {
                ui.colored_label(palette.error, format!("Could not export: {error}"));
            }
        }
    }
}
