//! Key bindings: every action a key can trigger has a stable id, and `keymap.toml` can rebind
//! it. See `docs/features/keymap.md`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use dbrover_store::{ActionId, KeymapBindings};
use egui::{InputState, Key, KeyboardShortcut, Modifiers};

use crate::console::ClipboardFormat;

/// Something a key can do. The ids are what `keymap.toml` names, so they never change once
/// released; descriptions can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    Complete,
    RunCurrent,
    RunAll,
    Explain,
    /// Explain the statement at the cursor by running it: `EXPLAIN ANALYZE`.
    ExplainAnalyse,
    Cancel,
    ShowHistory,
    Commit,
    Rollback,
    Format,
    FormatAll,
    NextProblem,
    PreviousProblem,
    ViewCell,
    EditCell,
    OpenFile,
    Save,
    SaveAs,
    Find,
    Replace,
    FindNext,
    FindPrevious,
    Settings,
    /// Run the statement at the cursor straight into a file.
    RunToFile,
    /// Copy the selected cells as TSV with a header line.
    CopyWithHeaders,
    /// Copy the selected cells in a format, as **Copy as ›** does.
    CopyAs(ClipboardFormat),
}

impl Action {
    pub const ALL: [Self; 33] = [
        Self::Complete,
        Self::RunCurrent,
        Self::RunAll,
        Self::Explain,
        Self::ExplainAnalyse,
        Self::Cancel,
        Self::ShowHistory,
        Self::Commit,
        Self::Rollback,
        Self::Format,
        Self::FormatAll,
        Self::NextProblem,
        Self::PreviousProblem,
        Self::ViewCell,
        Self::EditCell,
        Self::OpenFile,
        Self::Save,
        Self::SaveAs,
        Self::Find,
        Self::Replace,
        Self::FindNext,
        Self::FindPrevious,
        Self::Settings,
        Self::RunToFile,
        Self::CopyWithHeaders,
        Self::CopyAs(ClipboardFormat::Csv),
        Self::CopyAs(ClipboardFormat::Tsv),
        Self::CopyAs(ClipboardFormat::Markdown),
        Self::CopyAs(ClipboardFormat::SqlInsert),
        Self::CopyAs(ClipboardFormat::Json),
        Self::CopyAs(ClipboardFormat::JsonLines),
        Self::CopyAs(ClipboardFormat::Html),
        Self::CopyAs(ClipboardFormat::SqlUpdate),
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Complete => "console.complete",
            Self::RunCurrent => "console.run",
            Self::RunAll => "console.run-all",
            Self::Explain => "console.explain",
            Self::ExplainAnalyse => "console.explain-analyse",
            Self::Cancel => "console.cancel",
            Self::ShowHistory => "console.history",
            Self::Commit => "console.commit",
            Self::Rollback => "console.rollback",
            Self::Format => "console.format",
            Self::FormatAll => "console.format-all",
            Self::NextProblem => "console.next-problem",
            Self::PreviousProblem => "console.previous-problem",
            Self::ViewCell => "grid.view-cell",
            Self::EditCell => "grid.edit-cell",
            Self::OpenFile => "file.open",
            Self::Save => "file.save",
            Self::SaveAs => "file.save-as",
            Self::Find => "editor.find",
            Self::Replace => "editor.replace",
            Self::FindNext => "editor.find-next",
            Self::FindPrevious => "editor.find-previous",
            Self::Settings => "app.settings",
            Self::RunToFile => "console.run-to-file",
            Self::CopyWithHeaders => "grid.copy-with-headers",
            Self::CopyAs(format) => format.keymap_id(),
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Complete => "Suggest names and keywords at the cursor",
            Self::RunCurrent => "Run the selection or the statement at the cursor",
            Self::RunAll => "Run every statement in the console",
            Self::Explain => "Explain the statement at the cursor",
            Self::ExplainAnalyse => {
                "Run the statement at the cursor and explain what each step did"
            }
            Self::Cancel => "Stop the running statement or discard unread rows",
            Self::ShowHistory => "Open the data source's query history",
            Self::Commit => "Commit the console's transaction",
            Self::Rollback => "Roll back the console's transaction",
            Self::Format => "Format the selection, or the statement at the cursor",
            Self::FormatAll => "Format every statement in the console",
            Self::NextProblem => "Go to the next syntax error or warning",
            Self::PreviousProblem => "Go to the previous syntax error or warning",
            Self::ViewCell => "Show the selected cell in the cell viewer",
            Self::EditCell => "Edit the selected cell, if its table can be edited",
            Self::OpenFile => "Open a .sql file in a new console",
            Self::Save => "Save the console to its .sql file",
            Self::SaveAs => "Save the console to a new .sql file",
            Self::Find => "Find in the console",
            Self::Replace => "Find and replace in the console",
            Self::FindNext => "Go to the next match",
            Self::FindPrevious => "Go to the previous match",
            Self::Settings => "Open the settings",
            Self::RunToFile => "Run the statement at the cursor straight into a file",
            Self::CopyWithHeaders => "Copy the selected cells with their column names",
            Self::CopyAs(format) => format.description(),
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    /// The keys IDEs and database tools commonly use for the action, on macOS.
    fn default_bindings(self) -> Vec<KeyboardShortcut> {
        let shortcut = |modifiers, key| vec![KeyboardShortcut::new(modifiers, key)];
        match self {
            // The usual completion key in IDEs, on every platform.
            Self::Complete => shortcut(Modifiers::CTRL, Key::Space),
            Self::RunCurrent => shortcut(Modifiers::COMMAND, Key::Enter),
            Self::RunAll => shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter),
            Self::Explain => shortcut(Modifiers::COMMAND | Modifiers::ALT, Key::E),
            Self::ExplainAnalyse => shortcut(
                Modifiers::COMMAND | Modifiers::ALT | Modifiers::SHIFT,
                Key::E,
            ),
            Self::Cancel => shortcut(Modifiers::COMMAND, Key::Period),
            // A common reformat key in IDEs, and the same with ⇧ for the whole console.
            Self::Format => shortcut(Modifiers::COMMAND | Modifiers::ALT, Key::L),
            Self::FormatAll => shortcut(
                Modifiers::COMMAND | Modifiers::ALT | Modifiers::SHIFT,
                Key::L,
            ),
            // VS Code's keys. F2, which some IDEs use for this, edits a grid cell here.
            Self::NextProblem => shortcut(Modifiers::NONE, Key::F8),
            Self::PreviousProblem => shortcut(Modifiers::SHIFT, Key::F8),
            Self::ViewCell => shortcut(Modifiers::NONE, Key::Space),
            Self::EditCell => vec![
                KeyboardShortcut::new(Modifiers::NONE, Key::F2),
                KeyboardShortcut::new(Modifiers::NONE, Key::Enter),
            ],
            Self::OpenFile => shortcut(Modifiers::COMMAND, Key::O),
            Self::Save => shortcut(Modifiers::COMMAND, Key::S),
            Self::SaveAs => shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::S),
            Self::Find => shortcut(Modifiers::COMMAND, Key::F),
            // VS Code's key on macOS; Ctrl+H elsewhere would differ by platform, and one
            // binding serves both here.
            Self::Replace => shortcut(Modifiers::COMMAND | Modifiers::ALT, Key::F),
            // ⌘G as in macOS's own apps, F3 as in Windows'.
            Self::FindNext => vec![
                KeyboardShortcut::new(Modifiers::COMMAND, Key::G),
                KeyboardShortcut::new(Modifiers::NONE, Key::F3),
            ],
            Self::FindPrevious => vec![
                KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::G),
                KeyboardShortcut::new(Modifiers::SHIFT, Key::F3),
            ],
            // The settings key of macOS apps, and of VS Code and JetBrains IDEs elsewhere.
            Self::Settings => shortcut(Modifiers::COMMAND, Key::Comma),
            // ⌘↵ with ⌥, as ⇧ runs everything: DataGrip's Execute to File has no key of its
            // own, and this one is free.
            Self::RunToFile => shortcut(Modifiers::COMMAND | Modifiers::ALT, Key::Enter),
            // ⌘C with ⇧, which no other action here takes.
            Self::CopyWithHeaders => shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::C),
            Self::ShowHistory | Self::Commit | Self::Rollback | Self::CopyAs(_) => Vec::new(),
        }
    }
}

