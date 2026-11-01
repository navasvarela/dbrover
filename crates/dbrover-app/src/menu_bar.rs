//! Draws [`commands::Menu`]s as egui's menu bar.

use egui::{Button, Color32, Ui, WidgetText, WidgetType};

use crate::commands::{AppCommand, CommandState, Entry, Menu};
use crate::keymap::Keymap;
use crate::{theme, widgets};

/// Draws `menus` and returns the command picked this frame, if any.
pub fn show(
    ui: &mut Ui,
    menus: &[Menu],
    state: &CommandState,
    keymap: &Keymap,
) -> Option<AppCommand> {
    let mut picked = None;
    ui.scope(|ui| {
        // `MenuBar` lays out the row it draws in in the style it is given, and applies its own
        // style only inside, so the bar's height has to be the row's too.
        theme::menu_bar_style(ui.style_mut());
        egui::MenuBar::new().ui(ui, |ui| {
            for menu in menus {
                ui.menu_button(menu.title, |ui| {
                    entries(ui, &menu.entries, state, keymap, &mut picked);
                });
            }
        });
    });
    picked
}

fn entries(
    ui: &mut Ui,
    entries: &[Entry],
    state: &CommandState,
    keymap: &Keymap,
    picked: &mut Option<AppCommand>,
) {
    for entry in entries {
        match entry {
            Entry::Command(command) => {
                if item(ui, command, state, keymap) {
                    *picked = Some(command.clone());
                }
            }
            Entry::Separator => {
                ui.separator();
            }
            Entry::Submenu(menu) => {
                ui.menu_button(menu.title, |ui| {
                    self::entries(ui, &menu.entries, state, keymap, picked);
                });
            }
            Entry::Note(text) => {
                ui.weak(*text);
            }
            // The Edit and system items are the macOS menu bar's; egui widgets take those keys
            // directly.
            Entry::Edit(_) | Entry::System(_) => {}
        }
    }
}

/// Draws one command and says whether it was clicked.
fn item(ui: &mut Ui, command: &AppCommand, state: &CommandState, keymap: &Keymap) -> bool {
    if let Some(checked) = command.checked(state) {
        return choice(ui, command, checked);
    }
    let mut button = Button::new(command.label());
    if let Some(shortcut) = command.shortcuts(keymap).first() {
        button = button.shortcut_text(ui.ctx().format_shortcut(shortcut));
    }
    let mut response = ui.add_enabled(command.enabled(state), button);
    if let Some(tooltip) = command.tooltip() {
        response = response.on_hover_text(tooltip);
    }
    if let Some(reason) = command.disabled_reason() {
        response = response.on_disabled_hover_text(reason);
    }
    response.clicked()
}

/// One of a set of choices, such as a theme: marked when picked, and announced as a radio
/// button under its plain label rather than the icon's private-use character.
fn choice(ui: &mut Ui, command: &AppCommand, checked: bool) -> bool {
    let label = command.label();
    let text: WidgetText = match command.icon() {
        Some(glyph) => widgets::icon_text(ui, glyph, Color32::PLACEHOLDER, &label).into(),
        None => label.as_ref().into(),
    };
    let response = widgets::name(
        ui.selectable_label(checked, text),
        WidgetType::RadioButton,
        &label,
        true,
    );
    // Unlike buttons, selectable labels don't close the menu they are in.
    if response.clicked() {
        ui.close();
    }
    response.clicked()
}
