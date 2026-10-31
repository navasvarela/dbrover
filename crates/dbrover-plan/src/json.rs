//! Reading the values of the JSON plan formats.

use serde_json::{Map, Value};

pub(crate) type Object = Map<String, Value>;

pub(crate) fn text(object: &Object, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// A number, written as a number or, as MySQL writes its costs, as a string.
pub(crate) fn number(object: &Object, key: &str) -> Option<f64> {
    match object.get(key)? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse().ok(),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

pub(crate) fn flag(object: &Object, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// A key written as words: `attached_condition` as `Attached condition`.
pub(crate) fn label(key: &str) -> String {
    let words = key.replace('_', " ");
    let mut characters = words.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// A value as a fact shows it: text as it is, a list of plain values joined with commas, and
/// anything nested as compact JSON.
pub(crate) fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items)
            if items
                .iter()
                .all(|item| !item.is_object() && !item.is_array()) =>
        {
            items.iter().map(shown).collect::<Vec<_>>().join(", ")
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            value.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers_written_either_way_and_labels_keys_as_words() {
        let object: Object =
            serde_json::from_str(r#"{"a": 1.5, "b": "800.62", "c": "21K"}"#).unwrap_or_default();
        assert_eq!(number(&object, "a"), Some(1.5));
        assert_eq!(number(&object, "b"), Some(800.62));
        assert_eq!(number(&object, "c"), None);
        assert_eq!(label("attached_condition"), "Attached condition");
        assert_eq!(
            shown(&serde_json::json!(["id", "name"])),
            "id, name".to_owned()
        );
        assert_eq!(
            shown(&serde_json::json!({"pages_accessed": 5})),
            r#"{"pages_accessed":5}"#
        );
    }
}