/// The bindings in effect: the defaults, with whatever `keymap.toml` changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<Action, Vec<KeyboardShortcut>>,
    /// Custom actions from `actions.toml`, which have no keys until `keymap.toml` gives them
    /// some as `custom.<id>`.
    custom: BTreeMap<ActionId, Vec<KeyboardShortcut>>,
    /// What in the file couldn't be used, for Settings › Keymap. Each was skipped.
    problems: Vec<String>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: Action::ALL
                .into_iter()
                .map(|action| (action, action.default_bindings()))
                .collect(),
            custom: BTreeMap::new(),
            problems: Vec::new(),
        }
    }
}

impl Keymap {
    /// The defaults overridden by `file`, with keys for the custom actions `custom` names.
    /// Unknown actions and unreadable shortcuts are skipped and listed as problems, so one typo
    /// doesn't cost every other binding.
    pub fn from_file(file: &KeymapBindings, custom: &[&ActionId]) -> Self {
        let mut keymap = Self::default();
        for (id, shortcuts) in file {
            let parsed = keymap.parse_shortcuts(id, shortcuts);
            if let Some(action) = Action::from_id(id) {
                keymap.bindings.insert(action, parsed);
            } else if let Some(action) = custom_action(id, custom) {
                keymap.custom.insert(action.clone(), parsed);
            } else if id.starts_with(ActionId::KEYMAP_PREFIX) {
                keymap
                    .problems
                    .push(format!("{id:?} is not an action in actions.toml"));
            } else {
                keymap.problems.push(format!(
                    "{id:?} is not an action; see Settings › Keymap for the ids"
                ));
            }
        }
        keymap.report_conflicts();
        keymap
    }

