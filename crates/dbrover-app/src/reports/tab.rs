//! A report's tab (`docs/report-editor.md`, RP8e): a toolbar with the report's state and
//! **Run**; its file, edited in place, beside what a run needs (parameters, output, problems);
//! and under both, the last run, its preview and the runs the history keeps.

use std::path::{Path, PathBuf};

use dbrover_core::{ByteCount, DataSource};
use dbrover_export::{ColumnType, CustomFormat};
use dbrover_report::{
    Batch, HintId, LimitDefaults, LimitHit, LimitSetIn, Plan, Preview, Step, TypedValues,
    WrittenFile, embedded_code, grouped,
};
use dbrover_store::ReportName;
use egui::{CollapsingHeader, Grid, Id, RichText, ScrollArea, TextEdit, Ui, vec2};

use super::highlight::ReportHighlighter;
use super::problems::{TextProblem, quiet_hint};
use super::{
    CheckInputs, OnDisk, ReportAction, ReportFile, Reports, RunEnd, RunPanel, RunRecord, SaveError,
    StepRecord, StepState, runs_table,
};
use super::{compare_view, explain_view, live, profile_view};
use crate::code_editor::highlight::Marks;
use crate::code_editor::{self, EditorView, Severity, find_bar};
use crate::console::LintOptions;
use crate::form;
use crate::icons;
use crate::keymap::{Action, Keymap};
use crate::split::VerticalSplit;
use crate::theme::Palette;
use crate::widgets;

/// The side panel's width when the tab first opens; dragging its edge changes it.
const SIDE_PANEL_WIDTH: f32 = 300.0;
/// Narrower than this, the parameters' fields and the problems' lines are cut short.
const SIDE_PANEL_MIN_WIDTH: f32 = 240.0;
/// Below this width the inputs go above the file, which would be too narrow beside them.
const NARROW_WIDTH: f32 = 720.0;
/// The run panel's height when the tab first opens; dragging the line above it changes it.
const RUN_PANEL_HEIGHT: f32 = 260.0;

/// What the tab draws from, besides the reports.
pub struct ReportView<'a> {
    pub data_sources: &'a [DataSource],
    pub presets: &'a [CustomFormat],
    /// The limits where a report's file leaves one out, from Settings › Reports.
    pub defaults: &'a LimitDefaults,
    pub keymap: &'a Keymap,
    /// The lint rules switched off in `settings.toml`, for the SQL steps.
    pub lint: &'a LintOptions,
}

/// What the keys do while the report's editor has focus.
const EDITOR_ACTIONS: [Action; 9] = [
    Action::Save,
    Action::Format,
    Action::RunCurrent,
    Action::NextProblem,
    Action::PreviousProblem,
    Action::Find,
    Action::Replace,
    Action::FindNext,
    Action::FindPrevious,
];

/// What the tab does to the text once it has drawn: where to put the cursor, a hint to quiet.
#[derive(Default)]
struct After {
    go_to: Option<usize>,
    quiet: Option<HintId>,
    /// **Reload** or **Keep mine**, after the file changed on disk under unsaved text.
    disk_choice: Option<DiskChoice>,
}

#[derive(Clone, Copy)]
enum DiskChoice {
    Reload,
    KeepMine,
}

/// How the text stands, as the toolbar's chip says it.
enum Standing {
    Running(String),
    Problems(usize),
    Warnings(usize),
    Unsaved,
    Ready,
}

/// What the tab knows of a report this frame, read before it draws.
struct Seen {
    title: String,
    description: Option<String>,
    path: PathBuf,
    plan: Option<Plan>,
    /// The plan is of an earlier text: this one has problems.
    plan_is_stale: bool,
    /// Every problem of the text, hints too, in the file's order.
    problems: Vec<TextProblem>,
    unsaved: bool,
    saving: bool,
    save_error: Option<SaveError>,
    disk: OnDisk,
    format_note: Option<String>,
    folder: Option<PathBuf>,
    running_here: bool,
    /// Another report's title, while it runs.
    running_other: Option<String>,
    progress: Option<String>,
}

