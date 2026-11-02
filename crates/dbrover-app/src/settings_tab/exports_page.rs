//! Settings › Exports: every format `exports.toml` adds, in a card each, what the file got
//! wrong, and links to create and reload it. Formats are written in the file. See
//! `docs/exports.md`.

use dbrover_export::{CustomFormat, CustomFormatKind};
use dbrover_store::CustomFileKind;
use egui::{RichText, Ui};

use super::{SettingsAction, problems, search, source_line};
use crate::custom_files::{CustomFiles, RELOAD_HINT};
use crate::form::{self, Rows};
use crate::keymap::Keymap;
use crate::theme::Palette;

/// Whether a search finds `format`: by its name, id, description or what it writes.
pub fn matches(format: &CustomFormat, query: &str) -> bool {
    let haystack = [
        format.name.as_str(),
        &format.id.keymap_id(),
        &format.summary(),
        &kind(format),
        &format.extension,
        "export format exports.toml",
    ]
    .join(" ")
    .to_lowercase();
    search::has_every_word(&haystack, query)
}

/// Draws the page below its title.
pub fn show(ui: &mut Ui, files: &CustomFiles, keymap: &Keymap, actions: &mut Vec<SettingsAction>) {
    let file = CustomFileKind::Exports;
    source_line(ui, files.path(file), |ui| {
        if ui.link("Reload").on_hover_text(RELOAD_HINT).clicked() {
            actions.push(SettingsAction::ReloadCustomFiles);
        }
        if ui
            .link("Create exports.toml")
            .on_hover_text(
                "Write a file with commented examples, to edit. An existing file is kept.",
            )
            .clicked()
        {
            actions.push(SettingsAction::CreateExports);
        }
    });
    form::description_label(ui, &files.summary(file));
    ui.add_space(8.0);
    problems(ui, files.problems(file));
    if files.exports().is_empty() {
        form::description_label(
            ui,
            "No formats yet. Create exports.toml writes a file of commented examples; add \
             [[export]] tables to it, then click Reload.",
        );
    }
    for format in files.exports() {
        form::section(ui, &format.name, |ui| {
            form::card(ui, |rows| card(rows, format, keymap));
        });
    }
}

/// A search result: the format's name, its id under it, and Open, which this returns whether
/// was clicked.
pub fn found_row(rows: &mut Rows<'_>, format: &CustomFormat) -> bool {
    let id = format.id.keymap_id();
    rows.row(&format.name, Some(&id), |ui| ui.link("Open").clicked())
}

fn card(rows: &mut Rows<'_>, format: &CustomFormat, keymap: &Keymap) {
    rows.row("Id", None, |ui| {
        ui.label(RichText::new(format.id.keymap_id()).monospace());
    });
    rows.row("Kind", None, |ui| ui.label(kind(format)));
    rows.row("File", None, |ui| {
        ui.label(format!(".{}", format.extension))
    });
    rows.row("Copy as", None, |ui| {
        let palette = Palette::for_visuals(ui.visuals());
        if !format.writes_text() {
            ui.label(RichText::new("files only").color(palette.text_weak));
            return;
        }
        let keys: Vec<String> = keymap
            .format_bindings(&format.id)
            .iter()
            .map(|shortcut| ui.ctx().format_shortcut(shortcut))
            .collect();
        if keys.is_empty() {
            ui.label("in the menu; no keys");
        } else {
            ui.label(format!("in the menu, and {}", keys.join(", ")));
        }
    });
    if let Some(description) = &format.description {
        rows.full(|ui| form::description_label(ui, description));
    }
    match &format.kind {
        CustomFormatKind::Preset(preset) => {
            let options = preset.changed_options();
            rows.full(|ui| {
                if options.is_empty() {
                    form::description_label(ui, "The format's default options");
                }
                for (name, value) in options {
                    ui.label(RichText::new(format!("{name} = {value}")).monospace());
                }
            });
        }
    }
}

/// What kind of format it is, in a few words.
fn kind(format: &CustomFormat) -> String {
    match &format.kind {
        CustomFormatKind::Preset(preset) => format!("Preset of {}", preset.format.name()),
    }
}