    fn parse_shortcuts(&mut self, id: &str, shortcuts: &[String]) -> Vec<KeyboardShortcut> {
        let mut parsed = Vec::new();
        for text in shortcuts {
            match parse_shortcut(text) {
                Some(shortcut) => parsed.push(shortcut),
                None => self
                    .problems
                    .push(format!("{id}: {text:?} is not a shortcut")),
            }
        }
        parsed
    }

    /// A keymap whose file couldn't be read: the defaults, and why.
    pub fn unreadable(error: String) -> Self {
        Self {
            problems: vec![error],
            ..Self::default()
        }
    }

    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    pub fn bindings(&self, action: Action) -> &[KeyboardShortcut] {
        self.bindings.get(&action).map_or(&[], Vec::as_slice)
    }

    /// The keys `keymap.toml` gives a custom action, minus any a built-in action has.
    pub fn custom_bindings(&self, action: &ActionId) -> &[KeyboardShortcut] {
        self.custom.get(action).map_or(&[], Vec::as_slice)
    }

    /// Consumes this frame's key presses bound to custom actions, and returns the actions.
    pub fn consume_custom(&self, input: &mut InputState) -> Vec<ActionId> {
        let mut candidates: Vec<(&ActionId, KeyboardShortcut)> = self
            .custom
            .iter()
            .flat_map(|(action, shortcuts)| {
                shortcuts.iter().map(move |&shortcut| (action, shortcut))
            })
            .collect();
        candidates
            .sort_by_key(|(_, shortcut)| std::cmp::Reverse(modifier_count(shortcut.modifiers)));
        let mut triggered = Vec::new();
        for (action, shortcut) in candidates {
            if input.consume_shortcut(&shortcut) && !triggered.contains(action) {
                triggered.push(action.clone());
            }
        }
        triggered
    }

    /// The action's first shortcut, as the platform writes it (`⌘↵`), for tooltips.
    pub fn hint(&self, ctx: &egui::Context, action: Action) -> Option<String> {
        self.bindings(action)
            .first()
            .map(|shortcut| ctx.format_shortcut(shortcut))
    }

    /// Consumes the key presses of this frame that trigger any of `actions`, and returns the
    /// actions triggered. Shortcuts with more modifiers are tried first: egui lets extra
    /// modifiers through, so ⌘↵ would otherwise also fire on ⇧⌘↵.
    pub fn consume(&self, input: &mut InputState, actions: &[Action]) -> Vec<Action> {
        let mut candidates: Vec<(Action, KeyboardShortcut)> = actions
            .iter()
            .flat_map(|&action| {
                self.bindings(action)
                    .iter()
                    .map(move |&shortcut| (action, shortcut))
            })
            .collect();
        candidates
            .sort_by_key(|(_, shortcut)| std::cmp::Reverse(modifier_count(shortcut.modifiers)));
        let mut triggered = Vec::new();
        for (action, shortcut) in candidates {
            if input.consume_shortcut(&shortcut) && !triggered.contains(&action) {
                triggered.push(action);
            }
        }
        triggered
    }

