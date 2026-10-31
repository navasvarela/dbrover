//! Reading and building the servers' JSON with `dbrover-document`'s values, which keep every
//! number as written, so a hit's fields reach the grid as the server sent them.

use dbrover_core::Cell;
use dbrover_document::Value;

/// The field `name` of `value`, an object; `None` for another value or a missing field.
pub(crate) fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    match value {
        Value::Object(fields) => fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value),
        _ => None,
    }
}

/// The field at `path`, a field name at each level.
pub(crate) fn at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |value, name| field(value, name))
}

/// The text of the string field `name`.
pub(crate) fn text_of(value: &Value, name: &str) -> Option<String> {
    match field(value, name)? {
        Value::String(text) => Some(text.clone()),
        _ => None,
    }
}

/// `value` as a grid's cell: a string as itself, `null` as SQL's `NULL`, anything else as JSON.
pub(crate) fn cell(value: &Value) -> Cell {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_json()),
    }
}

/// `object` with `name` set to `value`: replaced where it is, or added at the end.
pub(crate) fn set(object: &mut Vec<(String, Value)>, name: &str, value: Value) {
    match object.iter_mut().find(|(field, _)| field == name) {
        Some((_, existing)) => *existing = value,
        None => object.push((name.to_owned(), value)),
    }
}

pub(crate) fn string(text: impl Into<String>) -> Value {
    Value::String(text.into())
}

pub(crate) fn object(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    )
}