pub fn show(
    ui: &mut Ui,
    reports: &mut Reports,
    name: &ReportName,
    view: &ReportView<'_>,
    actions: &mut Vec<ReportAction>,
) {
    let Some(seen) = see(ui.ctx(), reports, name, view) else {
        missing(ui, reports, name);
        return;
    };
    let palette = Palette::for_visuals(ui.visuals());
    let editor_id = Id::new(("report_editor", name.as_str()));
    let mut after = After::default();
    if let Some(file) = reports.state_mut(name).file.as_mut() {
        shortcuts(
            ui,
            file,
            &seen.problems,
            view.keymap,
            editor_id,
            name,
            actions,
        );
    }
    toolbar(
        ui,
        &palette,
        reports,
        name,
        &seen,
        view.keymap,
        actions,
        &mut after,
    );
    if let Some(description) = &seen.description {
        ui.add(egui::Label::new(RichText::new(description).weak()).truncate());
    }
    if let Some(note) = &seen.format_note {
        widgets::status(ui, icons::MAGIC_WAND, palette.text_weak, note);
    }
    disk_bar(ui, &palette, &seen, &mut after);
    save_problem(ui, &palette, &seen, name, actions);
    ui.separator();
    let split = VerticalSplit {
        id: Id::new(("report_run_panel_height", name.as_str())),
        bottom_height: RUN_PANEL_HEIGHT,
    };
    let (top_height, bottom_height) = split.heights(ui);
    let width = ui.available_width();
    ui.allocate_ui(vec2(width, top_height), |ui| {
        file_and_inputs(
            ui, reports, name, &seen, view, editor_id, actions, &mut after,
        );
    });
    split.splitter(ui);
    ui.allocate_ui(vec2(width, bottom_height), |ui| {
        run_panel(ui, reports, name, &seen, actions);
    });
    if let Some(file) = reports.state_mut(name).file.as_mut() {
        if let Some(offset) = after.go_to {
            file.editor.put_cursor(offset);
        }
        if let Some(text) = after
            .quiet
            .and_then(|hint| quiet_hint(&file.editor.text, hint))
        {
            file.editor.replace_keeping_cursor(text);
        }
        match after.disk_choice {
            Some(DiskChoice::Reload) => file.reload(),
            Some(DiskChoice::KeepMine) => file.keep_mine(),
            None => {}
        }
    }
}

/// Reads what the tab draws from: the listing, the text's plan and problems, the run.
fn see(
    ctx: &egui::Context,
    reports: &mut Reports,
    name: &ReportName,
    view: &ReportView<'_>,
) -> Option<Seen> {
    // A tab whose file left the reports folder keeps its text, under the file's name.
    let listed = reports.find(name);
    let title = listed.map_or_else(|| name.to_string(), |listed| listed.title().to_owned());
    let description = listed
        .and_then(|listed| listed.read.as_ref().ok())
        .and_then(|report| report.description.clone());
    let folder = reports.output_folder(name);
    let running = reports.running().cloned();
    let progress = reports
        .state(name)
        .and_then(|state| state.last_run.as_ref())
        .filter(|record| record.end.is_none())
        .map(progress);
    let running_other = running
        .as_ref()
        .filter(|other| *other != name)
        .map(|other| {
            reports
                .find(other)
                .map_or_else(|| other.to_string(), |listed| listed.title().to_owned())
        });
    let runs = match reports.runs(name) {
        Some(Ok(runs)) => runs.clone(),
        Some(Err(_)) | None => Vec::new(),
    };
    let inputs = CheckInputs {
        data_sources: view.data_sources,
        presets: view.presets,
        defaults: view.defaults,
        lint: view.lint,
        runs: &runs,
    };
    let file = reports.file_mut(name)?;
    let path = file.path().to_owned();
    let disk = file.disk.clone();
    let (checked, wait) = file.check(&inputs, std::time::Instant::now());
    if let Some(wait) = wait {
        crate::repaint::request_repaint_after_pause(ctx, wait);
    }
    let plan = checked.plan.as_ref().ok().cloned();
    let problems = checked.problems.clone();
    let plan_is_stale = plan.is_none();
    let plan = plan.or_else(|| file.last_plan().cloned());
    Some(Seen {
        title,
        description,
        path,
        disk,
        plan_is_stale,
        plan,
        problems,
        unsaved: file.is_unsaved(),
        saving: file.is_saving(),
        save_error: file.save_error.clone(),
        format_note: file.format_note().map(str::to_owned),
        folder,
        running_here: running.as_ref() == Some(name),
        running_other,
        progress,
    })
}

