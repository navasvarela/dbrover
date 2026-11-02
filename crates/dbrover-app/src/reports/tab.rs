//! A report's tab: what it is, what is wrong with it, its parameters and output folder,
//! **Run** and **Cancel**, and its last run's progress, files, preview or error.

use std::path::Path;

use dbrover_core::{ByteCount, DataSource};
use dbrover_export::{ColumnType, CustomFormat};
use dbrover_report::{LimitHit, Plan, Preview, WrittenFile, grouped};
use dbrover_store::ReportName;
use egui::{Grid, RichText, ScrollArea, TextEdit, Ui, vec2};

use super::{ReportAction, Reports, RunEnd, RunRecord};
use crate::form;
use crate::icons;
use crate::theme::Palette;
use crate::widgets;

/// Room for a date and time in each parameter's field.
const VALUE_WIDTH: f32 = 220.0;
/// How tall the preview grows before it scrolls.
const PREVIEW_HEIGHT: f32 = 320.0;

/// What the tab draws from, besides the reports.
pub struct ReportView<'a> {
    pub data_sources: &'a [DataSource],
    pub presets: &'a [CustomFormat],
}

pub fn show(
    ui: &mut Ui,
    reports: &mut Reports,
    name: &ReportName,
    view: &ReportView<'_>,
    actions: &mut Vec<ReportAction>,
) {
    ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| report(ui, reports, name, view, actions));
}

fn report(
    ui: &mut Ui,
    reports: &mut Reports,
    name: &ReportName,
    view: &ReportView<'_>,
    actions: &mut Vec<ReportAction>,
) {
    let palette = Palette::for_visuals(ui.visuals());
    let Some(listed) = reports.find(name) else {
        if reports.listed().is_empty() && reports.unavailable().is_none() {
            ui.weak("Loading…");
        } else {
            ui.weak(format!(
                "There is no {name}.toml in the reports folder any more."
            ));
        }
        return;
    };
    let path = listed.path.clone();
    let title = listed.title().to_owned();
    let description = listed
        .read
        .as_ref()
        .ok()
        .and_then(|report| report.description.clone());
    let plan = listed.plan(view.data_sources, view.presets);

    ui.horizontal(|ui| {
        ui.label(
            widgets::glyph(icons::CLIPBOARD_TEXT)
                .size(20.0)
                .color(palette.primary),
        );
        ui.heading(&title);
    });
    if let Some(description) = description {
        ui.label(description);
    }
    ui.horizontal(|ui| {
        ui.weak(path.display().to_string());
        if ui.button("Edit file").clicked() {
            actions.push(ReportAction::OpenPath(path.clone()));
        }
        if ui.button("Reload").clicked() {
            actions.push(ReportAction::Reload);
        }
    });
    ui.add_space(8.0);

    let plan = match plan {
        Ok(plan) => plan,
        Err(problems) => {
            problems_box(ui, &palette, &problems);
            last_run(ui, reports, name, actions);
            return;
        }
    };
    let ready = parameters(ui, &palette, reports, name, &plan);
    ui.add_space(6.0);
    output_folder(ui, reports, name, actions);
    let files = plan.values(&reports.state_mut(name).values);
    if let Ok(values) = &files {
        ui.horizontal_wrapped(|ui| {
            ui.weak("Writes");
            ui.label(values.files.join(", "));
        });
    }
    ui.add_space(6.0);
    run_buttons(ui, &palette, reports, name, ready && files.is_ok(), actions);
    if let Err(problems) = &files
        && ready
    {
        for problem in problems {
            ui.colored_label(palette.error, problem);
        }
    }
    ui.separator();
    last_run(ui, reports, name, actions);
}

fn problems_box(ui: &mut Ui, palette: &Palette, problems: &[String]) {
    widgets::status(
        ui,
        icons::WARNING,
        palette.error,
        "This report can't run until its file is fixed:",
    );
    for problem in problems {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(20.0);
            ui.colored_label(palette.error, problem);
        });
    }
    ui.weak("Edit the file, save it, then Reload.");
}

/// A field per parameter, each checked as typed. Returns whether every value is fine.
fn parameters(
    ui: &mut Ui,
    palette: &Palette,
    reports: &mut Reports,
    name: &ReportName,
    plan: &Plan,
) -> bool {
    if plan.params.is_empty() {
        return true;
    }
    ui.label(RichText::new("Parameters").strong());
    let state = reports.state_mut(name);
    let mut ready = true;
    Grid::new(("report_parameters", name.as_str()))
        .num_columns(3)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            for param in &plan.params {
                let label = ui.label(&param.name);
                let value = state.values.entry(param.name.clone()).or_default();
                let size = vec2(VALUE_WIDTH, ui.spacing().interact_size.y);
                ui.add_sized(
                    size,
                    TextEdit::singleline(value).hint_text(example(param.param_type)),
                )
                .labelled_by(label.id);
                let check = check(param.param_type, value);
                match &check {
                    Ok(()) => ui.weak(param.param_type.to_string()),
                    Err(problem) => ui.colored_label(palette.error, problem),
                };
                ready &= check.is_ok();
                ui.end_row();
            }
        });
    ready
}

fn example(param_type: ColumnType) -> &'static str {
    match param_type {
        ColumnType::Date => "2026-10-01",
        ColumnType::Timestamp => "2026-10-01 00:00",
        ColumnType::Integer => "42",
        ColumnType::Decimal(_) => "12.50",
        ColumnType::Boolean => "true",
        ColumnType::Text => "",
    }
}

