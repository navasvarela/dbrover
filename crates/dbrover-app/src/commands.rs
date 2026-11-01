//! What the menus can do. Each menu bar is data ([`egui_menu_bar`], [`macos_menu_bar`]) built
//! from [`AppCommand`]s, and every item runs through `DbRoverApp::run_command`, so the native menu
//! on macOS can't make an item do something other than egui's. See `docs/macos-look.md`.

use std::borrow::Cow;
use std::path::PathBuf;

use dbrover_store::CustomAction;
use egui::{Event, Key, KeyboardShortcut, Modifiers, ThemePreference};

use crate::console::{ClipboardCopy, ClipboardFormat};
use crate::custom_actions::SubjectKind;
use crate::keymap::Keymap;
use crate::{icons, keymap};

/// Something a menu item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    OpenFile,
    OpenRecent(PathBuf),
    ClearRecentFiles,
    Save,
    SaveAs,
    ExportDataSources,
    ImportDataSources,
    Theme(ThemePreference),
    ShowKeymap,
    ShowActions,
    ShowSettings,
    ResetLayout,
    Format,
    FormatAll,
    RunToFile,
    CheckForUpdates,
    About,
    ReportProblem,
    AskQuestion,
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only the macOS menu bar has it")
    )]
    OpenManual,
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only the macOS menu bar has it")
    )]
    /// Closes the window the usual way, which asks first about open transactions and unsaved
    /// files.
    Quit,
    /// A custom action from `actions.toml`, on what is selected.
    RunAction(Box<CustomAction>),
    /// Copies the focused console's selected cells in a format.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only the macOS menu bar has an Edit menu")
    )]
    Copy(ClipboardCopy),
}

/// What decides whether a command can run and which item is checked, gathered once a frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandState {
    /// The dock's focused tab is a query console.
    pub console_focused: bool,
    pub has_data_sources: bool,
    pub theme: ThemePreference,
    /// What custom actions would act on: the focused console's cells, or the explorer's node.
    pub subject: Option<SubjectKind>,
}

