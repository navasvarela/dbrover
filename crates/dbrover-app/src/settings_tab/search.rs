//! What a search of the tab finds: settings, and what the other pages list. The sidebar's
//! counts and the results page both come from [`Found`], so a page whose items are added here
//! is searched and counted alike.

use super::Page;
use super::keymap_page::Shortcut;
use super::setting::Setting;

/// One thing a search can find, with the page it belongs to.
pub enum Found<'a> {
    Setting(Setting),
    Shortcut(Shortcut<'a>),
}

impl Found<'_> {
    pub fn page(&self) -> Page {
        match self {
            Self::Setting(setting) => Page::Settings(setting.category()),
            Self::Shortcut(_) => Page::Keymap,
        }
    }

    /// The caption of the card it is in, under its page's title.
    pub fn section(&self) -> &'static str {
        match self {
            Self::Setting(setting) => setting.section(),
            Self::Shortcut(shortcut) => shortcut.section.title(),
        }
    }

    pub fn matches(&self, query: &str) -> bool {
        match self {
            Self::Setting(setting) => setting.matches(query),
            Self::Shortcut(shortcut) => shortcut.matches(query),
        }
    }
}

/// Whether every word of `query` is in `haystack`, ignoring case. `haystack` is lower case
/// already: it is built once per item, and the query's words are few.
pub fn has_every_word(haystack: &str, query: &str) -> bool {
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}
