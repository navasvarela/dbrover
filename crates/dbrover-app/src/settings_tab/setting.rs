//! Every setting the tab shows: where it belongs, what it is called, and what a search finds it
//! by. The tab lays them out from this list, so a setting added here shows in its category and
//! in search results alike.

use std::borrow::Cow;

use dbrover_sql::LintRule;

use crate::icons;

/// A page of the tab, listed in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Category {
    #[default]
    Appearance,
    Editor,
    History,
    Mcp,
    Updates,
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Appearance,
        Self::Editor,
        Self::History,
        Self::Mcp,
        Self::Updates,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Editor => "Editor",
            Self::History => "Query history",
            Self::Mcp => "MCP",
            Self::Updates => "Updates",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Appearance => icons::PALETTE,
            Self::Editor => icons::CODE,
            Self::History => icons::CLOCK_COUNTER_CLOCKWISE,
            Self::Mcp => icons::PLUGS_CONNECTED,
            Self::Updates => icons::ARROWS_CLOCKWISE,
        }
    }

    /// The line under the page's title.
    pub fn summary(self) -> &'static str {
        match self {
            Self::Appearance => "How DbRover looks.",
            Self::Editor => "How SQL is formatted, and what the console checks while you type.",
            Self::History => "How long the query history keeps what ran.",
            Self::Mcp => "What MCP clients may do in a project that doesn't say otherwise.",
            Self::Updates => "Whether DbRover looks for a new release.",
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
    CheckForUpdates,
    McpDefaults,
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
        settings.extend([
            Self::HistoryAge,
            Self::HistoryCount,
            Self::McpDefaults,
            Self::CheckForUpdates,
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
            Self::McpDefaults => Category::Mcp,
            Self::CheckForUpdates => Category::Updates,
        }
    }

    /// The caption of the card it is in: settings with the same one share a card.
    pub fn section(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::KeywordCase | Self::IndentWidth => "SQL formatting",
            Self::AskDatabase | Self::Lint(_) => "Problems while typing",
            Self::HistoryAge | Self::HistoryCount => "Keeping statements",
            Self::McpDefaults => "Defaults for every project",
            Self::CheckForUpdates => "New releases",
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
            Self::CheckForUpdates => "Check for a new release when DbRover starts".into(),
            Self::McpDefaults => "What MCP clients may do".into(),
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
            Self::CheckForUpdates => {
                "Asks GitHub once per start. Help › Check for updates works either way.".into()
            }
            Self::McpDefaults => "Servers already running keep what they started with.".into(),
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
            Self::CheckForUpdates => &["update", "release", "version"],
            Self::McpDefaults => &[
                "read",
                "write",
                "schema",
                "admin",
                "row limit",
                "timeout",
                "permissions",
                "agent",
            ],
        }
    }

    /// Whether every word of `query` is in what the setting shows or is found by, ignoring
    /// case.
    pub fn matches(self, query: &str) -> bool {
        let haystack = self.search_text();
        query
            .split_whitespace()
            .all(|word| haystack.contains(&word.to_lowercase()))
    }

    fn search_text(self) -> String {
        let mut parts = vec![
            self.title(),
            self.section().into(),
            self.category().title().into(),
        ];
        parts.extend(self.description());
        parts.extend(self.keywords().iter().map(|&word| Cow::Borrowed(word)));
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
        assert_eq!(found("timeout"), [Setting::McpDefaults]);
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