impl AppCommand {
    /// A stable name for the command, for native menu items and tests. Commands that are
    /// keymap actions share the action's id, custom actions their `custom.<id>`.
    pub fn id(&self) -> Cow<'static, str> {
        let id = match self {
            Self::RunAction(action) => return Cow::Owned(action.id.keymap_id()),
            Self::Copy(copy) => copy_action(*copy).id(),
            Self::OpenFile => keymap::Action::OpenFile.id(),
            Self::Save => keymap::Action::Save.id(),
            Self::SaveAs => keymap::Action::SaveAs.id(),
            Self::Format => keymap::Action::Format.id(),
            Self::FormatAll => keymap::Action::FormatAll.id(),
            Self::RunToFile => keymap::Action::RunToFile.id(),
            Self::ShowSettings => keymap::Action::Settings.id(),
            Self::OpenRecent(_) => "file.open-recent",
            Self::ClearRecentFiles => "file.clear-recent",
            Self::ExportDataSources => "file.export-data-sources",
            Self::ImportDataSources => "file.import-data-sources",
            Self::Theme(ThemePreference::System) => "view.theme.system",
            Self::Theme(ThemePreference::Light) => "view.theme.light",
            Self::Theme(ThemePreference::Dark) => "view.theme.dark",
            Self::ShowKeymap => "view.keymap",
            Self::ShowActions => "view.actions",
            Self::ResetLayout => "view.reset-layout",
            Self::CheckForUpdates => "help.check-for-updates",
            Self::About => "help.about",
            Self::ReportProblem => "help.report-problem",
            Self::AskQuestion => "help.ask-question",
            Self::OpenManual => "help.manual",
            Self::Quit => "app.quit",
        };
        Cow::Borrowed(id)
    }

    pub fn label(&self) -> Cow<'_, str> {
        let label = match self {
            Self::RunAction(action) => return Cow::Borrowed(&action.name),
            Self::Copy(ClipboardCopy::WithHeaders) => "Copy with headers",
            Self::Copy(ClipboardCopy::As(format)) => format.format().name(),
            Self::OpenRecent(path) => {
                return path.file_name().map_or_else(
                    || Cow::Owned(path.display().to_string()),
                    |name| name.to_string_lossy(),
                );
            }
            Self::OpenFile => "Open…",
            Self::ClearRecentFiles => "Clear recent files",
            Self::Save => "Save",
            Self::SaveAs => "Save as…",
            Self::ExportDataSources => "Export data sources…",
            Self::ImportDataSources => "Import data sources…",
            Self::Theme(ThemePreference::System) => "Match system",
            Self::Theme(ThemePreference::Light) => "Light",
            Self::Theme(ThemePreference::Dark) => "Dark",
            Self::ShowKeymap => "Keymap…",
            Self::ShowActions => "Custom actions…",
            Self::ShowSettings => "Settings…",
            Self::ResetLayout => "Reset layout",
            Self::Format => "Format",
            Self::FormatAll => "Format all",
            Self::RunToFile => "Run to file…",
            Self::CheckForUpdates => "Check for updates…",
            Self::About => "About DbRover",
            Self::ReportProblem => "Report a problem…",
            Self::AskQuestion => "Ask a question…",
            Self::OpenManual => "DbRover manual",
            Self::Quit => "Quit DbRover",
        };
        Cow::Borrowed(label)
    }

    /// The keymap action whose keys run this command too, and which the menu shows.
    pub fn shortcut(&self) -> Option<keymap::Action> {
        match self {
            Self::OpenFile => Some(keymap::Action::OpenFile),
            Self::Save => Some(keymap::Action::Save),
            Self::SaveAs => Some(keymap::Action::SaveAs),
            Self::Format => Some(keymap::Action::Format),
            Self::FormatAll => Some(keymap::Action::FormatAll),
            Self::RunToFile => Some(keymap::Action::RunToFile),
            Self::ShowSettings => Some(keymap::Action::Settings),
            Self::Copy(copy) => Some(copy_action(*copy)),
            _ => None,
        }
    }

    /// The keys that run the command too, which the menu shows: its keymap action's, or for a
    /// custom action what `keymap.toml` gives `custom.<id>`.
    pub fn shortcuts<'k>(&self, keymap: &'k Keymap) -> &'k [KeyboardShortcut] {
        match (self, self.shortcut()) {
            (Self::RunAction(action), _) => keymap.custom_bindings(&action.id),
            (_, Some(action)) => keymap.bindings(action),
            (_, None) => &[],
        }
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only the macOS menu bar has it")
    )]
    /// The key every Mac app gives the command, for those the keymap doesn't bind.
    pub fn system_shortcut(&self) -> Option<KeyboardShortcut> {
        match self {
            Self::Quit => Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Q)),
            _ => None,
        }
    }

    /// The command a keymap action runs when it is one of the menus' commands.
    pub fn for_action(action: keymap::Action) -> Option<Self> {
        match action {
            keymap::Action::OpenFile => Some(Self::OpenFile),
            keymap::Action::Save => Some(Self::Save),
            keymap::Action::SaveAs => Some(Self::SaveAs),
            keymap::Action::Format => Some(Self::Format),
            keymap::Action::FormatAll => Some(Self::FormatAll),
            keymap::Action::RunToFile => Some(Self::RunToFile),
            keymap::Action::Settings => Some(Self::ShowSettings),
            keymap::Action::CopyWithHeaders => Some(Self::Copy(ClipboardCopy::WithHeaders)),
            keymap::Action::CopyAs(format) => Some(Self::Copy(ClipboardCopy::As(format))),
            _ => None,
        }
    }

    pub fn tooltip(&self) -> Option<Cow<'_, str>> {
        if let Some(action) = self.shortcut() {
            return Some(Cow::Borrowed(action.description()));
        }
        match self {
            Self::OpenRecent(path) => Some(Cow::Owned(path.display().to_string())),
            Self::ExportDataSources => Some(Cow::Borrowed(
                "Write data sources and projects to a file, without passwords",
            )),
            Self::ImportDataSources => Some(Cow::Borrowed(
                "Add or update data sources from an exported file",
            )),
            Self::ShowActions => Some(Cow::Borrowed(
                "The custom actions in actions.toml, and what it got wrong",
            )),
            Self::ReportProblem => Some(Cow::Borrowed(
                "Opens a new GitHub issue with this version and system filled in",
            )),
            Self::AskQuestion => Some(Cow::Borrowed("Opens GitHub Discussions")),
            Self::RunAction(action) => {
                let targets: Vec<&str> = action
                    .applies_to
                    .iter()
                    .map(|target| target.name())
                    .collect();
                Some(Cow::Owned(format!("On {}", targets.join(", "))))
            }
            _ => None,
        }
    }

    /// What the command is waiting for while it is disabled.
    pub fn disabled_reason(&self) -> Option<&'static str> {
        match self {
            Self::ExportDataSources => Some("There are no data sources to export yet"),
            Self::Save | Self::SaveAs | Self::Format | Self::FormatAll | Self::RunToFile => {
                Some("Select a query console first")
            }
            Self::RunAction(_) => Some("Select something the action applies to first"),
            Self::Copy(_) => Some("Select cells in a query console's results first"),
            _ => None,
        }
    }

    pub fn enabled(&self, state: &CommandState) -> bool {
        match self {
            Self::Save
            | Self::SaveAs
            | Self::Format
            | Self::FormatAll
            | Self::RunToFile
            | Self::Copy(_) => state.console_focused,
            Self::ExportDataSources => state.has_data_sources,
            Self::RunAction(action) => state.subject.is_some_and(|subject| subject.offers(action)),
            _ => true,
        }
    }

    /// Whether the item shows as picked, for commands that are one of a set of choices.
    pub fn checked(&self, state: &CommandState) -> Option<bool> {
        match self {
            Self::Theme(preference) => Some(*preference == state.theme),
            _ => None,
        }
    }

    pub fn icon(&self) -> Option<&'static str> {
        match self {
            Self::Theme(ThemePreference::System) => Some(icons::MONITOR),
            Self::Theme(ThemePreference::Light) => Some(icons::SUN),
            Self::Theme(ThemePreference::Dark) => Some(icons::MOON),
            _ => None,
        }
    }
}

