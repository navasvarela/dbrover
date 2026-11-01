//! Settings › Custom actions: every action `actions.toml` adds, in a card each, what the file
//! got wrong, and links to create and reload it. Actions are written in the file. See
//! `docs/hackability.md`.

use dbrover_store::{ActionBody, CommandAction, CustomAction, CustomFileKind};
use egui::{RichText, Ui};

use super::{SettingsAction, file_line, problems, search};
use crate::command_runs::{CommandRuns, command_text, timeout_text};
use crate::custom_files::CustomFiles;
use crate::form::{self, Rows};
use crate::keymap::Keymap;
use crate::theme::Palette;

/// Whether a search finds `action`: by its name, id, what it applies to, its drivers, or what
/// it runs.
pub fn matches(action: &CustomAction, query: &str) -> bool {
    let haystack = [
        action.name.as_str(),
        &action.id.keymap_id(),
        &targets(action),
        &drivers(action),
        &runs(action),
        "custom actions.toml",
    ]
    .join(" ")
    .to_lowercase();
    search::has_every_word(&haystack, query)
}

/// Draws the page below its title.
pub fn show(
    ui: &mut Ui,
    files: &CustomFiles,
    keymap: &Keymap,
    runs: &CommandRuns,
    actions: &mut Vec<SettingsAction>,
) {
    let kind = CustomFileKind::Actions;
    let about = files
        .path(kind)
        .map(|path| format!("Read from {}", path.display()));
    file_line(ui, about.as_deref(), |ui| {
        if ui
            .link("Reload")
            .on_hover_text("Read actions.toml, snippets.toml and theme.toml again")
            .clicked()
        {
            actions.push(SettingsAction::ReloadCustomFiles);
        }
        if ui
            .link("Create actions.toml")
            .on_hover_text(
                "Write a file with commented examples, to edit. An existing file is kept.",
            )
            .clicked()
        {
            actions.push(SettingsAction::CreateActions);
        }
    });
    form::description_label(ui, &files.summary(kind));
    ui.add_space(8.0);
    problems(ui, files.problems(kind));
    if files.actions().is_empty() {
        form::description_label(
            ui,
            "No actions yet. Create actions.toml writes a file of commented examples; add \
             [[action]] tables to it, then click Reload.",
        );
    }
    for action in files.actions() {
        form::section(ui, &action.name, |ui| {
            form::card(ui, |rows| card(rows, action, keymap, runs));
        });
    }
}

/// A search result: the action's name, its id under it, and Open, which this returns whether
/// was clicked.
pub fn found_row(rows: &mut Rows<'_>, action: &CustomAction) -> bool {
    let id = action.id.keymap_id();
    rows.row(&action.name, Some(&id), |ui| ui.link("Open").clicked())
}

fn card(rows: &mut Rows<'_>, action: &CustomAction, keymap: &Keymap, runs: &CommandRuns) {
    rows.row("Id", None, |ui| {
        ui.label(RichText::new(action.id.keymap_id()).monospace());
    });
    rows.row("On", None, |ui| ui.label(targets(action)));
    rows.row("Drivers", None, |ui| ui.label(drivers(action)));
    rows.row("Keys", None, |ui| {
        let keys: Vec<String> = keymap
            .custom_bindings(&action.id)
            .iter()
            .map(|shortcut| ui.ctx().format_shortcut(shortcut))
            .collect();
        if keys.is_empty() {
            let palette = Palette::for_visuals(ui.visuals());
            ui.label(RichText::new("unbound").color(palette.text_weak));
        } else {
            ui.label(keys.join(", "));
        }
    });
    rows.full(|ui| match &action.body {
        ActionBody::Sql { sql, run } => {
            ui.label("Runs");
            ui.label(RichText::new(sql.source()).monospace());
            if !run {
                form::description_label(ui, "Opens in a console to edit before running");
            }
        }
        ActionBody::Command(command) => {
            ui.label("Runs the program");
            command_lines(ui, command, runs.is_trusted(&action.id, command));
        }
    });
}

/// The command in full, what goes in and out of it, and whether it runs without asking.
fn command_lines(ui: &mut Ui, command: &CommandAction, trusted: bool) {
    ui.label(RichText::new(command_text(command.command.source())).monospace());
    form::description_label(
        ui,
        &format!(
            "stdin {} · output {} · timeout {}",
            command.stdin.name(),
            command.output.name(),
            timeout_text(command.timeout)
        ),
    );
    form::description_label(
        ui,
        if trusted {
            "Trusted: runs without asking"
        } else {
            "Asks before it next runs"
        },
    );
}

fn targets(action: &CustomAction) -> String {
    let targets: Vec<&str> = action
        .applies_to
        .iter()
        .map(|target| target.name())
        .collect();
    targets.join(", ")
}

fn drivers(action: &CustomAction) -> String {
    match &action.drivers {
        Some(drivers) => {
            let names: Vec<&str> = drivers.iter().map(|driver| driver.display_name()).collect();
            names.join(", ")
        }
        None => "all".to_owned(),
    }
}

fn runs(action: &CustomAction) -> String {
    match &action.body {
        ActionBody::Sql { sql, .. } => sql.source().to_owned(),
        ActionBody::Command(command) => command_text(command.command.source()),
    }
}