/// A tab whose report isn't listed, or whose file can't be read.
fn missing(ui: &mut Ui, reports: &Reports, name: &ReportName) {
    let palette = Palette::for_visuals(ui.visuals());
    match reports.find(name) {
        Some(listed) => {
            widgets::status(
                ui,
                icons::WARNING,
                palette.error,
                &format!("{} can't be read.", listed.path.display()),
            );
            if let Err(problems) = &listed.read {
                for problem in problems {
                    ui.colored_label(palette.error, problem);
                }
            }
        }
        None if reports.listed().is_empty() && reports.unavailable().is_none() => {
            ui.weak("Loading…");
        }
        None => {
            ui.weak(format!(
                "There is no {name}.toml in the reports folder any more."
            ));
        }
    }
}

/// The keymap's actions while the editor has focus, taken before the editor sees them, so ⌘↵
/// runs instead of breaking the line.
fn shortcuts(
    ui: &mut Ui,
    file: &mut ReportFile,
    problems: &[TextProblem],
    keymap: &Keymap,
    editor_id: Id,
    name: &ReportName,
    actions: &mut Vec<ReportAction>,
) {
    if !ui.memory(|memory| memory.has_focus(editor_id)) {
        return;
    }
    for action in ui.input_mut(|input| keymap.consume(input, &EDITOR_ACTIONS)) {
        match action {
            Action::Save => actions.push(ReportAction::Save(name.clone())),
            Action::Format => actions.push(ReportAction::Format(name.clone())),
            Action::RunCurrent => actions.push(ReportAction::Run(name.clone())),
            Action::NextProblem => jump(file, problems, true),
            Action::PreviousProblem => jump(file, problems, false),
            Action::Find => file.editor.open_find(false),
            Action::Replace => file.editor.open_find(true),
            Action::FindNext => file.editor.find_next(true),
            Action::FindPrevious => file.editor.find_next(false),
            _ => {}
        }
    }
}

/// Moves the cursor to the next problem after it, or the previous one before it, wrapping
/// around.
fn jump(file: &mut ReportFile, problems: &[TextProblem], forward: bool) {
    let (start, end) = file.editor.ordered_selection();
    let next = if forward {
        problems
            .iter()
            .find(|problem| problem.range.start > end)
            .or_else(|| problems.first())
    } else {
        problems
            .iter()
            .rev()
            .find(|problem| problem.range.start < start)
            .or_else(|| problems.last())
    };
    if let Some(problem) = next {
        file.editor.put_cursor(problem.range.start);
    }
}

/// The report's name and state, and what can be done with it.
#[expect(
    clippy::too_many_arguments,
    reason = "a toolbar's parts, each drawn from a different part of the tab's state"
)]
fn toolbar(
    ui: &mut Ui,
    palette: &Palette,
    reports: &mut Reports,
    name: &ReportName,
    seen: &Seen,
    keymap: &Keymap,
    actions: &mut Vec<ReportAction>,
    after: &mut After,
) {
    let ready = can_run(reports, name, seen);
    ui.horizontal(|ui| {
        ui.label(
            widgets::glyph(icons::CLIPBOARD_TEXT)
                .size(20.0)
                .color(palette.primary),
        );
        let title = if seen.unsaved {
            format!("{} \u{2022}", seen.title)
        } else {
            seen.title.clone()
        };
        ui.heading(title)
            .on_hover_text(seen.path.display().to_string());
        chip(ui, palette, seen, after);
        if let Some(plan) = &seen.plan
            && !seen.plan_is_stale
        {
            ui.weak(summary(plan));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            more_menu(ui, reports, name, seen, actions);
            run_button(ui, seen, &ready, keymap, name, actions);
            explain_button(ui, reports, name, seen, &ready, actions);
            let can_save = match seen.disk {
                OnDisk::Unchanged => seen.unsaved,
                OnDisk::Changed { .. } => false,
                OnDisk::Deleted => true,
            };
            let save = ui.add_enabled(can_save && !seen.saving, egui::Button::new("Save"));
            let format = ui.button("Format").on_hover_text(hint(
                ui,
                keymap,
                Action::Format,
                "Lay out the file and its SQL",
            ));
            if format.clicked() {
                actions.push(ReportAction::Format(name.clone()));
            }
            let save = save.on_hover_text(hint(ui, keymap, Action::Save, "Write the file"));
            if save.clicked() {
                actions.push(ReportAction::Save(name.clone()));
            }
        });
    });
}

/// `description`, with the action's shortcut when it has one.
fn hint(ui: &Ui, keymap: &Keymap, action: Action, description: &str) -> String {
    match keymap.hint(ui.ctx(), action) {
        Some(shortcut) => format!("{description} ({shortcut})"),
        None => description.to_owned(),
    }
}

