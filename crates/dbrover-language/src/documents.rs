//! What the grid needs to edit a result of documents a command at a time: how each document
//! is found again, and the command each change becomes, in the language that read them
//! (`docs/nosql.md`, N2·6, N3·6 and N5·7).

use dbrover_document::{Path, Value};

use crate::{mongo, partiql, search};

/// Where the documents a command read can be changed: a MongoDB collection, by `_id`; the
/// indices a search's hits came from, each hit by its `_index` and `_id`; or a DynamoDB table,
/// each item by its primary key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentView {
    Collection(mongo::CollectionView),
    Index(search::IndexView),
    Table(partiql::TableView),
}

/// The command for one change, with what names the document it changes, for the history, and
/// the command that would undo it, when one can be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentEdit {
    pub command: String,
    /// Each field that names the document, and its value as the command writes it.
    pub key: Vec<(String, String)>,
    pub revert: Option<String>,
}

impl DocumentView {
    /// What the documents belong to, for the history: the collection, or the search's index.
    pub fn name(&self) -> &str {
        match self {
            Self::Collection(view) => view.collection(),
            Self::Index(view) => view.name(),
            Self::Table(view) => view.table(),
        }
    }

    /// Whether the column `name` names a loaded document rather than holding one of its
    /// fields, so it isn't typed over: `_id`, and a hit's `_index` and `_score` too.
    pub fn is_key(&self, name: &str) -> bool {
        match self {
            Self::Collection(_) => name == "_id",
            Self::Index(_) => search::HIT_KEYS.contains(&name),
            Self::Table(view) => view.is_key(name),
        }
    }

    /// Deletes `document`.
    ///
    /// # Errors
    ///
    /// The document can't be named: it has no `_id`, or no `_index`.
    pub fn delete(&self, document: &Value) -> Result<DocumentEdit, String> {
        match self {
            Self::Collection(view) => {
                let id = mongo::document_id(document)?;
                let revert = mongo::shell_value(document)
                    .ok()
                    .map(|whole| view.insert(&whole));
                Ok(DocumentEdit {
                    command: view.delete(&id),
                    key: vec![("_id".to_owned(), id)],
                    revert,
                })
            }
            Self::Index(view) => view.delete(document),
            Self::Table(view) => view.delete(document),
        }
    }

    /// Sets the field of `document` at `field`, which held `old`, to `typed`, read as the old
    /// value's type; `None` sets it to `null`. Returns the change and the field's name as the
    /// history shows it.
    ///
    /// # Errors
    ///
    /// The document can't be named, the field can't be reached, or the text can't be a value
    /// of the old value's type.
    pub fn update(
        &self,
        document: &Value,
        field: &Path,
        old: Option<&Value>,
        typed: Option<&str>,
    ) -> Result<(DocumentEdit, String), String> {
        match self {
            Self::Collection(view) => {
                let id = mongo::document_id(document)?;
                let path = mongo::update_path(field)?;
                let new = mongo::typed_value(old, typed)?;
                let revert = match old {
                    Some(old) => mongo::shell_value(old)
                        .ok()
                        .map(|old| view.update(&id, &path, Some(&old))),
                    None => Some(view.update(&id, &path, None)),
                };
                let edit = DocumentEdit {
                    command: view.update(&id, &path, Some(&new)),
                    key: vec![("_id".to_owned(), id)],
                    revert,
                };
                Ok((edit, path))
            }
            Self::Index(view) => view.update(document, field, old, typed),
            Self::Table(view) => view.update(document, field, old, typed),
        }
    }

    /// Adds a document of the `fields` typed into a new row, each by its column's name.
    ///
    /// # Errors
    ///
    /// Nothing was typed, a field's name or value can't be written, or there is no telling
    /// which index the document goes to.
    pub fn insert(&self, fields: &[(&str, Option<&str>)]) -> Result<String, String> {
        match self {
            Self::Collection(view) => {
                let mut written = Vec::new();
                for (name, value) in fields {
                    if name.contains('.') {
                        return Err(format!(
                            "type {name} in its whole object's column, which an added document needs"
                        ));
                    }
                    let field = if mongo::is_plain_name(name) {
                        (*name).to_owned()
                    } else {
                        mongo::quote(name)
                    };
                    written.push(format!("{field}: {}", mongo::typed_value(None, *value)?));
                }
                if written.is_empty() {
                    return Err("type a field first".to_owned());
                }
                Ok(view.insert(&format!("{{{}}}", written.join(", "))))
            }
            Self::Index(view) => view.insert(fields),
            Self::Table(view) => view.insert(fields),
        }
    }
}