/// The Edit menu's commands, for the focused egui widget. They become the input the keys would
/// have produced, so text fields, the editor and the results grid handle them as they handle
/// the keys.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS menu bar has it")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditCommand {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS menu bar has it")
)]
impl EditCommand {
    pub fn label(self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::Cut => "Cut",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::SelectAll => "Select All",
        }
    }

    pub fn shortcut(self) -> KeyboardShortcut {
        let (modifiers, key) = match self {
            Self::Undo => (Modifiers::COMMAND, Key::Z),
            Self::Redo => (Modifiers::COMMAND | Modifiers::SHIFT, Key::Z),
            Self::Cut => (Modifiers::COMMAND, Key::X),
            Self::Copy => (Modifiers::COMMAND, Key::C),
            Self::Paste => (Modifiers::COMMAND, Key::V),
            Self::SelectAll => (Modifiers::COMMAND, Key::A),
        };
        KeyboardShortcut::new(modifiers, key)
    }

    /// What egui would have received for the key, as egui-winit translates it: clipboard
    /// events for cut, copy and paste (with `pasted`, the clipboard's text), a key press and
    /// release for the rest.
    pub fn events(self, pasted: Option<String>) -> Vec<Event> {
        match self {
            Self::Cut => vec![Event::Cut],
            Self::Copy => vec![Event::Copy],
            Self::Paste => pasted.into_iter().map(Event::Paste).collect(),
            Self::Undo | Self::Redo | Self::SelectAll => {
                let shortcut = self.shortcut();
                [true, false]
                    .into_iter()
                    .map(|pressed| Event::Key {
                        key: shortcut.logical_key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: shortcut.modifiers,
                    })
                    .collect()
            }
        }
    }
}

