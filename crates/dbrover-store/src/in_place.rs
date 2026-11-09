//! Writing a TOML file DbRover rewrites by editing the text it read, so comments, blank lines
//! and the order chosen by hand survive, and a save changes only the lines of what changed
//! (VC1c in `docs/version-control.md`).
//!
//! The outline: the values the file held and the values to write are both serialized the way a
//! whole file would be, so the same value always reads the same; the two are compared key by
//! key, table by table, and only what differs is changed in the file's own document. Arrays of
//! tables with an `id` (`[[data_source]]`, `[[project]]`) are compared entry by entry,
//! matched by id, so an entry keeps its place and its comments, a new one goes at the end, and
//! a removed one takes only its own lines.

use serde::Serialize;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

/// The key an entry of an array of tables is matched by.
const ENTRY_ID: &str = "id";

/// Why a file couldn't be edited in place.
#[derive(Debug, thiserror::Error)]
pub enum InPlaceError {
    #[error("could not write the values as TOML")]
    Serialize(#[from] toml::ser::Error),
    #[error("the file isn't TOML DbRover can edit")]
    Parse(#[from] toml_edit::TomlError),
}

/// The comment DbRover writes at the top of a file, and the ones earlier versions wrote, which
/// it replaces when it finds one of them there.
pub(crate) struct Header {
    pub current: &'static str,
    pub earlier: &'static [&'static str],
}

impl Header {
    pub(crate) const NONE: Self = Self {
        current: "",
        earlier: &[],
    };

    /// `text` with an earlier header of DbRover's replaced by the current one. Anything else
    /// at the top, such as a comment of the user's, is left as it is.
    fn renewed<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        self.earlier
            .iter()
            .find_map(|earlier| text.strip_prefix(earlier))
            .map_or(std::borrow::Cow::Borrowed(text), |rest| {
                std::borrow::Cow::Owned(format!("{}{rest}", self.current))
            })
    }
}

/// The text to write for `new`: `read` (the file's text, which held `old`) with only what
/// differs between `old` and `new` changed, or `header` and `new` written whole when there was
/// no file.
///
/// # Errors
///
/// [`InPlaceError`] if a value can't be written as TOML, or the file read isn't TOML (it was
/// read as `old`, so it is).
pub(crate) fn edit_in_place<T: Serialize>(
    read: Option<&str>,
    old: &T,
    new: &T,
    header: &Header,
) -> Result<String, InPlaceError> {
    let new_text = toml::to_string(new)?;
    let Some(read) = read else {
        return Ok(format!("{}{new_text}", header.current));
    };
    let mut file: DocumentMut = header.renewed(read).parse()?;
    let old: DocumentMut = toml::to_string(old)?.parse()?;
    let new: DocumentMut = new_text.parse()?;
    edit_table(file.as_table_mut(), old.as_table(), new.as_table());
    Ok(file.to_string())
}

/// Changes `file` where `new` differs from `old`, which `file` holds.
fn edit_table(file: &mut Table, old: &Table, new: &Table) {
    let removed: Vec<String> = old
        .iter()
        .map(|(key, _)| key)
        .filter(|key| !new.contains_key(key))
        .map(str::to_owned)
        .collect();
    for key in removed {
        file.remove(&key);
    }
    for (key, new_item) in new.iter() {
        let old_item = old.get(key);
        if old_item.is_some_and(|old_item| same(old_item, new_item)) {
            continue;
        }
        match (file.get_mut(key), old_item, new_item) {
            (Some(Item::Table(file_table)), Some(Item::Table(old_table)), Item::Table(table)) => {
                edit_table(file_table, old_table, table);
            }
            (
                Some(Item::ArrayOfTables(file_entries)),
                Some(Item::ArrayOfTables(old_entries)),
                Item::ArrayOfTables(entries),
            ) if keyed_by_id(old_entries) && keyed_by_id(entries) => {
                edit_entries(file_entries, old_entries, entries);
            }
            // A table the file leaves out, all defaults: only what changed is written in it.
            (None, Some(Item::Table(old_table)), Item::Table(table)) => {
                let mut added = Table::new();
                edit_table(&mut added, old_table, table);
                file.insert(key, Item::Table(added));
            }
            (Some(Item::Value(file_value)), _, Item::Value(value)) => {
                let decor = file_value.decor().clone();
                *file_value = value.clone();
                *file_value.decor_mut() = decor;
            }
            _ => {
                file.insert(key, unplaced(new_item.clone()));
            }
        }
    }
}

