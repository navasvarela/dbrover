//! Where a found row's values hold the searched text, to highlight it and cut long values
//! around it. Done again here on the text the database returned, so it is a guide, not the
//! search: a collation that folds accents finds `café` for `cafe`, which this doesn't
//! highlight, and the row still shows.

use dbrover_core::{CaseMatch, DataSearch, MatchKind};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};

/// How many characters of a long value show on each side of the match.
const AROUND: usize = 40;
/// Values longer than this, in characters, are cut.
const LONGEST: usize = 120;

/// The byte range of the first match in `value`, if it has one.
pub(super) fn find(value: &str, search: &DataSearch) -> Option<std::ops::Range<usize>> {
    let needle = search.value.text();
    match search.matching {
        MatchKind::WholeValue => {
            let equal = match search.case {
                CaseMatch::Ignore => value.to_lowercase() == needle.to_lowercase(),
                CaseMatch::Match => value == needle,
            };
            equal.then_some(0..value.len())
        }
        MatchKind::Contains => match search.case {
            CaseMatch::Match => value.find(needle).map(|start| start..start + needle.len()),
            CaseMatch::Ignore => find_ignoring_case(value, needle),
        },
    }
}

/// The first place `needle` is in `value`, ignoring case, as a byte range of `value`.
/// Lower-casing can change a character's length (`İ`), so each start is tried on the
/// original text rather than the offsets of a lowered copy trusted.
fn find_ignoring_case(value: &str, needle: &str) -> Option<std::ops::Range<usize>> {
    let lowered_needle = needle.to_lowercase();
    let needle_chars = needle.chars().count();
    value.char_indices().find_map(|(start, _)| {
        let end = value[start..]
            .char_indices()
            .nth(needle_chars)
            .map_or(value.len(), |(offset, _)| start + offset);
        (value[start..end].to_lowercase() == lowered_needle).then_some(start..end)
    })
}

/// `value` laid out with the match marked, cut to [`LONGEST`] characters around it.
pub(super) fn layout(
    value: &str,
    found: Option<std::ops::Range<usize>>,
    font: FontId,
    text: Color32,
    marked: Color32,
) -> LayoutJob {
    let (shown, found) = cut(value, found);
    let plain = TextFormat::simple(font.clone(), text);
    let mut job = LayoutJob::default();
    match found {
        Some(range) => {
            job.append(&shown[..range.start], 0.0, plain.clone());
            job.append(
                &shown[range.clone()],
                0.0,
                TextFormat {
                    background: marked,
                    ..plain.clone()
                },
            );
            job.append(&shown[range.end..], 0.0, plain);
        }
        None => job.append(&shown, 0.0, plain),
    }
    job
}

/// The value, or a window of it around the match with `…` where it was cut, and the match's
/// range in what is shown.
fn cut(
    value: &str,
    found: Option<std::ops::Range<usize>>,
) -> (String, Option<std::ops::Range<usize>>) {
    if value.chars().count() <= LONGEST {
        return (value.to_owned(), found);
    }
    let Some(found) = found else {
        let head: String = value.chars().take(LONGEST).collect();
        return (format!("{head}\u{2026}"), None);
    };
    let before: String = value[..found.start]
        .chars()
        .rev()
        .take(AROUND)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let after: String = value[found.end..].chars().take(AROUND).collect();
    let opens = if before.len() < found.start {
        "\u{2026}"
    } else {
        ""
    };
    let closes = if after.len() < value.len() - found.end {
        "\u{2026}"
    } else {
        ""
    };
    let start = opens.len() + before.len();
    let end = start + (found.end - found.start);
    (
        format!("{opens}{before}{}{after}{closes}", &value[found]),
        Some(start..end),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(text: &str, matching: MatchKind, case: CaseMatch) -> DataSearch {
        DataSearch::new(text, matching, case).unwrap()
    }

    #[test]
    fn finds_the_value_as_the_search_matches_it() {
        let ignoring = search("o'brien", MatchKind::Contains, CaseMatch::Ignore);
        assert_eq!(find("Kate O'Brien", &ignoring), Some(5..12));
        let matching = search("o'brien", MatchKind::Contains, CaseMatch::Match);
        assert_eq!(find("Kate O'Brien", &matching), None);
        let whole = search("kate", MatchKind::WholeValue, CaseMatch::Ignore);
        assert_eq!(find("KATE", &whole), Some(0..4));
        assert_eq!(find("Kate O'Brien", &whole), None);
        let accented = search("élan", MatchKind::Contains, CaseMatch::Ignore);
        assert_eq!(find("Un ÉLAN rare", &accented), Some(3..8));
    }

    #[test]
    fn cuts_a_long_value_around_its_match() {
        let value = format!("{}refund{}", "a".repeat(200), "b".repeat(200));
        let found = find(
            &value,
            &search("refund", MatchKind::Contains, CaseMatch::Ignore),
        );
        let (shown, range) = cut(&value, found);
        let range = range.unwrap();
        assert_eq!(&shown[range], "refund");
        assert!(
            shown.starts_with('\u{2026}') && shown.ends_with('\u{2026}'),
            "{shown}"
        );
        assert_eq!(shown.chars().count(), 2 + 2 * AROUND + 6);
    }
}
