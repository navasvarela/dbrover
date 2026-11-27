//! The control each setting is changed with, drawn into its row.

use std::num::NonZeroU32;

use dbrover_store::{ActivitySettings, DataSearchSettings, KeywordCase, McpPermissions, Settings};
use egui::{Button, RichText, ThemePreference, Ui};

use super::setting::Setting;
use crate::folder_switch::{FolderInUse, FolderRequest};
use crate::form::{self, Rows, Segment};
use crate::mcp_settings::McpDefaultsEditor;
use crate::settings::INDENT_WIDTHS;

/// What the controls change this frame: a copy of the settings and theme, compared with the
/// originals afterwards, and the MCP defaults' own editor, saved with its own button.
pub struct Draft<'a> {
    pub settings: Settings,
    pub theme: ThemePreference,
    pub mcp: &'a mut McpDefaultsEditor,
    /// The MCP defaults to save, once their Save button is clicked.
    pub mcp_to_save: Option<McpPermissions>,
    /// The settings folder in use; unknown in tests that don't give one.
    pub folder: Option<&'a FolderInUse>,
    pub folder_request: Option<FolderRequest>,
}

/// Draws `setting` as a row of `rows`.
pub fn row(rows: &mut Rows<'_>, setting: Setting, draft: &mut Draft<'_>) {
    if setting == Setting::SettingsFolder {
        rows.full(|ui| draft.folder_request = settings_folder(ui, setting, draft.folder));
        return;
    }
    if setting == Setting::McpDefaults {
        let title = setting.title();
        rows.full(|ui| {
            ui.label(title.as_ref());
            if let Some(description) = setting.description() {
                form::description_label(ui, &description);
            }
            ui.add_space(ui.spacing().item_spacing.y);
            draft.mcp_to_save = draft.mcp.show(ui);
        });
        return;
    }
    let title = setting.title();
    let description = setting.description();
    rows.row(&title, description.as_deref(), |ui| {
        control(ui, setting, &title, draft);
    });
}

fn control(ui: &mut Ui, setting: Setting, title: &str, draft: &mut Draft<'_>) {
    let settings = &mut draft.settings;
    match setting {
        Setting::Theme => theme(ui, &mut draft.theme),
        Setting::KeywordCase => keyword_case(ui, &mut settings.format.keyword_case),
        Setting::IndentWidth => indent_width(ui, &mut settings.format.indent_width),
        Setting::AskDatabase => {
            form::toggle(ui, &mut settings.lint.ask_database, title);
        }
        Setting::Lint(rule) => {
            let disabled = &mut settings.lint.disabled;
            let mut enabled = !disabled.iter().any(|id| id == rule.id());
            if form::toggle(ui, &mut enabled, title).changed() {
                if enabled {
                    disabled.retain(|id| id != rule.id());
                } else {
                    disabled.push(rule.id().to_owned());
                }
            }
        }
        Setting::HistoryAge => limit_choice(
            ui,
            "history_max_age",
            &mut settings.history.max_age_days,
            &AGES,
            |days| format!("{days} days"),
        ),
        Setting::HistoryCount => limit_choice(
            ui,
            "history_max_count",
            &mut settings.history.max_per_data_source,
            &COUNTS,
            |count| format!("the newest {count}"),
        ),
        Setting::ActivityRefresh => activity_refresh(ui, &mut settings.activity.refresh_seconds),
        Setting::SearchTimeLimit => search_time_limit(ui, &mut settings.data_search.table_seconds),
        Setting::SearchRows => search_rows(ui, &mut settings.data_search.rows_per_table),
        Setting::SearchViews => {
            form::toggle(ui, &mut settings.data_search.include_views, title);
        }
        Setting::DumpProgram(program) => dump_program(ui, program, &mut settings.dumps),
        Setting::Report(default) => default.control(ui, &mut settings.reports),
        Setting::CheckForUpdates => {
            form::toggle(ui, &mut settings.updates.check_on_start, title);
        }
        // Drawn across the whole card by `row`.
        Setting::McpDefaults | Setting::SettingsFolder => {}
    }
}

