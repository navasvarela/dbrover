//! The macOS menu bar, built with muda from [`commands::macos_menu_bar`]. See
//! `docs/macos-look.md` (D5b).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use egui::KeyboardShortcut;
use muda::accelerator::{Key, KeyAccelerator, Modifiers, NamedKey};
use muda::{CheckMenuItem, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

use dbrover_export::CustomFormat;
use dbrover_store::CustomAction;

use super::pasteboard;
use crate::commands::{self, AppCommand, CommandState, EditCommand, Entry, Menu, SystemItem};
use crate::keymap::Keymap;
use crate::native_menu::{Chosen, CustomEntries};

pub struct NativeMenu {
    /// What the menu was last built from; a change rebuilds it.
    recent_files: Vec<PathBuf>,
    keymap: Keymap,
    actions: Vec<CustomAction>,
    formats: Vec<CustomFormat>,
    built: Built,
    /// Ids of the items picked since the last frame, from muda's event handler.
    picked: Receiver<MenuId>,
}

impl NativeMenu {
    /// Replaces winit's default menu bar with DbRover's. `None`, with the reason logged, if muda
    /// couldn't build it; the app then keeps drawing egui's.
    pub fn install(ctx: &egui::Context) -> Option<Self> {
        let (sender, picked) = mpsc::channel();
        let repaint = ctx.clone();
        // Called on the main thread while the app may be idle, so it wakes egui for the frame
        // that runs the command.
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            // The receiver only goes away with the app.
            let _ = sender.send(event.id);
            repaint.request_repaint();
        }));
        let keymap = Keymap::default();
        let none = CustomEntries {
            actions: &[],
            formats: &[],
        };
        let built = Built::new(&[], &keymap, none)
            .inspect_err(|error| tracing::warn!("couldn't build the macOS menu bar: {error}"))
            .ok()?;
        Some(Self {
            recent_files: Vec::new(),
            keymap,
            actions: Vec::new(),
            formats: Vec::new(),
            built,
            picked,
        })
    }

    /// What was picked since the last call, in order.
    pub fn drain(&mut self) -> Vec<Chosen> {
        self.picked
            .try_iter()
            .filter_map(|id| self.built.chosen.get(&id).cloned())
            .map(|item| match item {
                Action::Command(command) => Chosen::Command(command),
                Action::Edit(edit) => Chosen::Input(edit.events(pasted_text(edit))),
            })
            .collect()
    }

    /// Brings enabled and checked marks up to date, and rebuilds the menu when the recent files,
    /// the keymap, the custom actions or the custom formats changed, which happens rarely
    /// enough not to track item by item.
    pub fn sync(
        &mut self,
        state: &CommandState,
        keymap: &Keymap,
        recent_files: &[PathBuf],
        custom: CustomEntries<'_>,
    ) {
        let changed = self.recent_files != recent_files
            || self.keymap != *keymap
            || self.actions != custom.actions
            || self.formats != custom.formats;
        if changed {
            match Built::new(recent_files, keymap, custom) {
                Ok(built) => self.built = built,
                Err(error) => tracing::warn!("couldn't rebuild the macOS menu bar: {error}"),
            }
            recent_files.clone_into(&mut self.recent_files);
            self.keymap = keymap.clone();
            custom.actions.clone_into(&mut self.actions);
            custom.formats.clone_into(&mut self.formats);
        }
        for item in &mut self.built.items {
            item.sync(state);
        }
    }
}

fn pasted_text(edit: EditCommand) -> Option<String> {
    (edit == EditCommand::Paste)
        .then(pasteboard::text)
        .flatten()
}

/// What an item does, by its muda id.
#[derive(Debug, Clone)]
enum Action {
    Command(AppCommand),
    Edit(EditCommand),
}

/// One build of the menu bar, installed in `NSApp` while it lives.
struct Built {
    /// Kept so the items stay alive: NSApp's menu only holds the Objective-C side.
    _menu: muda::Menu,
    items: Vec<CommandItem>,
    chosen: HashMap<MenuId, Action>,
}

impl Built {
    fn new(
        recent_files: &[PathBuf],
        keymap: &Keymap,
        custom: CustomEntries<'_>,
    ) -> muda::Result<Self> {
        let mut built = Self {
            _menu: muda::Menu::new(),
            items: Vec::new(),
            chosen: HashMap::new(),
        };
        let mut submenus = Vec::new();
        let menus =
            commands::macos_menu_bar(recent_files.iter().cloned(), custom.actions, custom.formats);
        for menu in menus {
            let submenu = built.submenu(&menu, keymap)?;
            built._menu.append(&submenu)?;
            submenus.push((menu.title, submenu));
        }
        built._menu.init_for_nsapp();
        // After `init_for_nsapp`: AppKit only takes these roles from menus in the menu bar.
        for (title, submenu) in &submenus {
            match *title {
                commands::WINDOW_MENU => submenu.set_as_windows_menu_for_nsapp(),
                commands::HELP_MENU => submenu.set_as_help_menu_for_nsapp(),
                _ => {}
            }
        }
        Ok(built)
    }

