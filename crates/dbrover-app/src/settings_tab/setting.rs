//! Every setting the tab shows: where it belongs, what it is called, and what a search finds it
//! by. The tab lays them out from this list, so a setting added here shows in its category and
//! in search results alike.

use std::borrow::Cow;

use dbrover_sql::LintRule;

use super::report_defaults::ReportDefault;
use crate::icons;

/// A page of the tab made of settings' rows, listed in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Category {
    #[default]
    Appearance,
    Editor,
    History,
    Reports,
    Mcp,
    Updates,
    Files,
}

impl Category {
    pub const ALL: [Self; 7] = [
        Self::Appearance,
        Self::Editor,
        Self::History,
        Self::Reports,
        Self::Mcp,
        Self::Updates,
        Self::Files,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Editor => "Editor",
            Self::History => "Query history",
            Self::Reports => "Reports",
            Self::Mcp => "MCP",
            Self::Updates => "Updates",
            Self::Files => "Files",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Appearance => icons::PALETTE,
            Self::Editor => icons::CODE,
            Self::History => icons::CLOCK_COUNTER_CLOCKWISE,
            Self::Reports => icons::CLIPBOARD_TEXT,
            Self::Mcp => icons::PLUGS_CONNECTED,
            Self::Updates => icons::ARROWS_CLOCKWISE,
            Self::Files => icons::FOLDER_SIMPLE,
        }
    }

    /// The line under the page's title.
    pub fn summary(self) -> &'static str {
        match self {
            Self::Appearance => "How DbRover looks.",
            Self::Editor => "How SQL is formatted, and what the console checks while you type.",
            Self::History => "How long the query history keeps what ran.",
            Self::Reports => {
                "The limits every report runs under unless its file sets its own, and the runs \
                 kept."
            }
            Self::Mcp => "What MCP clients may do in a project that doesn't say otherwise.",
            Self::Updates => "Whether DbRover looks for a new release.",
            Self::Files => "Where DbRover keeps your settings.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Theme,
    KeywordCase,
    IndentWidth,
    AskDatabase,
    Lint(LintRule),
    HistoryAge,
    HistoryCount,
    Report(ReportDefault),
    CheckForUpdates,
    McpDefaults,
    SettingsFolder,
}

impl Setting {
    /// Every setting, in the order the pages show them.
    pub fn all() -> Vec<Self> {
        let mut settings = vec![
            Self::Theme,
            Self::KeywordCase,
            Self::IndentWidth,
            Self::AskDatabase,
        ];
        settings.extend(LintRule::ALL.into_iter().map(Self::Lint));
        settings.extend([Self::HistoryAge, Self::HistoryCount]);
        settings.extend(ReportDefault::ALL.into_iter().map(Self::Report));
        settings.extend([
            Self::McpDefaults,
            Self::CheckForUpdates,
            Self::SettingsFolder,
        ]);
        settings
    }

    pub fn category(self) -> Category {
        match self {
            Self::Theme => Category::Appearance,
            Self::KeywordCase | Self::IndentWidth | Self::AskDatabase | Self::Lint(_) => {
                Category::Editor
            }
            Self::HistoryAge | Self::HistoryCount => Category::History,
            Self::Report(_) => Category::Reports,
            Self::McpDefaults => Category::Mcp,
            Self::CheckForUpdates => Category::Updates,
            Self::SettingsFolder => Category::Files,
        }
    }