    /// Reports shortcuts given twice. Between built-in actions the first in `Action`'s order
    /// keeps it; a custom action loses one a built-in action has, or another custom action has
    /// first, since the console and the app consume their keys before custom actions'.
    fn report_conflicts(&mut self) {
        let mut seen: Vec<(KeyboardShortcut, String)> = Vec::new();
        for (&action, shortcuts) in &self.bindings {
            for shortcut in shortcuts {
                match seen.iter().find(|(taken, _)| taken == shortcut) {
                    Some((_, first)) => self.problems.push(format!(
                        "{first} and {} share a shortcut; {first} gets it",
                        action.id()
                    )),
                    None => seen.push((*shortcut, action.id().to_owned())),
                }
            }
        }
        for (action, shortcuts) in &mut self.custom {
            let id = action.keymap_id();
            shortcuts.retain(
                |shortcut| match seen.iter().find(|(taken, _)| taken == shortcut) {
                    Some((_, first)) => {
                        self.problems.push(format!(
                            "{first} and {id} share a shortcut; {first} gets it"
                        ));
                        false
                    }
                    None => {
                        seen.push((*shortcut, id.clone()));
                        true
                    }
                },
            );
        }
    }
}

/// The custom action a `custom.<id>` binding names, if `actions.toml` has it.
fn custom_action<'a>(id: &str, custom: &[&'a ActionId]) -> Option<&'a ActionId> {
    let id = id.strip_prefix(ActionId::KEYMAP_PREFIX)?;
    custom.iter().copied().find(|action| action.as_str() == id)
}

fn modifier_count(modifiers: Modifiers) -> usize {
    [
        modifiers.alt,
        modifiers.ctrl,
        modifiers.shift,
        modifiers.command || modifiers.mac_cmd,
    ]
    .into_iter()
    .filter(|&held| held)
    .count()
}

/// Reads `cmd+shift+enter`: modifiers, then a key name, joined by `+`, ignoring case. `cmd` is
/// ⌘ on macOS and Ctrl elsewhere, as egui's `COMMAND`; `ctrl` is the Control key everywhere.
pub fn parse_shortcut(text: &str) -> Option<KeyboardShortcut> {
    let text = text.trim();
    // `cmd++` binds the plus key itself.
    let (modifier_part, key_name) = match text.strip_suffix("++") {
        Some(rest) => (rest, "+"),
        None => text.rsplit_once('+').unwrap_or(("", text)),
    };
    let mut modifiers = Modifiers::NONE;
    for name in modifier_part.split('+').filter(|name| !name.is_empty()) {
        modifiers |= match name.trim().to_ascii_lowercase().as_str() {
            "cmd" | "command" => Modifiers::COMMAND,
            "ctrl" | "control" => Modifiers::CTRL,
            "alt" | "option" | "opt" => Modifiers::ALT,
            "shift" => Modifiers::SHIFT,
            _ => return None,
        };
    }
    let key = parse_key(key_name.trim())?;
    Some(KeyboardShortcut::new(modifiers, key))
}

fn parse_key(name: &str) -> Option<Key> {
    Key::from_name(name).or_else(|| {
        Key::ALL
            .iter()
            .copied()
            .find(|key| key.name().eq_ignore_ascii_case(name))
    })
}

/// Writes `shortcut` the way [`parse_shortcut`] reads it.
fn shortcut_text(shortcut: &KeyboardShortcut) -> String {
    let modifiers = shortcut.modifiers;
    let mut text = String::new();
    for (held, name) in [
        (modifiers.command || modifiers.mac_cmd, "cmd"),
        (modifiers.ctrl && !modifiers.command, "ctrl"),
        (modifiers.alt, "alt"),
        (modifiers.shift, "shift"),
    ] {
        if held {
            text.push_str(name);
            text.push('+');
        }
    }
    text.push_str(&shortcut.logical_key.name().to_ascii_lowercase());
    text
}