    fn submenu(&mut self, menu: &Menu, keymap: &Keymap) -> muda::Result<Submenu> {
        let submenu = Submenu::new(menu.title, true);
        for entry in &menu.entries {
            match entry {
                Entry::Command(command) => {
                    let item = CommandItem::new(command.clone(), self.items.len(), keymap)?;
                    item.append_to(&submenu)?;
                    self.chosen
                        .insert(item.id().clone(), Action::Command(command.clone()));
                    self.items.push(item);
                }
                Entry::Edit(edit) => {
                    let item = MenuItem::new(edit.label(), true, None);
                    item.set_key_accelerator(key_accelerator(&edit.shortcut()))?;
                    submenu.append(&item)?;
                    self.chosen.insert(item.id().clone(), Action::Edit(*edit));
                }
                Entry::System(system) => submenu.append(&predefined(*system))?,
                Entry::Separator => submenu.append(&PredefinedMenuItem::separator())?,
                Entry::Submenu(inner) => submenu.append(&self.submenu(inner, keymap)?)?,
                Entry::Note(text) => submenu.append(&MenuItem::new(*text, false, None))?,
            }
        }
        Ok(submenu)
    }
}

/// AppKit renames a full-screen item to "Exit Full Screen" and back as the window changes, from
/// the title Mac apps start with; muda's own is "Toggle Full Screen".
fn predefined(item: SystemItem) -> PredefinedMenuItem {
    match item {
        SystemItem::Services => PredefinedMenuItem::services(None),
        SystemItem::Hide => PredefinedMenuItem::hide(None),
        SystemItem::HideOthers => PredefinedMenuItem::hide_others(None),
        SystemItem::ShowAll => PredefinedMenuItem::show_all(None),
        SystemItem::FullScreen => PredefinedMenuItem::fullscreen(Some("Enter Full Screen")),
        SystemItem::Minimize => PredefinedMenuItem::minimize(None),
        SystemItem::Zoom => PredefinedMenuItem::maximize(None),
        SystemItem::BringAllToFront => PredefinedMenuItem::bring_all_to_front(None),
    }
}

/// A command's item, and the marks last given to it, so only changes reach AppKit.
struct CommandItem {
    command: AppCommand,
    kind: ItemKind,
    enabled: bool,
    checked: Option<bool>,
}

enum ItemKind {
    Plain(MenuItem),
    /// One of a set of choices, such as a theme.
    Check(CheckMenuItem),
}

impl CommandItem {
    /// `index` makes the id unique: Open recent's items share their command's id.
    fn new(command: AppCommand, index: usize, keymap: &Keymap) -> muda::Result<Self> {
        let id = format!("{}#{index}", command.id());
        let accelerator = command
            .shortcuts(keymap)
            .first()
            .copied()
            .or_else(|| command.system_shortcut())
            .and_then(|shortcut| key_accelerator(&shortcut));
        let label = command.label();
        let kind = match command.checked(&CommandState::default()) {
            Some(_) => {
                let item = CheckMenuItem::with_id(id, label, true, false, None);
                item.set_key_accelerator(accelerator)?;
                ItemKind::Check(item)
            }
            None => {
                let item = MenuItem::with_id(id, label, true, None);
                item.set_key_accelerator(accelerator)?;
                ItemKind::Plain(item)
            }
        };
        Ok(Self {
            command,
            kind,
            enabled: true,
            checked: None,
        })
    }

    fn id(&self) -> &MenuId {
        match &self.kind {
            ItemKind::Plain(item) => item.id(),
            ItemKind::Check(item) => item.id(),
        }
    }

    fn append_to(&self, submenu: &Submenu) -> muda::Result<()> {
        match &self.kind {
            ItemKind::Plain(item) => submenu.append(item),
            ItemKind::Check(item) => submenu.append(item),
        }
    }

    fn sync(&mut self, state: &CommandState) {
        let enabled = self.command.enabled(state);
        if enabled != self.enabled {
            self.enabled = enabled;
            match &self.kind {
                ItemKind::Plain(item) => item.set_enabled(enabled),
                ItemKind::Check(item) => item.set_enabled(enabled),
            }
        }
        let checked = self.command.checked(state);
        if checked != self.checked {
            self.checked = checked;
            if let (ItemKind::Check(item), Some(checked)) = (&self.kind, checked) {
                item.set_checked(checked);
            }
        }
    }
}

