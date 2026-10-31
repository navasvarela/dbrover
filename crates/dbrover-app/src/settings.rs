//! The settings from `settings.toml`, as the engine last read them, and what they mean for
//! the parts of the app that use them.

use std::path::PathBuf;

use dbrover_engine::Event;

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
}

impl AppSettings {
    pub fn apply(&mut self, event: &Event) {
        let Event::SettingsLoaded { path, result } = event else {
            return;
        };
        self.path = Some(path.clone());
        match result {
            Ok(settings) => {
                self.settings = settings.clone();
                self.error = None;
            }
            Err(error) => self.error = Some(error.clone()),
        }
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
}