/// Items AppKit provides and runs itself, in the macOS menu bar only.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS menu bar has it")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemItem {
    Services,
    Hide,
    HideOthers,
    ShowAll,
    FullScreen,
    Minimize,
    Zoom,
    BringAllToFront,
}

/// The titles AppKit gives the Window and Help menus' extra items to (the window list, the
/// help search field).
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS menu bar has it")
)]
pub const WINDOW_MENU: &str = "Window";
pub const HELP_MENU: &str = "Help";

/// The keymap action whose keys copy this way.
fn copy_action(copy: ClipboardCopy) -> keymap::Action {
    match copy {
        ClipboardCopy::WithHeaders => keymap::Action::CopyWithHeaders,
        ClipboardCopy::As(format) => keymap::Action::CopyAs(format),
    }
}

/// Edit › Copy as: each format the clipboard takes.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS menu bar has an Edit menu")
)]
fn copy_as() -> Menu {
    Menu {
        title: "Copy as",
        entries: ClipboardFormat::ALL
            .into_iter()
            .map(|format| Entry::Command(AppCommand::Copy(ClipboardCopy::As(format))))
            .collect(),
    }
}

/// A menu or submenu: its title and what it lists, top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub title: &'static str,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Command(AppCommand),
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only the macOS menu bar has it")
    )]
    /// Only in the macOS menu bar: elsewhere egui's widgets take the keys directly.
    Edit(EditCommand),
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only the macOS menu bar has it")
    )]
    /// Only in the macOS menu bar.
    System(SystemItem),
    Separator,
    Submenu(Menu),
    /// Greyed-out text standing in for an empty list.
    Note(&'static str),
}

/// The menu bar drawn by egui: on Windows and Linux, and wherever there is no native one.
/// `recent_files` are the files for File › Open recent, most recent first; `actions` are
/// `actions.toml`'s.
pub fn egui_menu_bar(
    recent_files: impl IntoIterator<Item = PathBuf>,
    actions: &[CustomAction],
) -> Vec<Menu> {
    use AppCommand as C;
    use Entry::{Command, Separator, Submenu};
    vec![
        Menu {
            title: "File",
            entries: vec![
                Command(C::OpenFile),
                Submenu(open_recent(recent_files)),
                Command(C::Save),
                Command(C::SaveAs),
                Separator,
                Command(C::ExportDataSources),
                Command(C::ImportDataSources),
            ],
        },
        Menu {
            title: "View",
            entries: vec![
                Submenu(theme()),
                Command(C::ShowKeymap),
                Command(C::ShowActions),
                Command(C::ShowSettings),
                Command(C::ResetLayout),
            ],
        },
        code(),
        actions_menu(actions),
        Menu {
            title: HELP_MENU,
            entries: vec![
                Command(C::ReportProblem),
                Command(C::AskQuestion),
                Separator,
                Command(C::CheckForUpdates),
                Command(C::About),
            ],
        },
    ]
}