/// The menu key equivalent for an egui shortcut, or `None` for a key a menu can't show. Such a
/// shortcut stays off the item and keeps working through egui, which then still sees the key.
fn key_accelerator(shortcut: &KeyboardShortcut) -> Option<KeyAccelerator> {
    let key = menu_key(shortcut.logical_key)?;
    let pressed = shortcut.modifiers;
    let mut modifiers = Modifiers::empty();
    for (held, modifier) in [
        (pressed.command || pressed.mac_cmd, Modifiers::META),
        (pressed.ctrl, Modifiers::CONTROL),
        (pressed.alt, Modifiers::ALT),
        (pressed.shift, Modifiers::SHIFT),
    ] {
        if held {
            modifiers |= modifier;
        }
    }
    Some(KeyAccelerator::new(modifiers, key))
}

fn menu_key(key: egui::Key) -> Option<Key> {
    use egui::Key as K;
    let named = match key {
        K::Enter => NamedKey::Enter,
        K::Tab => NamedKey::Tab,
        K::Escape => NamedKey::Escape,
        K::Backspace => NamedKey::Backspace,
        K::Delete => NamedKey::Delete,
        K::Insert => NamedKey::Insert,
        K::Home => NamedKey::Home,
        K::End => NamedKey::End,
        K::PageUp => NamedKey::PageUp,
        K::PageDown => NamedKey::PageDown,
        K::ArrowUp => NamedKey::ArrowUp,
        K::ArrowDown => NamedKey::ArrowDown,
        K::ArrowLeft => NamedKey::ArrowLeft,
        K::ArrowRight => NamedKey::ArrowRight,
        K::F1 => NamedKey::F1,
        K::F2 => NamedKey::F2,
        K::F3 => NamedKey::F3,
        K::F4 => NamedKey::F4,
        K::F5 => NamedKey::F5,
        K::F6 => NamedKey::F6,
        K::F7 => NamedKey::F7,
        K::F8 => NamedKey::F8,
        K::F9 => NamedKey::F9,
        K::F10 => NamedKey::F10,
        K::F11 => NamedKey::F11,
        K::F12 => NamedKey::F12,
        K::Space => return Some(Key::Character(" ".to_owned())),
        // Letters, digits and punctuation: a menu's key equivalent is the character itself.
        _ => {
            let mut symbol = key.symbol_or_name().chars();
            return match (symbol.next(), symbol.next()) {
                (Some(character), None) => Some(Key::Character(character.to_lowercase().collect())),
                _ => None,
            };
        }
    };
    Some(Key::Named(named))
}

#[cfg(test)]
mod tests {
    use egui::{Key as K, Modifiers as M};

    use super::*;

    #[test]
    fn shortcuts_become_key_equivalents() {
        for (shortcut, modifiers, key) in [
            (
                KeyboardShortcut::new(M::COMMAND, K::S),
                Modifiers::META,
                Key::Character("s".to_owned()),
            ),
            (
                KeyboardShortcut::new(M::COMMAND | M::SHIFT, K::S),
                Modifiers::META | Modifiers::SHIFT,
                Key::Character("s".to_owned()),
            ),
            (
                KeyboardShortcut::new(M::COMMAND | M::ALT, K::L),
                Modifiers::META | Modifiers::ALT,
                Key::Character("l".to_owned()),
            ),
            (
                KeyboardShortcut::new(M::COMMAND, K::Comma),
                Modifiers::META,
                Key::Character(",".to_owned()),
            ),
            (
                KeyboardShortcut::new(M::CTRL, K::Space),
                Modifiers::CONTROL,
                Key::Character(" ".to_owned()),
            ),
            (
                KeyboardShortcut::new(M::COMMAND, K::Enter),
                Modifiers::META,
                Key::Named(NamedKey::Enter),
            ),
            (
                KeyboardShortcut::new(M::SHIFT, K::F8),
                Modifiers::SHIFT,
                Key::Named(NamedKey::F8),
            ),
        ] {
            assert_eq!(
                key_accelerator(&shortcut),
                Some(KeyAccelerator::new(modifiers, key)),
                "{shortcut:?}"
            );
        }
    }

    #[test]
    fn keys_a_menu_cant_show_stay_with_egui() {
        assert_eq!(
            key_accelerator(&KeyboardShortcut::new(M::COMMAND, K::F20)),
            None
        );
    }
}
