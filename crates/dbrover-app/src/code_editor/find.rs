//! Find and replace in an editor's text: what is searched for, the matches, and the text a
//! replacement makes. Every search is a regular expression underneath: plain text is escaped
//! first, so match case and whole word mean the same in both modes.

use std::ops::Range;

use regex::{NoExpand, Regex, RegexBuilder};

use super::{CodeEditor, ordered};

/// How the find bar's text is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FindOptions {
    pub match_case: bool,
    /// Only matches with no letter, digit or `_` right before or after them.
    pub whole_word: bool,
    /// The text is a regular expression, and `$1` in the replacement names its groups.
    pub regex: bool,
}

/// What the find bar searches for, compiled.
#[derive(Debug)]
pub struct Search {
    pattern: Regex,
    expands_groups: bool,
}

impl Search {
    /// `None` for empty text, which finds nothing; `Err` says why a regular expression is
    /// invalid.
    pub fn new(text: &str, options: FindOptions) -> Result<Option<Self>, String> {
        if text.is_empty() {
            return Ok(None);
        }
        let mut pattern = if options.regex {
            text.to_owned()
        } else {
            regex::escape(text)
        };
        if options.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        let pattern = RegexBuilder::new(&pattern)
            .case_insensitive(!options.match_case)
            .multi_line(true)
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Some(Self {
            pattern,
            expands_groups: options.regex,
        }))
    }

    /// Every match in `text`, in order. Empty matches (a regular expression like `x*`) are
    /// left out: there is nothing to select or replace in them.
    pub fn matches(&self, text: &str) -> Vec<Range<usize>> {
        self.pattern
            .find_iter(text)
            .map(|found| found.range())
            .filter(|range| !range.is_empty())
            .collect()
    }

    /// What replaces the match at `range` of `text`: `with` itself, or with `$1` and `${name}`
    /// expanded from the match's groups when searching with a regular expression.
    pub fn replacement(&self, text: &str, range: Range<usize>, with: &str) -> String {
        if !self.expands_groups {
            return with.to_owned();
        }
        let Some(captures) = self.pattern.captures_at(text, range.start) else {
            return with.to_owned();
        };
        let mut expanded = String::new();
        captures.expand(with, &mut expanded);
        expanded
    }

    /// `text` with every match replaced, and how many there were.
    pub fn replace_all(&self, text: &str, with: &str) -> (String, usize) {
        let count = self.matches(text).len();
        let replaced = if self.expands_groups {
            self.pattern.replace_all(text, with)
        } else {
            self.pattern.replace_all(text, NoExpand(with))
        };
        (replaced.into_owned(), count)
    }
}

/// The match to go to from the selection: the first one starting at or after `from` going
/// forward, or the last one ending at or before it going back, wrapping around. `None` if there
/// are no matches.
pub fn next_match(matches: &[Range<usize>], from: usize, forward: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let found = if forward {
        matches.iter().position(|range| range.start >= from)
    } else {
        matches.iter().rposition(|range| range.end <= from)
    };
    Some(found.unwrap_or(if forward { 0 } else { matches.len() - 1 }))
}

/// The find bar's state. Kept while the bar is closed, so it reopens with the last search.
#[derive(Debug, Default)]
pub struct FindBar {
    pub open: bool,
    /// The replace field and its buttons show too.
    pub replacing: bool,
    pub query: String,
    pub replacement: String,
    pub options: FindOptions,
    /// Set when the find field should take the keyboard focus on the next frame.
    pub focus: bool,
    /// How many matches Replace all replaced, until the search changes.
    pub replaced: Option<usize>,
}

/// What the find bar shows about its search.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub matches: Vec<Range<usize>>,
    /// Which match is selected in the editor, if one is.
    pub current: Option<usize>,
    /// Why the regular expression is invalid.
    pub error: Option<String>,
}

impl CodeEditor {
    pub fn find_bar(&self) -> &FindBar {
        &self.find
    }

    pub fn find_bar_mut(&mut self) -> &mut FindBar {
        &mut self.find
    }

    /// Opens the find bar, with the replace field if `replacing`, searching for the selected
    /// text if it is on one line.
    pub fn open_find(&mut self, replacing: bool) {
        let (start, end) = ordered(&self.selection);
        let selected = &self.text[start..end];
        if !selected.is_empty() && !selected.contains('\n') {
            selected.clone_into(&mut self.find.query);
        }
        self.find.replacing = replacing || (self.find.open && self.find.replacing);
        self.find.open = true;
        self.find.focus = true;
    }

    /// Closes the find bar and gives the editor back the keyboard focus, keeping the selection.
    pub fn close_find(&mut self) {
        self.find.open = false;
        let (start, end) = ordered(&self.selection);
        self.pending_cursor = Some(end);
        self.pending_selection = Some(start..end);
    }

    fn search(&self) -> Result<Option<Search>, String> {
        Search::new(&self.find.query, self.find.options)
    }