/// Ready, how many problems, unsaved, or what the run is doing.
fn chip(ui: &mut Ui, palette: &Palette, seen: &Seen, after: &mut After) {
    let count = |severity| {
        seen.problems
            .iter()
            .filter(|problem| problem.severity == severity)
            .count()
    };
    let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
    let standing = match &seen.progress {
        Some(progress) if seen.running_here => Standing::Running(progress.clone()),
        _ if errors > 0 => Standing::Problems(errors),
        _ if warnings > 0 => Standing::Warnings(warnings),
        _ if seen.unsaved => Standing::Unsaved,
        _ => Standing::Ready,
    };
    let first = |severity| {
        seen.problems
            .iter()
            .find(|problem| problem.severity == severity)
            .map(|problem| problem.range.start)
    };
    match standing {
        Standing::Running(progress) => {
            ui.spinner();
            ui.label(progress);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
        Standing::Problems(count) => {
            let text = counted(count, "problem", "problems");
            if chip_button(ui, icons::X_CIRCLE, palette.error, &text) {
                after.go_to = first(Severity::Error);
            }
        }
        Standing::Warnings(count) => {
            let text = counted(count, "warning", "warnings");
            if chip_button(ui, icons::WARNING, palette.warning, &text) {
                after.go_to = first(Severity::Warning);
            }
        }
        Standing::Unsaved => {
            ui.weak("Unsaved");
        }
        Standing::Ready => {
            ui.label(widgets::icon_text(
                ui,
                icons::CHECK_CIRCLE,
                palette.success,
                "Ready",
            ));
        }
    }
}

/// `1 problem`, `2 problems`.
fn counted(count: usize, one: &str, many: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

/// The chip as a button that goes to the first problem it counts. Returns whether it was
/// clicked.
fn chip_button(ui: &mut Ui, glyph: &str, color: egui::Color32, text: &str) -> bool {
    let job = widgets::icon_text(ui, glyph, color, text);
    let response = ui
        .add(egui::Button::new(job).frame(false))
        .on_hover_text("Go to the first one (F8 goes to the next)");
    widgets::name(response, egui::WidgetType::Button, text, true).clicked()
}

/// `1 source · 3 files`, the outputs' and the charts'.
fn summary(plan: &Plan) -> String {
    let sources = plan.source_starts().len();
    let outputs = plan.outputs.len() + plan.charts.len();
    let count = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    format!(
        "{} · {}",
        count(sources, "source", "sources"),
        count(outputs, "file", "files")
    )
}

/// Whether the report can run now, or why not.
fn can_run(reports: &Reports, name: &ReportName, seen: &Seen) -> Result<(), String> {
    if let Some(other) = &seen.running_other {
        return Err(format!("{other:?} is running; one report runs at a time."));
    }
    if seen.running_here {
        return Err("It is running.".to_owned());
    }
    if seen.saving {
        return Err("Its file is being saved.".to_owned());
    }
    match seen.disk {
        OnDisk::Unchanged => {}
        OnDisk::Changed { .. } => {
            return Err("Its file changed on disk: Reload or Keep mine first.".to_owned());
        }
        OnDisk::Deleted => return Err("Its file was deleted: Save writes it again.".to_owned()),
    }
    let Some(plan) = seen.plan.as_ref().filter(|_| !seen.plan_is_stale) else {
        return Err("Its file has problems to fix first.".to_owned());
    };
    let values = reports
        .state(name)
        .map(|state| state.values.clone())
        .unwrap_or_default();
    if plan.params.iter().any(|param| {
        check(
            param.param_type,
            values.get(&param.name).map_or("", String::as_str),
        )
        .is_err()
    }) {
        return Err("A parameter's value isn't right yet.".to_owned());
    }
    plan.values(&values)
        .map(drop)
        .map_err(|problems| problems.join("; "))
}

/// **Run**, or **Save and run** while the text is unsaved, or **Cancel** while it runs.
fn run_button(
    ui: &mut Ui,
    seen: &Seen,
    ready: &Result<(), String>,
    keymap: &Keymap,
    name: &ReportName,
    actions: &mut Vec<ReportAction>,
) {
    if seen.running_here {
        if ui.button("Cancel").clicked() {
            actions.push(ReportAction::Cancel);
        }
        return;
    }
    let label = if seen.unsaved { "Save and run" } else { "Run" };
    let response = form::primary_button(ui, label, ready.is_ok());
    let response = match ready {
        Ok(()) => response.on_hover_text(hint(ui, keymap, Action::RunCurrent, "Run the report")),
        Err(reason) => response.on_disabled_hover_text(reason),
    };
    if response.clicked() {
        actions.push(ReportAction::Run(name.clone()));
    }
}

/// **Explain**: plans the saved report without running it, with the parameters typed.
fn explain_button(
    ui: &mut Ui,
    reports: &Reports,
    name: &ReportName,
    seen: &Seen,
    ready: &Result<(), String>,
    actions: &mut Vec<ReportAction>,
) {
    let explaining = reports.is_explaining(name);
    let can = ready.is_ok() && !seen.unsaved && !explaining;
    let response = ui.add_enabled(can, egui::Button::new("Explain"));
    let response = match ready {
        Err(reason) => response.on_disabled_hover_text(reason),
        Ok(()) if seen.unsaved => response.on_disabled_hover_text("Save the file to explain it"),
        Ok(()) if explaining => response.on_disabled_hover_text("Explaining…"),
        Ok(()) => response.on_hover_text(
            "Plan the report without running it: each source's plan and estimated cost, and \
             the combine step's plan",
        ),
    };
    if response.clicked() {
        actions.push(ReportAction::Explain(name.clone()));
    }
}

/// What is done to the report as a whole, and to its file.
fn more_menu(
    ui: &mut Ui,
    reports: &mut Reports,
    name: &ReportName,
    seen: &Seen,
    actions: &mut Vec<ReportAction>,
) {
    let hidden = reports.state(name).is_some_and(|state| state.file_hidden);
    widgets::menu_button(ui, widgets::glyph(icons::DOTS_THREE), |ui| {
        if ui.button("Rename…").clicked() {
            actions.push(ReportAction::Rename(name.clone()));
        }
        if let Some(folder) = seen.path.parent()
            && ui.button("Show in folder").clicked()
        {
            actions.push(ReportAction::OpenPath(folder.to_owned()));
        }
        if ui.button("Open in another app").clicked() {
            actions.push(ReportAction::OpenPath(seen.path.clone()));
        }
        if ui.button("Copy path").clicked() {
            ui.ctx().copy_text(seen.path.display().to_string());
        }
        ui.separator();
        let toggle = if hidden {
            "Show the file"
        } else {
            "Hide the file"
        };
        if ui.button(toggle).clicked() {
            reports.state_mut(name).file_hidden = !hidden;
        }
    });
}

/// What another program did to the file, while that matters: changed under unsaved text
/// (which to keep?), or deleted (Save writes it again). W6's bar for a console's file.
fn disk_bar(ui: &mut Ui, palette: &Palette, seen: &Seen, after: &mut After) {
    let file = seen.path.file_name().map_or_else(
        || seen.title.clone(),
        |name| name.to_string_lossy().into_owned(),
    );
    match &seen.disk {
        OnDisk::Unchanged => {}
        OnDisk::Changed { .. } => {
            ui.horizontal_wrapped(|ui| {
                let text = format!("{file} changed on disk, and you have unsaved changes");
                ui.label(widgets::icon_text(
                    ui,
                    icons::WARNING,
                    palette.warning,
                    &text,
                ));
                if ui
                    .button("Reload")
                    .on_hover_text("Show what the file holds now, dropping your changes")
                    .clicked()
                {
                    after.disk_choice = Some(DiskChoice::Reload);
                }
                if ui
                    .button("Keep mine")
                    .on_hover_text("Keep your text; saving it replaces the file")
                    .clicked()
                {
                    after.disk_choice = Some(DiskChoice::KeepMine);
                }
            });
        }
        OnDisk::Deleted => {
            let text = format!("{file} was deleted from the reports folder; Save writes it again");
            ui.label(widgets::icon_text(
                ui,
                icons::WARNING,
                palette.warning,
                &text,
            ));
        }
    }
}

/// Why the last save failed, until one succeeds.
fn save_problem(
    ui: &mut Ui,
    palette: &Palette,
    seen: &Seen,
    name: &ReportName,
    actions: &mut Vec<ReportAction>,
) {
    match &seen.save_error {
        None => {}
        Some(SaveError::ChangedOnDisk) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::icon_text(
                    ui,
                    icons::WARNING,
                    palette.warning,
                    "The file changed on disk since it was read, so it wasn't saved.",
                ));
                if ui
                    .button("Save over it")
                    .on_hover_text("Replace what the file holds now with your text")
                    .clicked()
                {
                    actions.push(ReportAction::Save(name.clone()));
                }
            });
        }
        Some(SaveError::Failed(error)) => {
            widgets::status(
                ui,
                icons::X_CIRCLE,
                palette.error,
                &format!("The file couldn't be saved: {error}"),
            );
        }
    }
}

