//! Where a value sits inside a document, written as JavaScript and the MongoDB shell write it:
//! `items[2].sku`.

use std::fmt;
use std::str::FromStr;

use crate::Value;
use crate::write::write_string;

/// One step down into a value: a field of an object, or an item of an array.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Step {
    Field(String),
    Index(usize),
}

/// The steps from a document's top to one of its values. Empty for the document itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Path(Vec<Step>);

impl Path {
    pub fn steps(&self) -> &[Step] {
        &self.0
    }

    /// This path, one step further down.
    pub fn child(&self, step: Step) -> Self {
        let mut steps = self.0.clone();
        steps.push(step);
        Self(steps)
    }
}

impl From<Vec<Step>> for Path {
    fn from(steps: Vec<Step>) -> Self {
        Self(steps)
    }
}

/// `address.city`, `items[2].sku`; a field whose name isn't an identifier is written in
/// brackets and quotes, `["first name"]`, so the path reads back the same way.
impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, step) in self.0.iter().enumerate() {
            match step {
                Step::Index(item) => write!(f, "[{item}]")?,
                Step::Field(name) if is_identifier(name) => {
                    if index > 0 {
                        f.write_str(".")?;
                    }
                    f.write_str(name)?;
                }
                Step::Field(name) => {
                    let mut quoted = String::new();
                    write_string(&mut quoted, name);
                    write!(f, "[{quoted}]")?;
                }
            }
        }
        Ok(())
    }
}

/// A path that isn't written as [`Path`]'s `Display` writes one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a path to a field, as address.city or items[2].sku")]
pub struct PathError(pub String);

/// Reads a path as [`Path`]'s `Display` writes it: `address.city`, `items[2].sku`,
/// `["first name"]`. Not the empty path: a path names a value inside a document.
impl FromStr for Path {
    type Err = PathError;

    fn from_str(text: &str) -> Result<Self, PathError> {
        let invalid = || PathError(text.to_owned());
        let mut steps = Vec::new();
        let mut rest = text;
        while !rest.is_empty() {
            if let Some(inside) = rest.strip_prefix('[') {
                let (step, after) = bracketed(inside).ok_or_else(invalid)?;
                steps.push(step);
                rest = after;
                continue;
            }
            let name = if steps.is_empty() {
                rest
            } else {
                rest.strip_prefix('.').ok_or_else(invalid)?
            };
            let end = name.find(['.', '[']).unwrap_or(name.len());
            if !is_identifier(&name[..end]) {
                return Err(invalid());
            }
            steps.push(Step::Field(name[..end].to_owned()));
            rest = &name[end..];
        }
        if steps.is_empty() {
            return Err(invalid());
        }
        Ok(Self(steps))
    }
}

/// The step inside `[…]`, an index or a quoted name, and what follows the `]`.
fn bracketed(inside: &str) -> Option<(Step, &str)> {
    if inside.starts_with('"') {
        let end = string_end(inside)?;
        let Ok(Value::String(name)) = Value::parse(&inside[..end]) else {
            return None;
        };
        let after = inside[end..].strip_prefix(']')?;
        return Some((Step::Field(name), after));
    }
    let (digits, after) = inside.split_once(']')?;
    let plain = !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit());
    plain
        .then(|| digits.parse().ok())
        .flatten()
        .map(|index| (Step::Index(index), after))
}

/// The length of the JSON string `text` starts with, quotes included.
fn string_end(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (offset, c) in text.char_indices().skip(1) {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(offset + 1),
            _ => {}
        }
    }
    None
}

/// A name JavaScript reads after a dot: a letter, `_` or `$`, then those or digits.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || matches!(first, '_' | '$'))
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str) -> Step {
        Step::Field(name.to_owned())
    }

    #[test]
    fn writes_paths_as_the_shell_does() {
        let cases = [
            (
                vec![field("items"), Step::Index(2), field("sku")],
                "items[2].sku",
            ),
            (vec![field("address"), field("city")], "address.city"),
            (vec![Step::Index(0), field("_id")], "[0]._id"),
            (
                vec![field("first name"), field("$oid")],
                r#"["first name"].$oid"#,
            ),
            (vec![field("a.b"), field("2x")], r#"["a.b"]["2x"]"#),
            (vec![field("say \"hi\"")], r#"["say \"hi\""]"#),
            (vec![], ""),
        ];
        for (steps, expected) in cases {
            assert_eq!(Path::from(steps.clone()).to_string(), expected);
            if !steps.is_empty() {
                assert_eq!(
                    expected.parse::<Path>(),
                    Ok(Path::from(steps)),
                    "{expected}"
                );
            }
        }
    }

    #[test]
    fn refuses_text_that_is_not_a_path() {
        for text in [
            "", ".a", "a.", "a..b", "a[", "a[x]", "a[-1]", "2x", "a b", r#"["a"#, "a]",
        ] {
            assert!(text.parse::<Path>().is_err(), "{text:?}");
        }
    }
}
