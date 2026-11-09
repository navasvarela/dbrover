//! The settings from `settings.toml`, as the engine last read them, and what they mean for
//! the parts of the app that use them.

use std::path::PathBuf;

use dbrover_engine::{Command, Document, Event, Noticed};

use crate::console::LintOptions;
use dbrover_sql::{FormatOptions, KeywordCase, LintRule};
use dbrover_store::{FormatSettings, Settings};

/// The indent widths the formatter accepts; a file asking for more or less gets the nearest.
pub const INDENT_WIDTHS: std::ops::RangeInclusive<u8> = 1..=8;

#[derive(Debug, Default)]
pub struct AppSettings {
    settings: Settings,
    /// Where `settings.toml` is, once the engine has said.
    path: Option<PathBuf>,
    /// Why the file couldn't be read or written; the settings in use are then the last good
    /// ones (or the defaults).
    error: Option<String>,
    /// Another program changed the file and a save found it: the settings in use aren't saved
    /// until **Reload** or **Keep mine**.
    changed_on_disk: bool,
}

impl AppSettings {
    /// Takes in what the engine read or found, and returns what that calls for: reading the
    /// file again when another program changed it and nothing here is waiting to be saved.
    pub fn apply(&mut self, event: &Event) -> Option<Command> {
        match event {
            Event::SettingsLoaded { path, result } => {
                self.path = Some(path.clone());
                match result {
                    Ok(settings) => {
                        self.settings = settings.clone();
                        self.error = None;
                        self.changed_on_disk = false;
                    }
                    Err(error) => self.error = Some(error.clone()),
                }
                None
            }
            Event::SettingsChangedOnDisk { noticed, .. } => {
                if *noticed == Noticed::Looking && !self.changed_on_disk {
                    return Some(Command::ReloadSettings);
                }
                self.changed_on_disk = true;
                None
            }
            _ => None,
        }
    }

    /// Whether a save found the file changed by another program, and waits for **Reload** or
    /// **Keep mine**.
    pub fn changed_on_disk(&self) -> bool {
        self.changed_on_disk
    }

    /// **Keep mine**: the commands that write the settings in use over the file's.
    pub fn keep_mine(&mut self) -> [Command; 2] {
        self.changed_on_disk = false;
        [
            Command::SeenOnDisk(Document::Settings),
            Command::SaveSettings(self.settings.clone()),
        ]
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Takes `settings` into use at once, before the engine has written them, so the window
    /// never lags behind a click.
    pub fn set(&mut self, settings: Settings) {
        self.settings = settings;
    }

    pub fn format_options(&self) -> FormatOptions {
        format_options(&self.settings.format)
    }

    pub fn lint_options(&self) -> LintOptions {
        LintOptions {
            disabled: self
                .settings
                .lint
                .disabled
                .iter()
                .filter_map(|id| LintRule::from_id(id))
                .collect(),
            ask_database: self.settings.lint.ask_database,
        }
    }

    /// What in the file couldn't be used: rule ids that name no rule.
    pub fn problems(&self) -> Vec<String> {
        self.settings
            .lint
            .disabled
            .iter()
            .filter(|id| LintRule::from_id(id).is_none())
            .map(|id| format!("lint.disabled: {id:?} is not a lint rule"))
            .collect()
    }
}

fn format_options(settings: &FormatSettings) -> FormatOptions {
    FormatOptions {
        keyword_case: match settings.keyword_case {
            dbrover_store::KeywordCase::Upper => KeywordCase::Upper,
            dbrover_store::KeywordCase::Lower => KeywordCase::Lower,
            dbrover_store::KeywordCase::Preserve => KeywordCase::Preserve,
        },
        indent_width: usize::from(
            settings
                .indent_width
                .clamp(*INDENT_WIDTHS.start(), *INDENT_WIDTHS.end()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_last_good_settings_when_the_file_breaks() {
        let mut settings = AppSettings::default();
        let lower = Settings {
            format: FormatSettings {
                keyword_case: dbrover_store::KeywordCase::Lower,
                indent_width: 40,
            },
            ..Settings::default()
        };
        settings.apply(&Event::SettingsLoaded {
            path: "settings.toml".into(),
            result: Ok(lower.clone()),
        });
        settings.apply(&Event::SettingsLoaded {
            path: "settings.toml".into(),
            result: Err("not TOML".to_owned()),
        });

        assert_eq!(settings.settings(), &lower);
        assert_eq!(settings.error(), Some("not TOML"));
        assert_eq!(
            settings.format_options(),
            FormatOptions {
                keyword_case: KeywordCase::Lower,
                indent_width: 8,
            }
        );
    }

    #[test]
    fn reads_a_changed_file_again_unless_a_save_found_it_and_then_asks() {
        let mut settings = AppSettings::default();
        let changed = |noticed| Event::SettingsChangedOnDisk {
            settings: Settings::default(),
            noticed,
        };

        let reload = settings.apply(&changed(Noticed::Looking));
        assert!(matches!(reload, Some(Command::ReloadSettings)));
        assert!(!settings.changed_on_disk());

        assert!(settings.apply(&changed(Noticed::Saving)).is_none());
        assert!(settings.changed_on_disk());
        assert!(
            settings.apply(&changed(Noticed::Looking)).is_none(),
            "still asking"
        );
        let [seen, save] = settings.keep_mine();
        assert!(matches!(seen, Command::SeenOnDisk(Document::Settings)));
        assert!(matches!(save, Command::SaveSettings(_)));
        assert!(!settings.changed_on_disk());
    }
}