/// The file beside the run's inputs, or above them in a narrow tab, or the inputs alone when
/// the file is hidden.
#[expect(
    clippy::too_many_arguments,
    reason = "the editor and the inputs, drawn from different parts of the tab's state"
)]
fn file_and_inputs(
    ui: &mut Ui,
    reports: &mut Reports,
    name: &ReportName,
    seen: &Seen,
    view: &ReportView<'_>,
    editor_id: Id,
    actions: &mut Vec<ReportAction>,
    after: &mut After,
) {
    let hidden = reports.state(name).is_some_and(|state| state.file_hidden);
    if hidden {
        ScrollArea::vertical()
            .id_salt(("report_inputs", name.as_str()))
            .auto_shrink(false)
            .show(ui, |ui| inputs(ui, reports, name, seen, actions, after));
        return;
    }
    if ui.available_width() < NARROW_WIDTH {
        let inputs_height = ui.available_height() * 0.4;
        ScrollArea::vertical()
            .id_salt(("report_inputs", name.as_str()))
            .max_height(inputs_height)
            .show(ui, |ui| inputs(ui, reports, name, seen, actions, after));
        ui.separator();
        editor(ui, reports, name, seen, view, editor_id);
        return;
    }
    egui::Panel::right(Id::new(("report_side_panel", name.as_str())))
        .resizable(true)
        .default_size(SIDE_PANEL_WIDTH)
        .min_size(SIDE_PANEL_MIN_WIDTH)
        .show(ui, |ui| {
            ScrollArea::vertical()
                .id_salt(("report_inputs", name.as_str()))
                .auto_shrink(false)
                .show(ui, |ui| inputs(ui, reports, name, seen, actions, after));
        });
    editor(ui, reports, name, seen, view, editor_id);
}

