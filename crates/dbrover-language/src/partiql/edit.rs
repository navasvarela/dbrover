//! The statements the grid's edits of a DynamoDB table's items become (`docs/nosql.md`, N5·7):
//! a changed attribute is an `UPDATE … SET`, a deleted item a `DELETE`, an added row an
//! `INSERT … VALUE`, each naming the item by its whole primary key, which DynamoDB requires
//! of a write. Values are written as PartiQL literals of the type the old value had, so a
//! number stays a number and a set a set.

use dbrover_document::{Path, Step, Value};

use super::{Verb, parse, quote};
use crate::DocumentEdit;

/// A `SELECT` on one table, whose items the grid can change a statement at a time once the
/// table's key is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableView {
    table: String,
    /// The primary key's attributes, partition first.
    key: Vec<String>,
}

impl TableView {
    /// The table `command` reads items from that can be edited: a `SELECT` of one table, not
    /// of an index, whose items may share a key.
    pub fn source(command: &str) -> Option<String> {
        let statement = parse(command).ok()?;
        (statement.verb == Verb::Select && statement.index.is_none()).then_some(statement.table)
    }

    /// The view of `table`'s items, keyed by `key`, partition first.
    pub fn new(table: String, key: Vec<String>) -> Self {
        Self { table, key }
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub(crate) fn is_key(&self, name: &str) -> bool {
        self.key.iter().any(|key| key == name)
    }

    pub(crate) fn delete(&self, document: &Value) -> Result<DocumentEdit, String> {
        let key = self.key_of(document)?;
        let revert = literal(document)
            .ok()
            .map(|item| format!("INSERT INTO {} VALUE {item}", quote(&self.table)));
        Ok(DocumentEdit {
            command: format!("DELETE FROM {}{}", quote(&self.table), where_clause(&key)),
            key,
            revert,
        })
    }

    pub(crate) fn update(
        &self,
        document: &Value,
        field: &Path,
        old: Option<&Value>,
        typed: Option<&str>,
    ) -> Result<(DocumentEdit, String), String> {
        let key = self.key_of(document)?;
        let names = field_names(field)?;
        if let [name] = names.as_slice()
            && self.is_key(name)
        {
            return Err(format!(
                "{name} is part of the item's key, which DynamoDB can't change; delete the item \
                 and add it again"
            ));
        }
        let path = names
            .iter()
            .map(|name| quote(name))
            .collect::<Vec<_>>()
            .join(".");
        let new = literal(&typed_value(old, typed)?)?;
        let where_clause = where_clause(&key);
        let table = quote(&self.table);
        let set = |value: &str| format!("UPDATE {table} SET {path} = {value}{where_clause}");
        let revert = match old {
            Some(old) => literal(old).ok().map(|old| set(&old)),
            None => Some(format!("UPDATE {table} REMOVE {path}{where_clause}")),
        };
        let edit = DocumentEdit {
            command: set(&new),
            key,
            revert,
        };
        Ok((edit, names.join(".")))
    }

    pub(crate) fn insert(&self, fields: &[(&str, Option<&str>)]) -> Result<String, String> {
        let mut written = Vec::new();
        for (name, value) in fields {
            if name.contains('.') {
                return Err(format!(
                    "type {name} in its whole map's column, which an added item needs"
                ));
            }
            let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
                continue;
            };
            let value = literal(&typed_value(None, Some(value))?)?;
            written.push(format!("{}: {value}", string(name)));
        }
        if let Some(missing) = self.key.iter().find(|key| {
            !fields
                .iter()
                .any(|(name, value)| name == key && value.is_some())
        }) {
            return Err(format!("type the item's {missing}, part of its key"));
        }
        Ok(format!(
            "INSERT INTO {} VALUE {{{}}}",
            quote(&self.table),
            written.join(", ")
        ))
    }

    /// Each key attribute of `document` and its value as a PartiQL literal.
    fn key_of(&self, document: &Value) -> Result<Vec<(String, String)>, String> {
        self.key
            .iter()
            .map(|name| {
                let value = field(document, name)
                    .ok_or_else(|| format!("this item has no {name} to find it by"))?;
                Ok((name.clone(), literal(value)?))
            })
            .collect()
    }
}

/// ` WHERE "pk" = 'a' AND "sk" = 1`.
fn where_clause(key: &[(String, String)]) -> String {
    let conditions: Vec<String> = key
        .iter()
        .map(|(name, value)| format!("{} = {value}", quote(name)))
        .collect();
    format!(" WHERE {}", conditions.join(" AND "))
}

fn field<'a>(document: &'a Value, name: &str) -> Option<&'a Value> {
    match document {
        Value::Object(fields) => fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value),
        _ => None,
    }
}

