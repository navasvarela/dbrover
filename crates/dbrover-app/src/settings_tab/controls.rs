//! The control each setting is changed with, drawn into its row.

use std::num::NonZeroU32;

use dbrover_store::{KeywordCase, McpPermissions, Settings};
use egui::{ThemePreference, Ui};

use super::setting::Setting;
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
}

/// Draws `setting` as a row of `rows`.
pub fn row(rows: &mut Rows<'_>, setting: Setting, draft: &mut Draft<'_>) {
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
        Setting::CheckForUpdates => {
            form::toggle(ui, &mut settings.updates.check_on_start, title);
        }
        // Drawn across the whole card by `row`.
        Setting::McpDefaults => {}
    }
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
