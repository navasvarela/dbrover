//! The requests the grid's edits of a search's hits become (`docs/nosql.md`, N3·6): a changed
//! field is `_update` with a partial document holding it, a deleted hit `DELETE` of its
//! document, an added row a new document; each refreshes the index, so the search read again
//! shows the change. A hit is found by its own `_index` and `_id`, so the hits of an alias or a
//! pattern are edited too.

use dbrover_document::{Path, Step, Value};

use super::{HIT_KEYS, Request, pages_hits, parse};
use crate::DocumentEdit;

/// Keys a hit's document may hold besides its source's fields (see the driver's hit
/// documents). Should a document hold one, its source can't be told from them, so a deletion
/// isn't given an undo.
const HIT_EXTRAS: &[&str] = &[
    "_explanation",
    "_ignored",
    "_ignored_field_values",
    "_nested",
    "_node",
    "_primary_term",
    "_seq_no",
    "_shard",
    "_source",
    "_version",
    "fields",
    "highlight",
    "inner_hits",
    "matched_queries",
];

/// A search whose hits are documents of the indices they came from, which the grid can change
/// a request at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexView {
    /// The one index or alias the search names, which an added document goes to unless its
    /// `_index` is typed; `None` for a search of every index or of several.
    target: Option<String>,
}

/// Where a hit's document is, as the requests for it name it.
struct Located {
    index: String,
    id: String,
    /// `routing=…` for a document indexed with its own routing, which every request for it
    /// must carry.
    routing: Option<String>,
}

impl Located {
    fn of(document: &Value) -> Result<Self, String> {
        let text = |name| match field(document, name) {
            Some(Value::String(text)) => Some(text.clone()),
            _ => None,
        };
        let index = text("_index").ok_or("this hit has no _index to find it by")?;
        let id = text("_id").ok_or("this hit has no _id to find it by")?;
        Ok(Self {
            index,
            id,
            routing: text("_routing"),
        })
    }

    /// `/<index>/<endpoint>/<id>?refresh=true`, with the routing when there is one.
    fn target(&self, endpoint: &str) -> String {
        let routing = self
            .routing
            .as_deref()
            .map(|routing| format!("&routing={}", encode(routing)))
            .unwrap_or_default();
        format!(
            "/{}/{endpoint}/{}?refresh=true{routing}",
            encode(&self.index),
            encode(&self.id)
        )
    }

    fn key(&self) -> Vec<(String, String)> {
        vec![
            ("_index".to_owned(), self.index.clone()),
            ("_id".to_owned(), self.id.clone()),
        ]
    }
}

impl IndexView {
    /// The view of `command`'s hits, when it is a search whose hits the grid shows as
    /// documents.
    pub fn of(command: &str) -> Option<Self> {
        let request = parse(command).ok()?;
        if !pages_hits(&request) {
            return None;
        }
        Some(Self {
            target: single_target(&request),
        })
    }

    /// The index or alias the search names, or `_all` for one of several.
    pub fn name(&self) -> &str {
        self.target.as_deref().unwrap_or("_all")
    }