/// The report's file, with the find bar over it.
fn editor(
    ui: &mut Ui,
    reports: &mut Reports,
    name: &ReportName,
    seen: &Seen,
    view: &ReportView<'_>,
    editor_id: Id,
) {
    let Some(file) = reports.state_mut(name).file.as_mut() else {
        return;
    };
    let find = find_bar::show(ui, &mut file.editor, view.keymap, editor_id);
    let marks = Marks {
        code_blocks: embedded_code(&file.editor.text)
            .into_iter()
            .map(|code| code.range)
            .collect(),
        current_match: find
            .found
            .current
            .map(|index| find.found.matches[index].clone()),
        matches: find.found.matches.clone(),
        ..Marks::default()
    };
    let highlighter = ReportHighlighter {
        data_sources: view.data_sources,
    };
    ScrollArea::vertical()
        .id_salt(("report_editor_scroll", name.as_str()))
        .auto_shrink(false)
        .show(ui, |ui| {
            let view = EditorView {
                id: editor_id,
                hint_text: "[report]",
                highlighter: &highlighter,
                marks: &marks,
                problems: &seen.problems,
            };
            code_editor::edit(ui, &mut file.editor, &view);
        });
}

/// The text's problems, then what a run needs, as the text says it: the parameters' values,
/// where its files go and which they are.
fn inputs(
    ui: &mut Ui,
    reports: &mut Reports,
    name: &ReportName,
    seen: &Seen,
    actions: &mut Vec<ReportAction>,
    after: &mut After,
) {
    let palette = Palette::for_visuals(ui.visuals());
    // First when there are any: they are what is read first.
    problems(ui, &palette, seen, after);
    if let Some(plan) = &seen.plan {
        ui.add_enabled_ui(!seen.plan_is_stale, |ui| {
            let state = reports.state_mut(name);
            parameters(ui, &palette, &mut state.values, name, plan);
            output(ui, &palette, &state.values, seen, name, plan, actions);
        });
    }
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong());
    ui.add_space(2.0);
}

