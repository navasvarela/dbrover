//! Settings › Keymap: every action's keys, in a card per part of the app, what `keymap.toml`
//! got wrong, and links to create and reload the file. Keys are changed in the file.

use std::borrow::Cow;
use std::path::Path;

use dbrover_store::CustomAction;
use egui::{KeyboardShortcut, RichText, Ui};

use super::{SettingsAction, problems, search, source_line};
use crate::form::{self, Rows};
use crate::keymap::{Action, Keymap};
use crate::theme::Palette;

/// Where an action is used, from the first part of its id. The cards follow this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeymapSection {
    Console,
    Grid,
    Editor,
    File,
    App,
    Custom,
}

impl KeymapSection {
    /// The section of a built-in action's id, such as `console.run`.
    fn of(id: &str) -> Self {
        match id.split_once('.').map(|(area, _)| area) {
            Some("console") => Self::Console,
            Some("grid") => Self::Grid,
            Some("editor") => Self::Editor,
            Some("file") => Self::File,
            _ => Self::App,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Console => "Console",
            Self::Grid => "Grid",
            Self::Editor => "Editor",
            Self::File => "File",
            Self::App => "App",
            Self::Custom => "Custom actions",
        }
    }
}

/// An action and the keys it has: one row of the page.
pub struct Shortcut<'a> {
    pub section: KeymapSection,
    /// The name `keymap.toml` binds it by.
    pub id: Cow<'a, str>,
    /// What the action does, or a custom action's name.
    pub what: &'a str,
    /// Its keys as the menus show them, such as "⌘S".
    pub keys: Vec<String>,
}

impl Shortcut<'_> {
    pub fn matches(&self, query: &str) -> bool {
        let haystack = [
            self.what,
            &self.id,
            self.section.title(),
            &self.keys.join(" "),
            "keymap shortcut keys",
        ]
        .join(" ")
        .to_lowercase();
        search::has_every_word(&haystack, query)
    }
}

/// Every action's row, DbRover's own and then `custom`'s, in the page's order, with keys as
/// `format` writes them (the menus' way, `Context::format_shortcut`, in the app).
pub fn shortcuts<'a>(
    format: impl Fn(&KeyboardShortcut) -> String,
    keymap: &'a Keymap,
    custom: &'a [CustomAction],
) -> Vec<Shortcut<'a>> {
    let keys =
        |bindings: &[KeyboardShortcut]| -> Vec<String> { bindings.iter().map(&format).collect() };
    let mut shortcuts: Vec<Shortcut<'a>> = Action::ALL
        .into_iter()
        .map(|action| Shortcut {
            section: KeymapSection::of(action.id()),
            id: Cow::Borrowed(action.id()),
            what: action.description(),
            keys: keys(keymap.bindings(action)),
        })
        .collect();
    // Stable, so each card keeps the actions in `Action::ALL`'s order.
    shortcuts.sort_by_key(|shortcut| shortcut.section);
    shortcuts.extend(custom.iter().map(|action| Shortcut {
        section: KeymapSection::Custom,
        id: Cow::Owned(action.id.keymap_id()),
        what: &action.name,
        keys: keys(keymap.custom_bindings(&action.id)),
    }));
    shortcuts
}

/// Draws the page below its title.
pub fn show(
    ui: &mut Ui,
    keymap: &Keymap,
    path: Option<&Path>,
    shortcuts: &[Shortcut<'_>],
    actions: &mut Vec<SettingsAction>,
) {
    source_line(ui, path, |ui| {
        if ui
            .link("Reload")
            .on_hover_text("Read keymap.toml again, after editing it")
            .clicked()
        {
            actions.push(SettingsAction::ReloadKeymap);
        }
        if ui
            .link("Create keymap.toml")
            .on_hover_text(
                "Write a file listing every action with its keys, commented out, to edit. An \
                 existing file is kept.",
            )
            .clicked()
        {
            actions.push(SettingsAction::CreateKeymap);
        }
    });
    ui.add_space(8.0);
    problems(ui, keymap.problems());
    for group in shortcuts.chunk_by(|a, b| a.section == b.section) {
        form::section(ui, group[0].section.title(), |ui| {
            form::card(ui, |rows| {
                for shortcut in group {
                    row(rows, shortcut);
                }
            });
        });
    }
}

/// What the action does, its id under it, and its keys at the right.
pub fn row(rows: &mut Rows<'_>, shortcut: &Shortcut<'_>) {
    rows.row(shortcut.what, Some(&shortcut.id), |ui| {
        let palette = Palette::for_visuals(ui.visuals());
        if shortcut.keys.is_empty() {
            ui.label(RichText::new("unbound").color(palette.text_weak));
        } else {
            ui.label(shortcut.keys.join(", "));
        }
    });
}

#[cfg(test)]
mod tests {
    use egui::ModifierNames;

    use super::*;

    #[test]
    fn puts_each_action_in_the_section_its_id_starts_with() {
        assert_eq!(KeymapSection::of("console.run"), KeymapSection::Console);
        assert_eq!(KeymapSection::of("grid.copy-as.csv"), KeymapSection::Grid);
        assert_eq!(KeymapSection::of("app.settings"), KeymapSection::App);
        for action in Action::ALL {
            let id = action.id();
            if KeymapSection::of(id) == KeymapSection::App {
                assert!(id.starts_with("app."), "{id} needs a section of its own");
            }
        }
    }

    #[test]
    fn finds_a_shortcut_by_what_it_does_its_id_or_its_keys() {
        let format = |shortcut: &KeyboardShortcut| shortcut.format(&ModifierNames::NAMES, false);
        let keymap = Keymap::default();
        let shortcuts = shortcuts(format, &keymap, &[]);
        let found = |query: &str| -> Vec<&str> {
            shortcuts
                .iter()
                .filter(|shortcut| shortcut.matches(query))
                .map(|shortcut| shortcut.id.as_ref())
                .collect()
        };

        assert_eq!(
            found("format"),
            ["console.format", "console.format-all"],
            "in the Console card's order"
        );
        assert_eq!(found("FILE.SAVE-AS"), ["file.save-as"]);
        assert!(found("no such action").is_empty());
        let save_keys = format(&keymap.bindings(Action::Save)[0]);
        assert!(found(&save_keys).contains(&"file.save"), "{save_keys}");
    }
}