/// A starting `keymap.toml`: every action with its default shortcuts, commented out, so the
/// file documents the ids and changes nothing until a line is uncommented.
pub fn template() -> String {
    let mut text = String::from(
        "# DbRover key bindings. Uncomment and change a line, then View → Keymap → Reload.\n\
         # An action takes one shortcut or a list of them; [] leaves it unbound.\n\
         # Modifiers: cmd (⌘ on macOS, Ctrl elsewhere), ctrl, alt, shift.\n\
         # Keys: letters, digits, enter, space, tab, escape, f1–f20, period, comma, …\n\
         \n[bindings]\n",
    );
    let defaults = Keymap::default();
    for action in Action::ALL {
        let shortcuts: Vec<String> = defaults
            .bindings(action)
            .iter()
            .map(|shortcut| format!("\"{}\"", shortcut_text(shortcut)))
            .collect();
        // Writing to a String can't fail.
        let _ = writeln!(
            text,
            "# {:?} = [{}]  # {}",
            action.id(),
            shortcuts.join(", "),
            action.description()
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bindings(entries: &[(&str, &[&str])]) -> KeymapBindings {
        entries
            .iter()
            .map(|(id, shortcuts)| {
                (
                    (*id).to_owned(),
                    shortcuts.iter().map(|s| (*s).to_owned()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn parses_modifiers_and_key_names_ignoring_case() {
        let cases = [
            (
                "cmd+enter",
                Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter)),
            ),
            (
                "Ctrl+Shift+R",
                Some(KeyboardShortcut::new(
                    Modifiers::CTRL | Modifiers::SHIFT,
                    Key::R,
                )),
            ),
            (
                "option+pagedown",
                Some(KeyboardShortcut::new(Modifiers::ALT, Key::PageDown)),
            ),
            ("f5", Some(KeyboardShortcut::new(Modifiers::NONE, Key::F5))),
            (
                "cmd++",
                Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Plus)),
            ),
            (
                "cmd+.",
                Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Period)),
            ),
            ("hyper+x", None),
            ("cmd+", None),
            ("", None),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_shortcut(text), expected, "{text:?}");
        }
    }

    #[test]
    fn the_file_overrides_defaults_and_reports_what_it_skipped() {
        let keymap = Keymap::from_file_for_tests(&bindings(&[
            ("console.run", &["cmd+r", "nonsense+key"]),
            ("console.cancel", &[]),
            ("console.runn", &["f5"]),
        ]));

        assert_eq!(
            keymap.bindings(Action::RunCurrent),
            [KeyboardShortcut::new(Modifiers::COMMAND, Key::R)]
        );
        assert!(keymap.bindings(Action::Cancel).is_empty());
        assert_eq!(
            keymap.bindings(Action::Explain),
            Action::Explain.default_bindings()
        );
        assert_eq!(keymap.problems().len(), 2, "{:?}", keymap.problems());
    }

    impl Keymap {
        fn from_file_for_tests(file: &KeymapBindings) -> Self {
            Self::from_file(file, &[])
        }
    }

    #[test]
    fn binds_custom_actions_actions_toml_has_and_reports_the_rest() {
        let row_count = ActionId::new("row-count").unwrap();
        let latest = ActionId::new("latest").unwrap();
        let keymap = Keymap::from_file(
            &bindings(&[
                ("custom.row-count", &["cmd+shift+y"]),
                ("custom.latest", &["cmd+enter", "f6"]),
                ("custom.gone", &["f7"]),
            ]),
            &[&row_count, &latest],
        );

        assert_eq!(
            keymap.custom_bindings(&row_count),
            [KeyboardShortcut::new(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::Y
            )]
        );
        assert_eq!(
            keymap.custom_bindings(&latest),
            [KeyboardShortcut::new(Modifiers::NONE, Key::F6)],
            "console.run keeps ⌘↵"
        );
        assert_eq!(
            keymap.problems(),
            [
                "\"custom.gone\" is not an action in actions.toml",
                "console.run and custom.latest share a shortcut; console.run gets it",
            ]
        );
    }

    #[test]
    fn reports_two_actions_on_one_shortcut() {
        let keymap = Keymap::from_file_for_tests(&bindings(&[("console.history", &["cmd+enter"])]));

        assert_eq!(
            keymap.problems(),
            ["console.run and console.history share a shortcut; console.run gets it"]
        );
    }

    #[test]
    fn the_template_lists_every_default_in_a_form_the_parser_reads_back() {
        let template = template();
        for action in Action::ALL {
            assert!(template.contains(action.id()), "{}", action.id());
        }
        let uncommented: String = template
            .lines()
            .map(|line| {
                line.strip_prefix("# ")
                    .filter(|_| line.starts_with("# \""))
                    .unwrap_or(line)
            })
            .map(|line| line.split("  #").next().unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed: toml::Value = toml::from_str(&uncommented).unwrap();
        let file: KeymapBindings = parsed["bindings"]
            .as_table()
            .unwrap()
            .iter()
            .map(|(id, value)| {
                let shortcuts = value
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_owned())
                    .collect();
                (id.clone(), shortcuts)
            })
            .collect();
        let keymap = Keymap::from_file(&file, &[]);
        assert!(keymap.problems().is_empty(), "{:?}", keymap.problems());
        for action in Action::ALL {
            assert_eq!(
                keymap.bindings(action),
                action.default_bindings(),
                "{action:?}"
            );
        }
    }
}