/// A field per parameter, each checked as typed.
fn parameters(
    ui: &mut Ui,
    palette: &Palette,
    values: &mut TypedValues,
    name: &ReportName,
    plan: &Plan,
) {
    if plan.params.is_empty() {
        return;
    }
    section(ui, "Parameters");
    let width = (ui.available_width() - 120.0).max(80.0);
    Grid::new(("report_parameters", name.as_str()))
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            for param in &plan.params {
                let label = ui.label(&param.name);
                let value = values.entry(param.name.clone()).or_default();
                ui.vertical(|ui| {
                    let size = vec2(width, ui.spacing().interact_size.y);
                    ui.add_sized(
                        size,
                        TextEdit::singleline(value).hint_text(example(param.param_type)),
                    )
                    .labelled_by(label.id);
                    match check(param.param_type, value) {
                        Ok(()) => ui.weak(param.param_type.to_string()),
                        Err(problem) => ui.colored_label(palette.error, problem),
                    };
                });
                ui.end_row();
            }
        });
}

/// The folder the files go in, and the files this run writes.
fn output(
    ui: &mut Ui,
    palette: &Palette,
    values: &TypedValues,
    seen: &Seen,
    name: &ReportName,
    plan: &Plan,
    actions: &mut Vec<ReportAction>,
) {
    section(ui, "Output");
    ui.horizontal_wrapped(|ui| {
        ui.weak("Folder");
        match &seen.folder {
            Some(folder) => ui.label(folder.display().to_string()),
            None => ui.weak("no folder"),
        };
        if ui.button("Choose…").clicked() {
            actions.push(ReportAction::ChooseFolder(name.clone()));
        }
    });
    match plan.values(values) {
        Ok(run) => {
            for file in run.files.iter().chain(&run.charts) {
                ui.horizontal(|ui| {
                    ui.label(widgets::glyph(file_icon(Path::new(file))).color(palette.primary));
                    ui.label(file);
                });
            }
        }
        Err(_) => {
            ui.weak("The files are named once the parameters have values.");
        }
    }
}

/// Each problem on a line of its own, its line number first, a hint with **Quiet**; a click
/// goes to it.
fn problems(ui: &mut Ui, palette: &Palette, seen: &Seen, after: &mut After) {
    if seen.problems.is_empty() {
        return;
    }
    section(ui, "Problems");
    for problem in &seen.problems {
        let (glyph, color) = match problem.severity {
            Severity::Error => (icons::X_CIRCLE, palette.error),
            Severity::Warning => (icons::WARNING, palette.warning),
            Severity::Hint => (icons::LIGHTBULB, palette.primary),
        };
        ui.horizontal(|ui| {
            ui.label(widgets::glyph(glyph).color(color));
            if let Some(hint) = problem.hint
                && ui
                    .small_button("Quiet")
                    .on_hover_text(format!("Add \"{hint}\" to [report] quiet_hints"))
                    .clicked()
            {
                after.quiet = Some(hint);
            }
            let text = format!("line {}  {}", problem.line, problem.message);
            let response = ui
                .add(
                    egui::Label::new(RichText::new(text).color(color))
                        .truncate()
                        .sense(egui::Sense::click()),
                )
                .on_hover_text(&problem.message);
            if response.clicked() {
                after.go_to = Some(problem.range.start);
            }
        });
    }
}

/// The last run, its preview, and the runs the history keeps, a tab each.
fn run_panel(
    ui: &mut Ui,
    reports: &mut Reports,
    name: &ReportName,
    seen: &Seen,
    actions: &mut Vec<ReportAction>,
) {
    let state = reports.state(name);
    let has_run = state.is_some_and(|state| state.last_run.is_some());
    let shown = state
        .and_then(|state| state.run_panel)
        .unwrap_or(if has_run {
            RunPanel::LastRun
        } else {
            RunPanel::Runs
        });
    let past = match reports.runs(name) {
        Some(Ok(runs)) if !runs.is_empty() => format!("Past runs {}", runs.len()),
        _ => "Past runs".to_owned(),
    };
    ui.horizontal(|ui| {
        for (panel, label) in [
            (RunPanel::LastRun, "Last run"),
            (RunPanel::Preview, "Preview"),
            (RunPanel::Profile, "Profile"),
            (RunPanel::Compare, "Compare"),
            (RunPanel::Runs, past.as_str()),
            (RunPanel::Explain, "Explain"),
        ] {
            if ui.selectable_label(shown == panel, label).clicked() {
                reports.state_mut(name).run_panel = Some(panel);
            }
        }
    });
    ScrollArea::both()
        .id_salt(("report_run_panel", name.as_str()))
        .auto_shrink(false)
        .show(ui, |ui| match shown {
            RunPanel::LastRun => {
                // What the next run sends, beside what the last one did.
                if let Some(sql) = seen.plan.as_ref().and_then(Plan::compiled_combine) {
                    compiled_sql(ui, sql, "Combine SQL, compiled from PRQL", false);
                }
                last_run(ui, reports, name, actions);
            }
            RunPanel::Preview => preview(ui, reports, name),
            RunPanel::Profile => profile_view::show(ui, reports, name),
            RunPanel::Compare => compare_view::show(ui, reports, name),
            RunPanel::Runs => runs_table::show(ui, reports, name, actions),
            RunPanel::Explain => explain_view::show(ui, reports, name),
        });
}