/// The same commands, laid out as Mac apps lay out theirs: About, updates, Settings and Quit in
/// the app menu, and the Edit and Window menus macOS users expect.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS menu bar has it")
)]
pub fn macos_menu_bar(
    recent_files: impl IntoIterator<Item = PathBuf>,
    actions: &[CustomAction],
) -> Vec<Menu> {
    use AppCommand as C;
    use Entry::{Command, Edit, Separator, Submenu, System};
    vec![
        // The first menu is the app menu, which macOS titles with the app's name.
        Menu {
            title: "DbRover",
            entries: vec![
                Command(C::About),
                Command(C::CheckForUpdates),
                Separator,
                Command(C::ShowSettings),
                Separator,
                System(SystemItem::Services),
                Separator,
                System(SystemItem::Hide),
                System(SystemItem::HideOthers),
                System(SystemItem::ShowAll),
                Separator,
                Command(C::Quit),
            ],
        },
        Menu {
            title: "File",
            entries: vec![
                Command(C::OpenFile),
                Submenu(open_recent(recent_files)),
                Separator,
                Command(C::Save),
                Command(C::SaveAs),
                Separator,
                Command(C::ExportDataSources),
                Command(C::ImportDataSources),
            ],
        },
        Menu {
            title: "Edit",
            entries: vec![
                Edit(EditCommand::Undo),
                Edit(EditCommand::Redo),
                Separator,
                Edit(EditCommand::Cut),
                Edit(EditCommand::Copy),
                Command(C::Copy(ClipboardCopy::WithHeaders)),
                Submenu(copy_as()),
                Edit(EditCommand::Paste),
                Edit(EditCommand::SelectAll),
            ],
        },
        Menu {
            title: "View",
            entries: vec![
                Submenu(theme()),
                Command(C::ShowKeymap),
                Command(C::ShowActions),
                Command(C::ResetLayout),
                Separator,
                System(SystemItem::FullScreen),
            ],
        },
        code(),
        actions_menu(actions),
        Menu {
            title: WINDOW_MENU,
            entries: vec![
                System(SystemItem::Minimize),
                System(SystemItem::Zoom),
                Separator,
                System(SystemItem::BringAllToFront),
            ],
        },
        Menu {
            title: HELP_MENU,
            entries: vec![
                Command(C::OpenManual),
                Separator,
                Command(C::ReportProblem),
                Command(C::AskQuestion),
            ],
        },
    ]
}

fn open_recent(files: impl IntoIterator<Item = PathBuf>) -> Menu {
    let mut entries: Vec<Entry> = files
        .into_iter()
        .map(|path| Entry::Command(AppCommand::OpenRecent(path)))
        .collect();
    if entries.is_empty() {
        entries.push(Entry::Note("No recent files"));
    } else {
        entries.push(Entry::Separator);
        entries.push(Entry::Command(AppCommand::ClearRecentFiles));
    }
    Menu {
        title: "Open recent",
        entries,
    }
}

fn theme() -> Menu {
    Menu {
        title: "Theme",
        entries: [
            ThemePreference::System,
            ThemePreference::Light,
            ThemePreference::Dark,
        ]
        .into_iter()
        .map(|preference| Entry::Command(AppCommand::Theme(preference)))
        .collect(),
    }
}

/// Formatting, for the console whose tab has focus.
fn code() -> Menu {
    Menu {
        title: "Code",
        entries: vec![
            Entry::Command(AppCommand::Format),
            Entry::Command(AppCommand::FormatAll),
            Entry::Separator,
            Entry::Command(AppCommand::RunToFile),
        ],
    }
}

/// The custom actions, for what is selected.
fn actions_menu(actions: &[CustomAction]) -> Menu {
    let mut entries: Vec<Entry> = actions
        .iter()
        .map(|action| Entry::Command(AppCommand::RunAction(Box::new(action.clone()))))
        .collect();
    if entries.is_empty() {
        entries.push(Entry::Note("No actions in actions.toml"));
    }
    Menu {
        title: "Actions",
        entries,
    }
}