/// What is wrong with a value as typed, as the run would say it.
fn check(param_type: ColumnType, value: &str) -> Result<(), String> {
    if value.trim().is_empty() && param_type != ColumnType::Text {
        return Err(format!("needs a {param_type}"));
    }
    param_type.plain_form(value.trim()).map(drop)
}

fn output_folder(
    ui: &mut Ui,
    reports: &Reports,
    name: &ReportName,
    actions: &mut Vec<ReportAction>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Save to").strong());
        match reports.output_folder(name) {
            Some(folder) => ui.label(folder.display().to_string()),
            None => ui.weak("no folder"),
        };
        if ui.button("Choose…").clicked() {
            actions.push(ReportAction::ChooseFolder(name.clone()));
        }
    });
}

fn run_buttons(
    ui: &mut Ui,
    palette: &Palette,
    reports: &Reports,
    name: &ReportName,
    ready: bool,
    actions: &mut Vec<ReportAction>,
) {
    let running = reports.running();
    ui.horizontal(|ui| {
        let free = running.is_none();
        if form::primary_button(ui, "Run", ready && free).clicked() {
            actions.push(ReportAction::Run(name.clone()));
        }
        match running {
            Some(other) if other == name => {
                if ui.button("Cancel").clicked() {
                    actions.push(ReportAction::Cancel);
                }
            }
            Some(other) => {
                let title = reports
                    .find(other)
                    .map_or(other.as_str(), |listed| listed.title());
                ui.colored_label(
                    palette.text_weak,
                    format!("{title:?} is running; one report runs at a time."),
                );
            }
            None => {}
        }
    });
}

fn last_run(ui: &mut Ui, reports: &Reports, name: &ReportName, actions: &mut Vec<ReportAction>) {
    let Some(record) = reports
        .state(name)
        .and_then(|state| state.last_run.as_ref())
    else {
        return;
    };
    let palette = Palette::for_visuals(ui.visuals());
    match &record.end {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(progress(record));
            });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
        Some(RunEnd::Finished { elapsed_ms }) => {
            widgets::status(
                ui,
                icons::CHECK_CIRCLE,
                palette.success,
                &format!(
                    "Finished in {:.1} s: {} rows read.",
                    *elapsed_ms as f64 / 1000.0,
                    grouped(record.rows)
                ),
            );
            files(ui, &palette, &record.files, actions);
        }
        Some(RunEnd::Failed(failure)) => {
            widgets::status(
                ui,
                icons::X_CIRCLE,
                palette.error,
                &format!("Failed at {}.", failure.step),
            );
            ui.colored_label(palette.error, &failure.message);
            if let Some(limit) = &failure.limit {
                limit_hint(ui, limit);
            }
        }
        Some(RunEnd::Cancelled) => {
            widgets::status(
                ui,
                icons::PROHIBIT,
                palette.text_weak,
                "Cancelled. No file was written.",
            );
        }
    }
    if let Some(preview) = &record.preview {
        ui.add_space(8.0);
        preview_grid(ui, preview);
    }
}

/// What the run has done so far.
fn progress(record: &RunRecord) -> String {
    let elapsed = record.started.elapsed().as_secs();
    let step = if record.source_done {
        "Writing the files"
    } else {
        "Reading the source"
    };
    format!(
        "{step}: {} rows, {} · {elapsed} s",
        grouped(record.rows),
        ByteCount(record.bytes)
    )
}

fn limit_hint(ui: &mut Ui, limit: &LimitHit) {
    ui.label(format!(
        "It stopped at {} = {}. To allow more, raise {} in {} of the report's file.",
        limit.key, limit.value, limit.key, limit.raise_in
    ));
}

fn files(ui: &mut Ui, palette: &Palette, files: &[WrittenFile], actions: &mut Vec<ReportAction>) {
    for file in files {
        let file_name = file.path.file_name().map_or_else(
            || file.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        ui.horizontal(|ui| {
            ui.label(widgets::glyph(file_icon(&file.path)).color(palette.primary));
            if ui
                .link(&file_name)
                .on_hover_text("Open with its app")
                .clicked()
            {
                actions.push(ReportAction::OpenPath(file.path.clone()));
            }
            ui.weak(format!(
                "{} rows, {}",
                grouped(file.rows),
                ByteCount(file.size)
            ));
            if let Some(folder) = file.path.parent()
                && ui.small_button("Show folder").clicked()
            {
                actions.push(ReportAction::OpenPath(folder.to_owned()));
            }
        });
        for warning in &file.warnings {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(20.0);
                ui.colored_label(palette.warning, warning);
            });
        }
    }
}

fn file_icon(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("xlsx") => icons::FILE_XLS,
        Some("csv") => icons::FILE_CSV,
        _ => icons::FILE_TXT,
    }
}

fn preview_grid(ui: &mut Ui, preview: &Preview) {
    let shown = if preview.more {
        format!("The first {} rows", preview.rows.len())
    } else {
        format!("All {} rows", preview.rows.len())
    };
    ui.label(RichText::new(format!("Preview: {shown}, as the source returned them")).strong());
    ScrollArea::both()
        .id_salt("report_preview")
        .max_height(PREVIEW_HEIGHT)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            Grid::new("report_preview_grid")
                .striped(true)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    for column in &preview.columns {
                        ui.label(RichText::new(column).strong());
                    }
                    ui.end_row();
                    for row in &preview.rows {
                        for cell in row {
                            match cell {
                                Some(value) => ui.label(RichText::new(value).monospace()),
                                None => ui.weak(RichText::new("NULL").monospace()),
                            };
                        }
                        ui.end_row();
                    }
                });
        });
}