fn preview(ui: &mut Ui, reports: &Reports, name: &ReportName) {
    let record = reports
        .state(name)
        .and_then(|state| state.last_run.as_ref());
    match record.and_then(|record| Some((record.preview.as_ref()?, record.combines()))) {
        Some((preview, combined)) => preview_grid(ui, preview, combined),
        None => {
            ui.weak("A run's first rows show here once it writes them.");
        }
    }
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

fn last_run(ui: &mut Ui, reports: &Reports, name: &ReportName, actions: &mut Vec<ReportAction>) {
    let Some(record) = reports
        .state(name)
        .and_then(|state| state.last_run.as_ref())
    else {
        ui.weak("No run since DbRover started. Run the report to see each step here.");
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
                limit_hint(ui, limit, actions);
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
                    if let Some(sql) = &step.sql {
                        // DuckDB's errors name a line of the SQL, so a failed step shows it.
                        let failed = step.state == StepState::Failed;
                        compiled_sql(ui, sql, "The SQL it ran, compiled from PRQL", failed);
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
        StepState::Waiting => match &step.waits_for {
            Some(source) => format!("waiting for {source}"),
            None => "waiting".to_owned(),
        },
        StepState::Running {
            rows,
            bytes,
            elapsed_ms,
        } => {
            let mut read = amounts(rows, bytes, elapsed_ms);
            let waits_on = live::waits_on(&step.step, step.engine.as_deref());
            let now = live::describe(&step.pace, step.busy.as_ref(), waits_on, " · ");
            if !now.is_empty() {
                read = format!("{read} · {now}");
            }
            match step.batches {
                Some(batch) => format!("batch {} of {} · {read}", batch.number, batch.of),
                None => read,
            }
        }
        StepState::Done {
            rows,
            bytes,
            elapsed_ms,
        } => {
            let read = amounts(rows, bytes, elapsed_ms);
            match step.batches {
                Some(Batch { of: 1, .. }) => format!("1 batch · {read}"),
                Some(batch) => format!("{} batches · {read}", batch.of),
                None => read,
            }
        }
        StepState::Attached => "attached, read in place".to_owned(),
        StepState::Failed => "failed".to_owned(),
    }
}

/// `1,500 rows, 30 KB · 1.2 s`.
fn amounts(rows: u64, bytes: u64, elapsed_ms: u64) -> String {
    format!(
        "{} rows, {} · {:.1} s",
        grouped(rows),
        ByteCount(bytes),
        elapsed_ms as f64 / 1000.0
    )
}

/// The SQL a combine step's PRQL compiled to, folded away unless `open`, with **Copy**.
fn compiled_sql(ui: &mut Ui, sql: &str, heading: &str, open: bool) {
    CollapsingHeader::new(heading)
        .id_salt(("report_compiled_sql", heading))
        .default_open(open)
        .show(ui, |ui| {
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(sql.to_owned());
            }
            ui.add(
                egui::Label::new(RichText::new(sql).monospace())
                    .selectable(true)
                    .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
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
                        if column.declared {
                            ui.weak("declared");
                        }
                        ui.end_row();
                    }
                });
        });
}

/// Where to raise the limit a run stopped at: the report's file, or Settings › Reports for a
/// default, as the limit says who set it.
fn limit_hint(ui: &mut Ui, limit: &LimitHit, actions: &mut Vec<ReportAction>) {
    ui.label(limit.advice());
    let settings = match limit.set_in {
        Some(LimitSetIn::Settings) => "Open Settings › Reports",
        // From a worker of before RP4h, which didn't say: the default may be what stopped it.
        None if limit.key != "continue_on_new_sheet" => "Settings › Reports",
        Some(LimitSetIn::File) | None => return,
    };
    if ui.link(settings).clicked() {
        actions.push(ReportAction::OpenSettings);
    }
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
        Some("parquet") => icons::TABLE,
        Some("svg" | "png") => icons::CHART_BAR,
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
    // The run panel scrolls both ways, so the grid takes the room its rows need.
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
}