/// The folder in use, where that came from, and the buttons that change it, unless the flag or
/// the variable set it for this start.
fn settings_folder(
    ui: &mut Ui,
    setting: Setting,
    folder: Option<&FolderInUse>,
) -> Option<FolderRequest> {
    ui.label(setting.title().as_ref());
    if let Some(description) = setting.description() {
        form::description_label(ui, &description);
    }
    let folder = folder?;
    ui.add_space(ui.spacing().item_spacing.y);
    ui.label(RichText::new(folder.folders.settings.display().to_string()).monospace());
    form::description_label(ui, &folder.describe_source());
    let gitignore = git_status(ui, folder);
    let locked = folder.locked_because();
    let switch = ui
        .horizontal(|ui| {
            let unlocked = locked.is_none();
            let mut choose = ui.add_enabled(unlocked, Button::new("Choose…"));
            let mut default = ui.add_enabled(
                unlocked && !folder.is_default(),
                Button::new("Use the default folder"),
            );
            if let Some(reason) = &locked {
                choose = choose.on_disabled_hover_text(reason);
                default = default.on_disabled_hover_text(reason);
            }
            if choose.clicked() {
                Some(FolderRequest::Choose)
            } else if default.clicked() {
                Some(FolderRequest::UseDefault)
            } else {
                None
            }
        })
        .inner;
    switch.or(gitignore)
}

/// Whether the folder is in a Git repository, and, when it has no `.gitignore`, a button that
/// writes DbRover's (VC1d). A `.gitignore` of the user's own is never judged.
fn git_status(ui: &mut Ui, folder: &FolderInUse) -> Option<FolderRequest> {
    let in_repository = match &folder.git.repository {
        Some(top) => format!("In the Git repository at {}.", top.display()),
        None => "Not in a Git repository.".to_owned(),
    };
    form::description_label(ui, &in_repository);
    if folder.git.has_gitignore {
        return None;
    }
    ui.horizontal(|ui| {
        form::description_label(
            ui,
            "No .gitignore: Git would commit the query history with the settings.",
        );
        ui.button("Add a .gitignore")
            .on_hover_text("Leave history.sqlite out of commits; nothing else changes")
            .clicked()
            .then_some(FolderRequest::AddGitignore)
    })
    .inner
}

fn theme(ui: &mut Ui, preference: &mut ThemePreference) {
    form::segmented(
        ui,
        preference,
        &[
            Segment::new(ThemePreference::System, "Match system"),
            Segment::new(ThemePreference::Light, "Light"),
            Segment::new(ThemePreference::Dark, "Dark"),
        ],
    );
}

fn keyword_case(ui: &mut Ui, case: &mut KeywordCase) {
    form::segmented(
        ui,
        case,
        &[
            Segment::new(KeywordCase::Upper, "UPPER"),
            Segment::new(KeywordCase::Lower, "lower"),
            Segment::new(KeywordCase::Preserve, "As typed"),
        ],
    );
}

fn indent_width(ui: &mut Ui, width: &mut u8) {
    egui::ComboBox::from_id_salt("indent_width")
        .selected_text(spaces(*width))
        .show_ui(ui, |ui| {
            for choice in INDENT_WIDTHS {
                ui.selectable_value(width, choice, spaces(choice));
            }
        });
}

fn spaces(width: u8) -> String {
    match width {
        1 => "1 space".to_owned(),
        n => format!("{n} spaces"),
    }
}

/// The lengths offered for the history's age, in days. Chosen from a few rather than typed:
/// every change is saved, and prunes, at once, so typing 365 would delete everything older
/// than 3 days on the way.
const AGES: [(u32, &str); 4] = [
    (30, "30 days"),
    (90, "90 days"),
    (365, "a year"),
    (730, "two years"),
];

/// The counts offered per data source.
const COUNTS: [(u32, &str); 3] = [
    (10_000, "the newest 10,000"),
    (100_000, "the newest 100,000"),
    (1_000_000, "the newest 1,000,000"),
];

/// A menu of `choices` and no limit. A value set by hand in `settings.toml` shows as it is.
fn limit_choice(
    ui: &mut Ui,
    id: &str,
    limit: &mut Option<NonZeroU32>,
    choices: &[(u32, &str)],
    custom: impl Fn(u32) -> String,
) {
    let label = |value: Option<NonZeroU32>| match value {
        None => "no limit".to_owned(),
        Some(value) => choices
            .iter()
            .find(|(choice, _)| *choice == value.get())
            .map_or_else(|| custom(value.get()), |(_, label)| (*label).to_owned()),
    };
    egui::ComboBox::from_id_salt(id)
        .selected_text(label(*limit))
        .show_ui(ui, |ui| {
            ui.selectable_value(limit, None, "no limit");
            for &(choice, text) in choices {
                ui.selectable_value(limit, NonZeroU32::new(choice), text);
            }
        });
}