/// The names of an attribute's path, from the item's top.
fn field_names(path: &Path) -> Result<Vec<String>, String> {
    let names: Vec<String> = path
        .steps()
        .iter()
        .map(|step| match step {
            Step::Field(name) => Ok(name.clone()),
            Step::Index(_) => Err(
                "a list's items are edited by setting the whole list, or with UPDATE in a console"
                    .to_owned(),
            ),
        })
        .collect::<Result<_, _>>()?;
    match names.is_empty() {
        true => Err("this column isn't an attribute".to_owned()),
        false => Ok(names),
    }
}

/// `text` as a PartiQL string, a quote in it written twice.
fn string(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// `value`, a document's value as Extended JSON, as a PartiQL literal of the same type:
/// strings, numbers, booleans and `NULL`, maps as `{'k': v}`, lists as `[…]`, and DynamoDB's
/// string and number sets as `<<…>>`.
///
/// # Errors
///
/// A binary value, or a set of them: PartiQL has no literal for one, so DynamoDB takes them
/// only as a statement's parameters.
pub fn literal(value: &Value) -> Result<String, String> {
    match value {
        Value::Null => Ok("NULL".to_owned()),
        Value::Bool(flag) => Ok(flag.to_string()),
        Value::Number(number) => Ok(number.clone()),
        Value::String(text) => Ok(string(text)),
        Value::Array(items) => Ok(format!("[{}]", list(items)?)),
        Value::Object(fields) => match fields.as_slice() {
            [(set, Value::Array(items))] if set == "$stringSet" || set == "$numberSet" => {
                Ok(format!("<<{}>>", list(items)?))
            }
            [(binary, _)] if binary == "$binary" || binary == "$binarySet" => Err(
                "PartiQL can't write a binary value; change it with the AWS CLI or an SDK"
                    .to_owned(),
            ),
            fields => {
                let written = fields
                    .iter()
                    .map(|(name, value)| Ok(format!("{}: {}", string(name), literal(value)?)))
                    .collect::<Result<Vec<String>, String>>()?;
                Ok(format!("{{{}}}", written.join(", ")))
            }
        },
    }
}

fn list(items: &[Value]) -> Result<String, String> {
    Ok(items
        .iter()
        .map(literal)
        .collect::<Result<Vec<_>, _>>()?
        .join(", "))
}

/// What `text`, typed over an attribute that held `old`, becomes, keeping the old value's
/// type: over a string it is that text, over a number it must be one, over `true` or `false`
/// one of those, over a set a JSON list of its items' type. Over a map, a list, `NULL` or
/// nothing, text that is JSON is taken as written, and other text is a string. `None`, a cell
/// set to `NULL`, is `NULL`.
///
/// # Errors
///
/// Text that can't be a value of the old type, such as `abc` over a number.
pub fn typed_value(old: Option<&Value>, text: Option<&str>) -> Result<Value, String> {
    let Some(text) = text else {
        return Ok(Value::Null);
    };
    let written = text.trim();
    let parsed = Value::parse(written).ok();
    let wrong = |what: &str| Err(format!("\u{201c}{written}\u{201d} isn't {what}"));
    match old {
        Some(Value::String(_)) => Ok(Value::String(text.to_owned())),
        Some(Value::Number(_)) => match parsed {
            Some(number @ Value::Number(_)) => Ok(number),
            _ => wrong("a number"),
        },
        Some(Value::Bool(_)) => match parsed {
            Some(boolean @ Value::Bool(_)) => Ok(boolean),
            _ => wrong("true or false"),
        },
        Some(Value::Object(fields)) => match (fields.as_slice(), parsed) {
            ([(set, _)], Some(Value::Array(items))) if set == "$stringSet" => set_of(
                set,
                items,
                |item| matches!(item, Value::String(_)),
                "strings",
            ),
            ([(set, _)], Some(Value::Array(items))) if set == "$numberSet" => set_of(
                set,
                items,
                |item| matches!(item, Value::Number(_)),
                "numbers",
            ),
            ([(set, _)], _) if set == "$stringSet" || set == "$numberSet" => {
                wrong("a list of the set's items, such as [\"a\", \"b\"]")
            }
            (_, Some(parsed)) => Ok(parsed),
            (_, None) => wrong("JSON, as a map's value must be"),
        },
        Some(Value::Array(_)) => parsed.map_or_else(|| wrong("JSON, as a list must be"), Ok),
        Some(Value::Null) | None => Ok(parsed.unwrap_or_else(|| Value::String(text.to_owned()))),
    }
}

/// A set named `set` of `items`, which must all be what `is_item` accepts, and some: DynamoDB
/// has no empty set.
fn set_of(
    set: &str,
    items: Vec<Value>,
    is_item: impl Fn(&Value) -> bool,
    what: &str,
) -> Result<Value, String> {
    if items.is_empty() {
        return Err("DynamoDB has no empty set; remove the attribute instead".to_owned());
    }
    if !items.iter().all(is_item) {
        return Err(format!("a set of {what} holds only {what}"));
    }
    Ok(Value::Object(vec![(set.to_owned(), Value::Array(items))]))
}
