//! The words completion offers in a DynamoDB console, each with what it shows beside it, and
//! the ones the editor highlights.

/// Every word, sorted, with what completion shows beside it: a function's arguments, or
/// nothing for a keyword. Lowercase, since PartiQL's keywords ignore case; DynamoDB's
/// functions are written in lowercase too.
const WORDS: &[(&str, &str)] = &[
    ("all", ""),
    ("and", ""),
    ("asc", ""),
    ("attribute_type", "attribute_type(path, 'S')"),
    ("begins_with", "begins_with(path, 'prefix')"),
    ("between", ""),
    ("by", ""),
    ("contains", "contains(path, value)"),
    ("delete", ""),
    ("desc", ""),
    ("exists", "exists(path)"),
    ("false", ""),
    ("from", ""),
    ("in", ""),
    ("insert", ""),
    ("into", ""),
    ("is", ""),
    ("list_append", "list_append(list, list)"),
    ("missing", ""),
    ("modified", ""),
    ("new", ""),
    ("not", ""),
    ("null", ""),
    ("old", ""),
    ("or", ""),
    ("order", ""),
    ("remove", ""),
    ("returning", ""),
    ("select", ""),
    ("set", ""),
    ("set_add", "set_add(set, <<value>>)"),
    ("set_delete", "set_delete(set, <<value>>)"),
    ("size", "size(path)"),
    ("true", ""),
    ("update", ""),
    ("value", ""),
    ("where", ""),
];

const NAMES: [&str; WORDS.len()] = {
    let mut names = [""; WORDS.len()];
    let mut index = 0;
    while index < WORDS.len() {
        names[index] = WORDS[index].0;
        index += 1;
    }
    names
};

pub(crate) fn completion_words() -> &'static [&'static str] {
    &NAMES
}

/// What completion shows beside `word`: a function's arguments.
pub(crate) fn detail(word: &str) -> Option<&'static str> {
    WORDS
        .binary_search_by_key(&word, |(listed, _)| listed)
        .ok()
        .map(|index| WORDS[index].1)
        .filter(|detail| !detail.is_empty())
}

/// Whether `word` is one of PartiQL's keywords or DynamoDB's functions, in any case.
pub(crate) fn is_keyword(word: &str) -> bool {
    WORDS
        .binary_search_by_key(&word.to_ascii_lowercase().as_str(), |(listed, _)| listed)
        .is_ok()
}
