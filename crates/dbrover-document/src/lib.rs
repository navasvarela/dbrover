//! Documents and JSON values as DbRover reads and shows them, with no engine in it: MongoDB's
//! documents, a SQL engine's JSON column and a Redis JSON string all go through here.
//!
//! A document travels as canonical Extended JSON text (ADR 0006). [`Value::parse`] reads it,
//! or any JSON, keeping each object's fields in the order written and each number as written,
//! and [`Value::shown`] says how a value reads: its type and its text, with Extended JSON's
//! types written the way the MongoDB shell writes them when the text is read as
//! [`Syntax::ExtendedJson`].

mod extended;
mod flatten;
mod path;
mod read;
mod write;

pub use extended::ValueType;
pub use flatten::{ColumnsChange, Document, FieldCell, Flattened, MAX_COLUMNS, UNREADABLE_COLUMN};
pub use path::{Path, PathError, Step};
pub use read::{MAX_DEPTH, ParseError};

/// A JSON value, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    /// The number's text as written, so `1.10` stays `1.10` and a 30-digit integer keeps every
    /// digit: DbRover shows numbers and never computes with them.
    Number(String),
    String(String),
    Array(Vec<Value>),
    /// Fields in the order written. A name written twice is kept twice, as the text has it.
    Object(Vec<(String, Value)>),
}

/// How to read a value's special objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// Plain JSON, from a SQL engine's JSON column or a Redis string: `{"$oid": "…"}` is an
    /// object with one field.
    Json,
    /// MongoDB's Extended JSON, for documents: `{"$oid": "…"}` is an object id, shown as
    /// `ObjectId("…")`.
    ExtendedJson,
}

/// A value as the tree and the grid show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub value_type: ValueType,
    /// A scalar's text, as the shell writes it (`ObjectId("…")`, a string without its
    /// quotes); an object's or array's size (`{3 fields}`, `[12 items]`).
    pub text: String,
}