    /// The caption of the card it is in: settings with the same one share a card.
    pub fn section(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::KeywordCase | Self::IndentWidth => "SQL formatting",
            Self::AskDatabase | Self::Lint(_) => "Problems while typing",
            Self::HistoryAge | Self::HistoryCount => "Keeping statements",
            Self::Report(default) => default.section(),
            Self::McpDefaults => "Defaults for every project",
            Self::CheckForUpdates => "New releases",
            Self::SettingsFolder => "Settings folder",
        }
    }

    pub fn title(self) -> Cow<'static, str> {
        match self {
            Self::Theme => "Theme".into(),
            Self::KeywordCase => "Keywords".into(),
            Self::IndentWidth => "Indent".into(),
            Self::AskDatabase => "Ask the database to prepare statements".into(),
            Self::Lint(rule) => rule.description().into(),
            Self::HistoryAge => "Keep statements for".into(),
            Self::HistoryCount => "Keep per data source".into(),
            Self::Report(default) => default.title().into(),
            Self::CheckForUpdates => "Check for a new release when DbRover starts".into(),
            Self::McpDefaults => "What MCP clients may do".into(),
            Self::SettingsFolder => "Settings folder".into(),
        }
    }

    pub fn description(self) -> Option<Cow<'static, str>> {
        let text: Cow<'static, str> = match self {
            Self::Theme => "The View menu switches it too.".into(),
            Self::KeywordCase => "How Format SQL writes keywords.".into(),
            Self::IndentWidth => return None,
            Self::AskDatabase => "In connected consoles. Nothing runs.".into(),
            Self::Lint(rule) => format!("Rule {}", rule.id()).into(),
            Self::HistoryAge => {
                "Older statements are deleted at start-up and when this changes.".into()
            }
            Self::HistoryCount => return None,
            Self::Report(default) => default.description().into(),
            Self::CheckForUpdates => {
                "Asks GitHub once per start. Help › Check for updates works either way.".into()
            }
            Self::McpDefaults => "Servers already running keep what they started with.".into(),
            Self::SettingsFolder => {
                "Your data sources, projects, consoles and own files. A change applies when \
                 DbRover restarts."
                    .into()
            }
        };
        Some(text)
    }

    /// Words a search finds the setting by, besides those it shows.
    fn keywords(self) -> &'static [&'static str] {
        match self {
            Self::Theme => &["dark", "light", "system", "colour", "color"],
            Self::KeywordCase => &["format", "upper", "lower", "case"],
            Self::IndentWidth => &["format", "spaces", "width"],
            Self::AskDatabase | Self::Lint(_) => &["lint", "check", "warning"],
            Self::HistoryAge | Self::HistoryCount => &["history", "delete", "limit", "prune"],
            Self::Report(_) => &["report", "limit", "default", "guardrail"],
            Self::CheckForUpdates => &["update", "release", "version"],
            Self::McpDefaults => &[
                "read",
                "write",
                "schema",
                "admin",
                "reports",
                "row limit",
                "timeout",
                "permissions",
                "agent",
            ],
            Self::SettingsFolder => &["folder", "config", "directory", "sync", "dotfiles"],
        }
    }

    /// Whether every word of `query` is in what the setting shows or is found by, ignoring
    /// case.
    pub fn matches(self, query: &str) -> bool {
        super::search::has_every_word(&self.search_text(), query)
    }

    fn search_text(self) -> String {
        let mut parts = vec![
            self.title(),
            self.section().into(),
            self.category().title().into(),
        ];
        parts.extend(self.description());
        parts.extend(self.keywords().iter().map(|&word| Cow::Borrowed(word)));
        if let Self::Report(default) = self {
            parts.push(default.key().into());
        }
        parts.join(" ").to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_settings_by_any_word_they_show_or_are_known_by() {
        let found = |query: &str| -> Vec<Setting> {
            Setting::all()
                .into_iter()
                .filter(|setting| setting.matches(query))
                .collect()
        };

        assert_eq!(found("DARK"), [Setting::Theme]);
        assert_eq!(found("indent"), [Setting::IndentWidth]);
        assert_eq!(
            found("history delete"),
            [Setting::HistoryAge, Setting::HistoryCount]
        );
        assert_eq!(
            found("timeout"),
            [
                Setting::Report(ReportDefault::SourceTimeout),
                Setting::Report(ReportDefault::CombineTimeout),
                Setting::Report(ReportDefault::RunTimeout),
                Setting::Report(ReportDefault::StallTimeout),
                Setting::McpDefaults
            ]
        );
        assert_eq!(
            found("memory_limit"),
            [Setting::Report(ReportDefault::MemoryLimit)]
        );
        assert!(found("no such setting").is_empty());
        assert_eq!(
            found("").len(),
            Setting::all().len(),
            "an empty query hides none"
        );
    }

    #[test]
    fn every_category_has_a_setting() {
        for category in Category::ALL {
            assert!(
                Setting::all()
                    .iter()
                    .any(|setting| setting.category() == category),
                "{category:?}"
            );
        }
    }
}