/// The intervals a new Server activity tab may read at, in seconds; 0 opens it paused.
const REFRESHES: [(u32, &str); 6] = [
    (0, "paused"),
    (1, "every second"),
    (2, "every 2 s"),
    (5, "every 5 s"),
    (10, "every 10 s"),
    (30, "every 30 s"),
];

/// Unset means DbRover's default, so choosing the default unsets it again.
fn activity_refresh(ui: &mut Ui, seconds: &mut Option<u32>) {
    let default = ActivitySettings::DEFAULT_REFRESH_SECONDS;
    let mut chosen = seconds.unwrap_or(default);
    let label = |value: u32| {
        REFRESHES
            .iter()
            .find(|(choice, _)| *choice == value)
            .map_or_else(
                || format!("every {value} s"),
                |(_, label)| (*label).to_owned(),
            )
    };
    egui::ComboBox::from_id_salt("activity_refresh")
        .selected_text(label(chosen))
        .show_ui(ui, |ui| {
            for &(choice, text) in &REFRESHES {
                ui.selectable_value(&mut chosen, choice, text);
            }
        });
    *seconds = (chosen != default).then_some(chosen);
}

/// How long a data search may spend on one table, in seconds; 0 for no limit.
const TABLE_LIMITS: [(u32, &str); 6] = [
    (5, "5 s"),
    (15, "15 s"),
    (30, "30 s"),
    (60, "1 min"),
    (300, "5 min"),
    (0, "never"),
];

/// Unset means DbRover's default, so choosing the default unsets it again.
fn search_time_limit(ui: &mut Ui, seconds: &mut Option<u32>) {
    let default = DataSearchSettings::DEFAULT_TABLE_SECONDS;
    let mut chosen = seconds.unwrap_or(default);
    let label = |value: u32| {
        TABLE_LIMITS
            .iter()
            .find(|(choice, _)| *choice == value)
            .map_or_else(|| format!("{value} s"), |(_, label)| (*label).to_owned())
    };
    egui::ComboBox::from_id_salt("search_time_limit")
        .selected_text(label(chosen))
        .show_ui(ui, |ui| {
            for &(choice, text) in &TABLE_LIMITS {
                ui.selectable_value(&mut chosen, choice, text);
            }
        });
    *seconds = (chosen != default).then_some(chosen);
}

/// The rows a data search may show per table.
const ROWS_PER_TABLE: [u32; 5] = [10, 50, 100, 500, 1_000];

fn search_rows(ui: &mut Ui, rows: &mut Option<NonZeroU32>) {
    let default = DataSearchSettings::DEFAULT_ROWS_PER_TABLE;
    let mut chosen = rows.map_or(default, NonZeroU32::get);
    egui::ComboBox::from_id_salt("search_rows")
        .selected_text(chosen.to_string())
        .show_ui(ui, |ui| {
            for choice in ROWS_PER_TABLE {
                ui.selectable_value(&mut chosen, choice, choice.to_string());
            }
        });
    *rows = NonZeroU32::new(chosen).filter(|rows| rows.get() != default);
}

/// A dump program's path, empty for the one DbRover finds.
fn dump_program(
    ui: &mut Ui,
    program: dbrover_dump::Program,
    dumps: &mut dbrover_store::DumpSettings,
) {
    let key = program.setting_key();
    let mut text = dumps
        .programs
        .get(key)
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let field = egui::TextEdit::singleline(&mut text)
        .hint_text("Found by DbRover")
        .desired_width(280.0);
    if crate::widgets::name(
        ui.add(field),
        egui::WidgetType::TextEdit,
        program.name(),
        true,
    )
    .changed()
    {
        let text = text.trim();
        if text.is_empty() {
            dumps.programs.remove(key);
        } else {
            dumps.programs.insert(key.to_owned(), text.into());
        }
    }
}