/// Changes the entries of `file` where `new`'s differ from `old`'s, matched by id: changed
/// entries are edited where they are, removed ones taken out, and new ones added at the end.
fn edit_entries(file: &mut ArrayOfTables, old: &ArrayOfTables, new: &ArrayOfTables) {
    let new_ids: Vec<Option<String>> = new.iter().map(entry_id).collect();
    file.retain(|entry| {
        let id = entry_id(entry);
        let was_written = old.iter().any(|old| entry_id(old) == id);
        !was_written || new_ids.contains(&id)
    });
    for entry in new {
        let id = entry_id(entry);
        let old_entry = old.iter().find(|old| entry_id(old) == id);
        let file_entry = file.iter_mut().find(|file| entry_id(file) == id);
        match (old_entry, file_entry) {
            (Some(old_entry), Some(file_entry)) => edit_table(file_entry, old_entry, entry),
            _ => file.push(unplaced_table(entry.clone())),
        }
    }
}

fn keyed_by_id(entries: &ArrayOfTables) -> bool {
    entries.iter().all(|entry| entry_id(entry).is_some())
}

fn entry_id(entry: &Table) -> Option<String> {
    entry.get(ENTRY_ID)?.as_str().map(str::to_owned)
}

/// Whether two items serialized the same way hold the same value.
fn same(old: &Item, new: &Item) -> bool {
    old.to_string() == new.to_string()
}

/// `item` with no place of its own in the document it came from, so it is written after
/// whatever precedes it in the document it goes into.
fn unplaced(item: Item) -> Item {
    match item {
        Item::Table(table) => Item::Table(unplaced_table(table)),
        Item::ArrayOfTables(entries) => {
            let mut unplaced_entries = ArrayOfTables::new();
            for entry in entries {
                unplaced_entries.push(unplaced_table(entry));
            }
            Item::ArrayOfTables(unplaced_entries)
        }
        other => other,
    }
}

fn unplaced_table(mut table: Table) -> Table {
    table.set_position(None);
    for (_, item) in table.iter_mut() {
        let placed = std::mem::take(item);
        *item = unplaced(placed);
    }
    table
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Clone, Serialize)]
    struct File {
        name: String,
        #[serde(rename = "entry")]
        entries: Vec<Entry>,
    }

    #[derive(Clone, Serialize)]
    struct Entry {
        id: String,
        value: u32,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        extra: BTreeMap<String, u32>,
    }

    fn entry(id: &str, value: u32) -> Entry {
        Entry {
            id: id.to_owned(),
            value,
            extra: BTreeMap::new(),
        }
    }

    const BY_HAND: &str = "\
# Mine.
name = \"files\"  # kept

# First.
[[entry]]
id = \"a\"
value = 1

# Second.
[[entry]]
id = \"b\"
value = 2
";

    /// What a case changes, and what it is called.
    type Case = (&'static str, fn(&mut File), String);

    fn edited(change: impl FnOnce(&mut File)) -> String {
        let old = File {
            name: "files".to_owned(),
            entries: vec![entry("a", 1), entry("b", 2)],
        };
        let mut new = old.clone();
        change(&mut new);
        edit_in_place(Some(BY_HAND), &old, &new, &Header::NONE).unwrap()
    }

    #[test]
    fn edits_only_what_changed_where_it_is() {
        let cases: [Case; 5] = [
            ("nothing", |_| {}, BY_HAND.to_owned()),
            (
                "a value",
                |file| file.name = "renamed".to_owned(),
                BY_HAND.replace("\"files\"  # kept", "\"renamed\"  # kept"),
            ),
            (
                "an entry",
                |file| file.entries[0].value = 10,
                BY_HAND.replace("value = 1\n", "value = 10\n"),
            ),
            (
                "a table in the first entry, placed with it",
                |file| {
                    file.entries[0].extra.insert("x".to_owned(), 3);
                },
                BY_HAND.replace("value = 1\n", "value = 1\n\n[entry.extra]\nx = 3\n"),
            ),
            (
                "an entry removed, with its own comment",
                |file| {
                    file.entries.remove(0);
                },
                BY_HAND.replace("# First.\n[[entry]]\nid = \"a\"\nvalue = 1\n\n", ""),
            ),
        ];
        for (what, change, expected) in cases {
            assert_eq!(edited(change), expected, "{what}");
        }
    }

    #[test]
    fn adds_new_entries_at_the_end() {
        let text = edited(|file| file.entries.push(entry("c", 3)));

        assert_eq!(
            text,
            format!("{BY_HAND}\n[[entry]]\nid = \"c\"\nvalue = 3\n")
        );
    }

    #[test]
    fn writes_a_new_file_whole_under_the_header() {
        let file = File {
            name: "files".to_owned(),
            entries: vec![entry("a", 1)],
        };
        let header = Header {
            current: "# Now.\n\n",
            earlier: &["# Before.\n\n"],
        };

        let new = edit_in_place(None, &file, &file, &header).unwrap();
        assert_eq!(
            new,
            "# Now.\n\nname = \"files\"\n\n[[entry]]\nid = \"a\"\nvalue = 1\n"
        );

        let renewed = edit_in_place(
            Some("# Before.\n\nname = \"files\"\n\n[[entry]]\nid = \"a\"\nvalue = 1\n"),
            &file,
            &file,
            &header,
        )
        .unwrap();
        assert_eq!(renewed, new);
    }
}
