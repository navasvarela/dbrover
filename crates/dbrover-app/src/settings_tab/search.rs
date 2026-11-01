//! What a search of the tab finds: settings, and what the other pages list. The sidebar's
//! counts and the results page both come from [`Found`], so a page whose items are added here
//! is searched and counted alike.

use dbrover_store::CustomAction;

use super::keymap_page::Shortcut;
use super::setting::Setting;
use super::{Page, actions_page};

/// One thing a search can find, with the page it belongs to.
pub enum Found<'a> {
    Setting(Setting),
    Shortcut(Shortcut<'a>),
    Action(&'a CustomAction),
}

impl Found<'_> {
    pub fn page(&self) -> Page {
        match self {
            Self::Setting(setting) => Page::Settings(setting.category()),
            Self::Shortcut(_) => Page::Keymap,
            Self::Action(_) => Page::Actions,
        }
    }

    /// The caption of the card it is in, under its page's title.
    pub fn section(&self) -> &'static str {
        match self {
            Self::Setting(setting) => setting.section(),
            Self::Shortcut(shortcut) => shortcut.section.title(),
            Self::Action(_) => "Actions",
        }
    }

    pub fn matches(&self, query: &str) -> bool {
        match self {
            Self::Setting(setting) => setting.matches(query),
            Self::Shortcut(shortcut) => shortcut.matches(query),
            Self::Action(action) => actions_page::matches(action, query),
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