    pub(crate) fn delete(&self, document: &Value) -> Result<DocumentEdit, String> {
        let located = Located::of(document)?;
        let revert = source_of(document)
            .map(|source| format!("PUT {}\n{}", located.target("_doc"), source.to_json()));
        Ok(DocumentEdit {
            command: format!("DELETE {}", located.target("_doc")),
            key: located.key(),
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
        let located = Located::of(document)?;
        let names = field_names(field)?;
        let (last, parents) = names.split_last().ok_or("this column isn't a field")?;
        let new = typed_value(old, typed)?;
        let target = located.target("_update");
        let partial = |value: Value| {
            let body = Value::Object(vec![("doc".to_owned(), nested(&names, value))]);
            format!("POST {target}\n{}", body.to_json())
        };
        let revert = match old {
            Some(old) => partial(old.clone()),
            None => format!("POST {target}\n{}", removal(parents, last).to_json()),
        };
        let edit = DocumentEdit {
            command: partial(new),
            key: located.key(),
            revert: Some(revert),
        };
        Ok((edit, names.join(".")))
    }

    pub(crate) fn insert(&self, fields: &[(&str, Option<&str>)]) -> Result<String, String> {
        let typed = |name| {
            fields
                .iter()
                .find(|(field, _)| *field == name)
                .and_then(|(_, value)| *value)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let index = typed("_index")
            .map(str::to_owned)
            .or_else(|| self.target.clone())
            .ok_or("type the index in _index: this search reads more than one")?;
        let mut document = Value::Object(Vec::new());
        for (name, value) in fields {
            if HIT_KEYS.contains(name) {
                continue;
            }
            let names: Vec<String> = name.split('.').map(str::to_owned).collect();
            merge(&mut document, &names, typed_value(None, *value)?);
        }
        if matches!(&document, Value::Object(fields) if fields.is_empty()) {
            return Err("type a field first".to_owned());
        }
        let target = match typed("_id") {
            Some(id) => format!("PUT /{}/_doc/{}?refresh=true", encode(&index), encode(id)),
            None => format!("POST /{}/_doc?refresh=true", encode(&index)),
        };
        Ok(format!("{target}\n{}", document.to_json()))
    }
}

/// The one index or alias `request` names before `_search`: none for `/_search`, a wildcard
/// or a list.
fn single_target(request: &Request) -> Option<String> {
    match request.segments().as_slice() {
        [index, "_search"] if !index.contains(['*', ',']) && *index != "_all" => {
            Some((*index).to_owned())
        }
        _ => None,
    }
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

/// The names of a field's path, from the document's top.
fn field_names(path: &Path) -> Result<Vec<String>, String> {
    let names: Vec<String> = path
        .steps()
        .iter()
        .map(|step| match step {
            Step::Field(name) => Ok(name.clone()),
            Step::Index(_) => Err(
                "an array's items are edited by setting the whole array, or with _update in a console"
                    .to_owned(),
            ),
        })
        .collect::<Result<_, _>>()?;
    match names.first() {
        Some(first) if HIT_KEYS.contains(&first.as_str()) => Err(format!(
            "{first} names the hit, and isn't one of its document's fields"
        )),
        None => Err("this column isn't a field".to_owned()),
        Some(_) => Ok(names),
    }
}

/// `value` at `names` in objects: `{"address": {"city": value}}`, the partial document
/// `_update` merges into the hit's.
fn nested(names: &[String], value: Value) -> Value {
    names.iter().rev().fold(value, |value, name| {
        Value::Object(vec![(name.clone(), value)])
    })
}

/// Sets `value` at `names` in `document`, an object, making the objects on the way.
fn merge(document: &mut Value, names: &[String], value: Value) {
    let Value::Object(fields) = document else {
        return;
    };
    let Some((name, rest)) = names.split_first() else {
        return;
    };
    if rest.is_empty() {
        fields.push((name.clone(), value));
        return;
    }
    let index = match fields.iter().position(|(field, _)| field == name) {
        Some(index) => index,
        None => {
            fields.push((name.clone(), Value::Object(Vec::new())));
            fields.len() - 1
        }
    };
    merge(&mut fields[index].1, rest, value);
}

/// The script that removes the field at `names` from the document, which undoes setting a
/// field it didn't have: a partial document can only add. The names go in as parameters, so
/// none is read as code.
fn removal(path: &[String], last: &str) -> Value {
    let steps: String = (0..path.len())
        .map(|step| format!("[params.p{step}]"))
        .collect();
    let mut params: Vec<(String, Value)> = path
        .iter()
        .enumerate()
        .map(|(step, name)| (format!("p{step}"), Value::String(name.clone())))
        .collect();
    params.push(("last".to_owned(), Value::String(last.to_owned())));
    let script = Value::Object(vec![
        (
            "source".to_owned(),
            Value::String(format!("ctx._source{steps}.remove(params.last)")),
        ),
        ("params".to_owned(), Value::Object(params)),
    ]);
    Value::Object(vec![("script".to_owned(), script)])
}

/// A hit's document as its source: its fields without the hit's own. `None` when the document
/// holds a key a hit may have besides its source, which would make the two impossible to tell
/// apart.
fn source_of(document: &Value) -> Option<Value> {
    let Value::Object(fields) = document else {
        return None;
    };
    let source: Vec<(String, Value)> = fields
        .iter()
        .filter(|(name, _)| !HIT_KEYS.contains(&name.as_str()) && name != "_routing")
        .cloned()
        .collect();
    let ambiguous = source
        .iter()
        .any(|(name, _)| HIT_EXTRAS.contains(&name.as_str()));
    (!ambiguous).then_some(Value::Object(source))
}

/// What `text`, typed over a field that held `old`, becomes, keeping the old value's JSON
/// type: over a string it is that text, over a number it must be one, over `true` or `false`
/// one of those. Over an object, an array, `null` or nothing, text that is JSON is taken as
/// written, and other text is a string. `None`, a cell set to `NULL`, is `null`.
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
        Some(Value::Object(_) | Value::Array(_)) => parsed.map_or_else(
            || wrong("JSON, as an object's or an array's value must be"),
            Ok,
        ),
        Some(Value::Null) | None => Ok(parsed.unwrap_or_else(|| Value::String(text.to_owned()))),
    }
}

/// `text` as one part of a path: every byte but the unreserved ones percent-encoded, so an id
/// with a slash, a space or a `?` stays one part.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}