impl Value {
    /// Reads `text`, which must hold one JSON value, with whitespace around it allowed.
    ///
    /// # Errors
    ///
    /// [`ParseError`] if the text isn't JSON, holds more than one value, or nests deeper than
    /// [`MAX_DEPTH`] levels.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        read::parse(text)
    }

    /// The value as compact JSON, numbers as written: what copying an object copies.
    pub fn to_json(&self) -> String {
        write::compact(self)
    }

    /// The value as JSON indented by two spaces, one field or item a line.
    pub fn to_pretty_json(&self) -> String {
        write::pretty(self)
    }

    /// The value's type and text, as the tree and the grid show them.
    pub fn shown(&self, syntax: Syntax) -> Shown {
        extended::shown(self, syntax)
    }

    /// The fields or items a tree unfolds this value into, each with its step from here: none
    /// for a scalar, nor for an Extended JSON special object, which shows as one value.
    pub fn children(&self, syntax: Syntax) -> Vec<(Step, &Value)> {
        if syntax == Syntax::ExtendedJson && extended::is_special(self) {
            return Vec::new();
        }
        match self {
            Self::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, item)| (Step::Index(index), item))
                .collect(),
            Self::Object(fields) => fields
                .iter()
                .map(|(name, value)| (Step::Field(name.clone()), value))
                .collect(),
            Self::Null | Self::Bool(_) | Self::Number(_) | Self::String(_) => Vec::new(),
        }
    }

    /// The value at `path` inside this one: a field of an object, an item of an array.
    pub fn at(&self, path: &Path) -> Option<&Value> {
        path.steps()
            .iter()
            .try_fold(self, |value, step| match (value, step) {
                (Self::Object(fields), Step::Field(name)) => fields
                    .iter()
                    .find(|(field, _)| field == name)
                    .map(|(_, value)| value),
                (Self::Array(items), Step::Index(index)) => items.get(*index),
                _ => None,
            })
    }

    /// The value itself, as plain text, when it is a scalar: a string without its quotes, a
    /// number's digits as written, `true` or `false`. Read as [`Syntax::ExtendedJson`], one of
    /// its special values is too: `{"$numberLong": "7"}` is `7`, an object id its hex, a UUID
    /// its usual form and a date its time in ISO 8601 and UTC, `2026-10-01T12:00:00.000Z`.
    /// `None` for `null`, an object, an array, and a special value with no plain form (a
    /// regular expression, a timestamp, a set).
    pub fn plain(&self, syntax: Syntax) -> Option<String> {
        extended::plain(self, syntax)
    }

    /// What copying the value gives: a scalar as shown, so an object id pastes into the shell
    /// as `ObjectId("…")`; an object or array as compact JSON.
    pub fn copied(&self, syntax: Syntax) -> String {
        if self.children(syntax).is_empty() && !self.is_empty_container() {
            self.shown(syntax).text
        } else {
            self.to_json()
        }
    }

    fn is_empty_container(&self) -> bool {
        match self {
            Self::Array(items) => items.is_empty(),
            Self::Object(fields) => fields.is_empty(),
            Self::Null | Self::Bool(_) | Self::Number(_) | Self::String(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDER: &str = r#"{"_id": {"$oid": "5f1d7f0e2b5c3a0001a1b2c3"}, "total": {"$numberLong": "7"}, "items": [{"sku": "a-1"}], "note": null, "tags": []}"#;

    #[test]
    fn unfolds_objects_and_arrays_but_not_extended_json_values() {
        let order = Value::parse(ORDER).unwrap();
        let steps = |value: &Value, syntax| -> Vec<Step> {
            value
                .children(syntax)
                .into_iter()
                .map(|(step, _)| step)
                .collect()
        };
        let fields: Vec<Step> = ["_id", "total", "items", "note", "tags"]
            .map(|name| Step::Field(name.to_owned()))
            .into();
        assert_eq!(steps(&order, Syntax::ExtendedJson), fields);
        let id = &order.children(Syntax::ExtendedJson)[0].1;
        assert_eq!(steps(id, Syntax::ExtendedJson), []);
        assert_eq!(steps(id, Syntax::Json), [Step::Field("$oid".to_owned())]);
        let items = &order.children(Syntax::Json)[2].1;
        assert_eq!(steps(items, Syntax::Json), [Step::Index(0)]);
    }

    #[test]
    fn copies_scalars_as_shown_and_containers_as_json() {
        let order = Value::parse(ORDER).unwrap();
        let copied: Vec<String> = order
            .children(Syntax::ExtendedJson)
            .into_iter()
            .map(|(_, value)| value.copied(Syntax::ExtendedJson))
            .collect();
        assert_eq!(
            copied,
            [
                r#"ObjectId("5f1d7f0e2b5c3a0001a1b2c3")"#,
                "NumberLong(7)",
                r#"[{"sku":"a-1"}]"#,
                "null",
                "[]",
            ]
        );
        assert_eq!(
            Value::parse(r#""plain""#).unwrap().copied(Syntax::Json),
            "plain"
        );
    }

    #[test]
    fn finds_a_value_by_its_path() {
        let order = Value::parse(ORDER).unwrap();
        let at = |path: &str| order.at(&path.parse().unwrap()).cloned();
        assert_eq!(at("items[0].sku"), Some(Value::String("a-1".to_owned())));
        assert_eq!(at("note"), Some(Value::Null));
        assert_eq!(at("items[1].sku"), None);
        assert_eq!(at("total.x"), None);
        assert_eq!(at("missing"), None);
    }

    #[test]
    fn reads_a_scalar_as_its_plain_value() {
        let cases = [
            (r#""text""#, Some("text")),
            ("12.50", Some("12.50")),
            ("true", Some("true")),
            ("null", None),
            ("[1]", None),
            (r#"{"a": 1}"#, None),
            (
                r#"{"$oid": "5f1d7f0e2b5c3a0001a1b2c3"}"#,
                Some("5f1d7f0e2b5c3a0001a1b2c3"),
            ),
            (r#"{"$numberLong": "7"}"#, Some("7")),
            (r#"{"$numberInt": "-3"}"#, Some("-3")),
            (r#"{"$numberDouble": "1.5"}"#, Some("1.5")),
            (r#"{"$numberDecimal": "0.10"}"#, Some("0.10")),
            (
                r#"{"$date": {"$numberLong": "1790856000000"}}"#,
                Some("2026-10-01T12:00:00.000Z"),
            ),
            (
                r#"{"$date": "2026-10-01T12:00:00Z"}"#,
                Some("2026-10-01T12:00:00Z"),
            ),
            (
                r#"{"$binary": {"base64": "AAECAwQFBgcICQoLDA0ODw==", "subType": "04"}}"#,
                Some("00010203-0405-0607-0809-0a0b0c0d0e0f"),
            ),
            (r#"{"$timestamp": {"t": 1, "i": 2}}"#, None),
            (r#"{"$stringSet": ["a"]}"#, None),
        ];
        for (text, expected) in cases {
            let value = Value::parse(text).unwrap();
            assert_eq!(
                value.plain(Syntax::ExtendedJson).as_deref(),
                expected,
                "{text}"
            );
        }
        let id = Value::parse(r#"{"$oid": "5f1d"}"#).unwrap();
        assert_eq!(id.plain(Syntax::Json), None);
    }
}