    /// The matches of the find bar's search, while it is open.
    pub fn found(&self) -> Found {
        if !self.find.open {
            return Found::default();
        }
        match self.search() {
            Ok(Some(search)) => {
                let matches = search.matches(&self.text);
                let (start, end) = ordered(&self.selection);
                let current = matches.iter().position(|range| *range == (start..end));
                Found {
                    matches,
                    current,
                    error: None,
                }
            }
            Ok(None) => Found::default(),
            Err(error) => Found {
                error: Some(error),
                ..Found::default()
            },
        }
    }

    /// Selects the next match after the selection, or the previous one before it, wrapping
    /// around, and scrolls to it without taking the keyboard focus from the find bar.
    pub fn find_next(&mut self, forward: bool) {
        let Found { matches, .. } = self.found();
        let (start, end) = ordered(&self.selection);
        let from = if forward { end } else { start };
        if let Some(index) = next_match(&matches, from, forward) {
            self.select_without_focus(matches[index].clone());
        }
    }

    /// Replaces the selected match and goes to the next one; with no match selected, only goes
    /// to the next one. One undo step takes the replacement back. Returns whether the text
    /// changed.
    pub fn replace_current(&mut self) -> bool {
        let Ok(Some(search)) = self.search() else {
            return false;
        };
        let Found {
            current, matches, ..
        } = self.found();
        let Some(range) = current.map(|index| matches[index].clone()) else {
            self.find_next(true);
            return false;
        };
        let with = search.replacement(&self.text, range.clone(), &self.find.replacement);
        let mut text = self.text.clone();
        text.replace_range(range.clone(), &with);
        self.replaced_text = Some(std::mem::replace(&mut self.text, text));
        let after = range.start + with.len();
        self.selection = after..after;
        self.find_next(true);
        if self.pending_selection.is_none() {
            self.pending_selection = Some(after..after);
        }
        true
    }

    /// Replaces every match, as one undo step. Returns how many there were.
    pub fn replace_all(&mut self) -> usize {
        let Ok(Some(search)) = self.search() else {
            return 0;
        };
        let (text, count) = search.replace_all(&self.text, &self.find.replacement);
        if count == 0 {
            return 0;
        }
        self.replaced_text = Some(std::mem::replace(&mut self.text, text));
        self.select_without_focus(0..0);
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found<'a>(text: &'a str, query: &str, options: FindOptions) -> Vec<&'a str> {
        let search = Search::new(query, options).unwrap().unwrap();
        search
            .matches(text)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn finds_by_case_whole_word_and_regular_expression() {
        let text = "SELECT id, order_id FROM orders WHERE id = 1; select ID";
        let plain = FindOptions::default();
        let cases = [
            ("id", plain, vec!["id", "id", "id", "ID"]),
            (
                "id",
                FindOptions {
                    match_case: true,
                    ..plain
                },
                vec!["id", "id", "id"],
            ),
            (
                "id",
                FindOptions {
                    whole_word: true,
                    ..plain
                },
                vec!["id", "id", "ID"],
            ),
            (
                r"order\w*",
                FindOptions {
                    regex: true,
                    ..plain
                },
                vec!["order_id", "orders"],
            ),
            ("(", plain, vec![]),
            ("1;", plain, vec!["1;"]),
        ];
        for (query, options, expected) in cases {
            assert_eq!(
                found(text, query, options),
                expected,
                "{query:?} {options:?}"
            );
        }
    }

    #[test]
    fn says_why_a_regular_expression_is_invalid_and_finds_nothing_for_nothing() {
        let regex = FindOptions {
            regex: true,
            ..FindOptions::default()
        };
        assert!(
            Search::new("(", regex)
                .unwrap_err()
                .contains("unclosed group")
        );
        assert!(Search::new("", regex).unwrap().is_none());
        assert!(found("aaa", "x*", regex).is_empty());
    }

    #[test]
    fn replaces_with_groups_only_for_regular_expressions() {
        let text = "a1 b2";
        let regex = Search::new(
            r"(\w)(\d)",
            FindOptions {
                regex: true,
                ..FindOptions::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(regex.replacement(text, 3..5, "$2$1"), "2b");
        assert_eq!(regex.replace_all(text, "$2$1"), ("1a 2b".to_owned(), 2));

        let plain = Search::new("a1", FindOptions::default()).unwrap().unwrap();
        assert_eq!(plain.replacement(text, 0..2, "$1"), "$1");
        assert_eq!(plain.replace_all(text, "$1"), ("$1 b2".to_owned(), 1));
    }

    #[test]
    fn goes_to_the_next_or_previous_match_wrapping_around() {
        let matches = [2..4, 10..12, 20..22];
        let cases = [
            (0, true, Some(0)),
            (4, true, Some(1)),
            (10, true, Some(1)),
            (21, true, Some(0)),
            (10, false, Some(0)),
            (12, false, Some(1)),
            (1, false, Some(2)),
        ];
        for (from, forward, expected) in cases {
            assert_eq!(
                next_match(&matches, from, forward),
                expected,
                "{from} {forward}"
            );
        }
        assert_eq!(next_match(&[], 0, true), None);
    }
}
