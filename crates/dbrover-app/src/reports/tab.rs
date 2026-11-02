//! A report's tab: what it is, what is wrong with it, its parameters and output folder,
//! **Run** and **Cancel**, and its last run: a line per step with its progress, then its
//! files, preview or error.

use std::path::Path;

use dbrover_core::{ByteCount, DataSource};
use dbrover_export::{ColumnType, CustomFormat};
use dbrover_report::{LimitHit, Plan, Preview, Step, WrittenFile, grouped};
use dbrover_store::ReportName;
use egui::{CollapsingHeader, Grid, RichText, ScrollArea, TextEdit, Ui, vec2};

use super::{ReportAction, Reports, RunEnd, RunRecord, StepRecord, StepState};
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
            let rows = if record.combines() {
                "combined"
            } else {
                "read"
            };
            widgets::status(
                ui,
                icons::CHECK_CIRCLE,
                palette.success,
                &format!(
                    "Finished in {:.1} s: {} rows {rows}.",
                    *elapsed_ms as f64 / 1000.0,
                    grouped(record.rows)
                ),
            );
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
    steps(ui, &palette, record);
    if matches!(record.end, Some(RunEnd::Finished { .. })) {
        files(ui, &palette, &record.files, actions);
    }
    if let Some(preview) = &record.preview {
        ui.add_space(8.0);
        preview_grid(ui, preview, record.combines());
    }
}

/// What the run is doing now, and for how long it has run.
fn progress(record: &RunRecord) -> String {
    let elapsed = record.started.elapsed().as_secs();
    let sources = record
        .steps
        .iter()
        .filter(|step| matches!(step.step, Step::Source { .. }));
    let reading = sources.clone().any(|step| !step.is_over());
    let combining = record
        .steps
        .iter()
        .any(|step| step.step == Step::Combine && !step.is_over());
    let phase = match (reading, combining) {
        (true, _) if sources.count() > 1 => "Reading the sources",
        (true, _) => "Reading the source",
        (false, true) => "Combining",
        (false, false) => "Writing the files",
    };
    format!("{phase} · {elapsed} s")
}

/// A line per source and for the combine step: its state, rows, bytes and time, and a loaded
/// source's columns and types.
fn steps(ui: &mut Ui, palette: &Palette, record: &RunRecord) {
    if record.steps.is_empty() {
        return;
    }
    let running = record.end.is_none();
    ui.add_space(4.0);
    Grid::new("report_steps")
        .num_columns(3)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            for step in &record.steps {
                step_state_icon(ui, palette, step, running);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(step_name(step));
                        if let Some(data_source) = &step.data_source {
                            ui.weak(data_source);
                        }
                    });
                    if !step.table.is_empty() {
                        table(ui, step);
                    }
                    for warning in &step.warnings {
                        ui.colored_label(palette.warning, warning);
                    }
                });
                ui.weak(step_detail(step));
                ui.end_row();
            }
        });
}

fn step_state_icon(ui: &mut Ui, palette: &Palette, step: &StepRecord, running: bool) {
    let (glyph, color) = match step.state {
        StepState::Running { .. } if running => {
            ui.spinner();
            return;
        }
        StepState::Waiting | StepState::Running { .. } => (icons::CIRCLE_DASHED, palette.text_weak),
        StepState::Done { .. } => (icons::CHECK_CIRCLE, palette.success),
        StepState::Attached => (icons::LINK_SIMPLE, palette.success),
        StepState::Failed => (icons::X_CIRCLE, palette.error),
    };
    ui.label(widgets::glyph(glyph).color(color));
}

fn step_name(step: &StepRecord) -> String {
    match &step.step {
        Step::Source { id } => format!("Source {id}"),
        Step::Combine => "Combine".to_owned(),
        other => other.to_string(),
    }
}

fn step_detail(step: &StepRecord) -> String {
    match step.state {
        StepState::Waiting if step.attached => "to attach".to_owned(),
        StepState::Waiting => "waiting".to_owned(),
        StepState::Running {
            rows,
            bytes,
            elapsed_ms,
        }
        | StepState::Done {
            rows,
            bytes,
            elapsed_ms,
        } => format!(
            "{} rows, {} · {:.1} s",
            grouped(rows),
            ByteCount(bytes),
            elapsed_ms as f64 / 1000.0
        ),
        StepState::Attached => "attached, read in place".to_owned(),
        StepState::Failed => "failed".to_owned(),
    }
}

/// A loaded source's columns and the types DuckDB gave them, folded away until asked for.
fn table(ui: &mut Ui, step: &StepRecord) {
    CollapsingHeader::new(format!("{} columns in DuckDB", step.table.len()))
        .id_salt(("report_table", step_name(step)))
        .show(ui, |ui| {
            Grid::new(("report_table_columns", step_name(step)))
                .spacing([12.0, 2.0])
                .show(ui, |ui| {
                    for column in &step.table {
                        ui.label(&column.name);
                        ui.weak(RichText::new(&column.type_name).monospace());
                        ui.end_row();
                    }
                });
        });
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

fn preview_grid(ui: &mut Ui, preview: &Preview, combined: bool) {
    let shown = if preview.more {
        format!("The first {} rows", preview.rows.len())
    } else {
        format!("All {} rows", preview.rows.len())
    };
    let from = if combined {
        "the combine step"
    } else {
        "the source"
    };
    ui.label(RichText::new(format!("Preview: {shown}, as {from} returned them")).strong());
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