/// Every command in `menus` and their submenus, in menu order.
#[cfg(test)]
pub fn commands_in(menus: &[Menu]) -> Vec<&AppCommand> {
    fn walk<'a>(entries: &'a [Entry], found: &mut Vec<&'a AppCommand>) {
        for entry in entries {
            match entry {
                Entry::Command(command) => found.push(command),
                Entry::Submenu(menu) => walk(&menu.entries, found),
                Entry::Edit(_) | Entry::System(_) | Entry::Separator | Entry::Note(_) => {}
            }
        }
    }
    let mut found = Vec::new();
    for menu in menus {
        walk(&menu.entries, &mut found);
    }
    found
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const NOTHING_OPEN: CommandState = CommandState {
        console_focused: false,
        has_data_sources: false,
        theme: ThemePreference::System,
        subject: None,
    };

    fn menu<'a>(menus: &'a [Menu], title: &str) -> &'a Menu {
        menus
            .iter()
            .find(|menu| menu.title == title)
            .unwrap_or_else(|| panic!("no {title} menu"))
    }

    #[test]
    fn ids_are_unique() {
        for menus in [
            egui_menu_bar([PathBuf::from("/tmp/a.sql")], &[]),
            macos_menu_bar([PathBuf::from("/tmp/a.sql")], &[]),
        ] {
            let mut seen = HashSet::new();
            for command in commands_in(&menus) {
                assert!(seen.insert(command.id()), "{} is used twice", command.id());
            }
        }
    }

    #[test]
    fn the_macos_menu_bar_has_every_command_of_egui_s() {
        let recent = || [PathBuf::from("/tmp/a.sql")];
        let macos_menus = macos_menu_bar(recent(), &[]);
        let macos = commands_in(&macos_menus);
        for command in commands_in(&egui_menu_bar(recent(), &[])) {
            assert!(macos.contains(&command), "{command:?} is missing on macOS");
        }
    }

    #[test]
    fn edit_commands_become_what_their_keys_would_have() {
        let press = |modifiers, key| {
            [true, false].map(|pressed| Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            })
        };
        assert_eq!(EditCommand::Copy.events(None), [Event::Copy]);
        assert_eq!(EditCommand::Cut.events(None), [Event::Cut]);
        assert_eq!(
            EditCommand::Paste.events(Some("id".to_owned())),
            [Event::Paste("id".to_owned())]
        );
        assert_eq!(EditCommand::Paste.events(None), []);
        assert_eq!(
            EditCommand::Undo.events(None),
            press(Modifiers::COMMAND, Key::Z)
        );
        assert_eq!(
            EditCommand::Redo.events(None),
            press(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)
        );
        assert_eq!(
            EditCommand::SelectAll.events(None),
            press(Modifiers::COMMAND, Key::A)
        );
    }

    #[test]
    fn settings_and_quit_have_the_mac_keys() {
        assert_eq!(
            AppCommand::ShowSettings.shortcut(),
            Some(keymap::Action::Settings)
        );
        assert_eq!(
            AppCommand::Quit.system_shortcut(),
            Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Q))
        );
    }

    #[test]
    fn the_menu_bar_has_the_menus_in_order() {
        let titles: Vec<_> = egui_menu_bar([], &[])
            .iter()
            .map(|menu| menu.title)
            .collect();
        assert_eq!(titles, ["File", "View", "Code", "Actions", "Help"]);
    }

    #[test]
    fn open_recent_lists_the_files_then_clear() {
        let menus = egui_menu_bar([PathBuf::from("/work/a.sql"), PathBuf::from("b.sql")], &[]);
        let Some(Entry::Submenu(recent)) = menu(&menus, "File").entries.get(1) else {
            panic!("File's second entry is not Open recent");
        };
        assert_eq!(recent.title, "Open recent");
        let labels: Vec<_> = commands_in(std::slice::from_ref(recent))
            .iter()
            .map(|command| command.label().into_owned())
            .collect();
        assert_eq!(labels, ["a.sql", "b.sql", "Clear recent files"]);
        assert_eq!(
            AppCommand::OpenRecent(PathBuf::from("/work/a.sql"))
                .tooltip()
                .as_deref(),
            Some(PathBuf::from("/work/a.sql").display().to_string().as_str())
        );
    }

    #[test]
    fn open_recent_says_when_there_are_none() {
        let menus = egui_menu_bar([], &[]);
        let Some(Entry::Submenu(recent)) = menu(&menus, "File").entries.get(1) else {
            panic!("File's second entry is not Open recent");
        };
        assert_eq!(recent.entries, [Entry::Note("No recent files")]);
    }

    #[test]
    fn console_commands_wait_for_a_focused_console() {
        let console = CommandState {
            console_focused: true,
            ..NOTHING_OPEN
        };
        for command in [
            AppCommand::Save,
            AppCommand::SaveAs,
            AppCommand::Format,
            AppCommand::FormatAll,
        ] {
            assert!(!command.enabled(&NOTHING_OPEN), "{command:?}");
            assert!(command.enabled(&console), "{command:?}");
            assert_eq!(
                command.disabled_reason(),
                Some("Select a query console first")
            );
        }
        assert!(AppCommand::OpenFile.enabled(&NOTHING_OPEN));
    }

    #[test]
    fn exporting_waits_for_a_data_source() {
        let export = AppCommand::ExportDataSources;
        assert!(!export.enabled(&NOTHING_OPEN));
        assert!(export.enabled(&CommandState {
            has_data_sources: true,
            ..NOTHING_OPEN
        }));
        assert_eq!(
            export.disabled_reason(),
            Some("There are no data sources to export yet")
        );
    }

    #[test]
    fn the_picked_theme_is_checked() {
        let dark = CommandState {
            theme: ThemePreference::Dark,
            ..NOTHING_OPEN
        };
        let checked: Vec<_> = [
            ThemePreference::System,
            ThemePreference::Light,
            ThemePreference::Dark,
        ]
        .into_iter()
        .map(|preference| AppCommand::Theme(preference).checked(&dark))
        .collect();
        assert_eq!(checked, [Some(false), Some(false), Some(true)]);
        assert_eq!(AppCommand::About.checked(&dark), None);
    }

    #[test]
    fn custom_actions_wait_for_what_they_apply_to() {
        use dbrover_core::{DriverKind, NodeKind};
        use dbrover_store::{ActionTarget, parse_actions};

        use crate::custom_actions::SubjectOn;

        let actions = parse_actions(
            "[[action]]\nid = \"row-count\"\nname = \"Row count\"\napplies_to = [\"table\"]\n\
             sql = \"SELECT count(*) FROM {qualified_name}\"\n",
        )
        .value;
        let menus = egui_menu_bar([], &actions);
        let [Entry::Command(command)] = menu(&menus, "Actions").entries.as_slice() else {
            panic!("the Actions menu doesn't hold the one action");
        };
        let on = |target| CommandState {
            subject: Some(SubjectKind {
                driver: DriverKind::Sqlite,
                on: SubjectOn::Node(target),
            }),
            ..NOTHING_OPEN
        };

        assert_eq!(command.id(), "custom.row-count");
        assert_eq!(command.label(), "Row count");
        assert!(command.enabled(&on(ActionTarget::Node(NodeKind::Table))));
        assert!(!command.enabled(&on(ActionTarget::Node(NodeKind::Column))));
        assert!(!command.enabled(&NOTHING_OPEN));
        let id = actions[0].id.clone();
        let keymap = Keymap::from_file(
            &[("custom.row-count".to_owned(), vec!["f6".to_owned()])].into(),
            &[&id],
        );
        assert_eq!(
            command.shortcuts(&keymap),
            [KeyboardShortcut::new(Modifiers::NONE, Key::F6)]
        );
        assert_eq!(
            menu(&egui_menu_bar([], &[]), "Actions").entries,
            [Entry::Note("No actions in actions.toml")]
        );
    }

    #[test]
    fn keymap_commands_show_and_answer_to_their_action() {
        for command in commands_in(&egui_menu_bar([], &[])) {
            if let Some(action) = command.shortcut() {
                assert_eq!(AppCommand::for_action(action).as_ref(), Some(command));
                assert_eq!(command.id(), action.id());
                assert_eq!(command.tooltip().as_deref(), Some(action.description()));
            }
        }
        assert_eq!(AppCommand::for_action(keymap::Action::RunCurrent), None);
    }
}
