//! An output's file name, with parameters in it: `valuations-{since:%Y-%m}.xlsx`.

use std::collections::BTreeMap;

use dbrover_export::ColumnType;
use jiff::civil::{Date, DateTime};
use jiff::fmt::strtime;

/// Characters no file name may have on any of the systems DbRover runs on, so a report
/// written on macOS runs on Windows too: Windows refuses all of these, and `/` is a folder
/// everywhere.
const FORBIDDEN: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// A file name with `{name}` and `{name:%Y-%m}` in it, and `{{`, `}}` for braces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileName {
    text: String,
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    Param {
        name: String,
        /// strftime's codes, for a date or timestamp parameter.
        format: Option<String>,
    },
}

impl FileName {
    /// Reads `text`. Which parameters it names, and whether they take a format, is checked
    /// against the report's parameters by [`FileName::check`].
    ///
    /// # Errors
    ///
    /// What is wrong with the braces, as a sentence.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    literal.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    literal.push('}');
                }
                '{' => {
                    let mut inside = String::new();
                    let mut closed = false;
                    for c in chars.by_ref() {
                        if c == '}' {
                            closed = true;
                            break;
                        }
                        inside.push(c);
                    }
                    if !closed {
                        return Err("a { is never closed; write {{ for a brace".to_owned());
                    }
                    let (name, format) = match inside.split_once(':') {
                        Some((name, format)) => (name, Some(format.to_owned())),
                        None => (inside.as_str(), None),
                    };
                    if name.is_empty() {
                        return Err(
                            "{} must name a parameter, as {since} or {since:%Y-%m}; write {{ for \
                             a brace"
                                .to_owned(),
                        );
                    }
                    if !literal.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut literal)));
                    }
                    parts.push(Part::Param {
                        name: name.to_owned(),
                        format,
                    });
                }
                '}' => return Err("a } has no { before it; write }} for a brace".to_owned()),
                c => literal.push(c),
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Text(literal));
        }
        Ok(Self {
            text: text.to_owned(),
            parts,
        })
    }

    /// As the report file writes it.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The parameters it names, each once.
    pub fn params(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|part| match part {
            Part::Param { name, .. } => Some(name.as_str()),
            Part::Text(_) => None,
        })
    }

    /// Whether every parameter it names is one of `params`, and each format is one its
    /// parameter's type takes.
    ///
    /// # Errors
    ///
    /// The first problem, as a sentence.
    pub fn check(&self, params: &BTreeMap<&str, ColumnType>) -> Result<(), String> {
        for part in &self.parts {
            let Part::Param { name, format } = part else {
                continue;
            };
            let Some(&param_type) = params.get(name.as_str()) else {
                return Err(format!("{{{name}}} names no parameter of the report"));
            };
            if let Some(format) = format {
                format_value(param_type, format, sample(param_type))
                    .map_err(|problem| format!("{{{name}:{format}}}: {problem}"))?;
            }
        }
        Ok(())
    }

    /// The file name for a run, from its parameters' values in their plain form.
    ///
    /// # Errors
    ///
    /// Why the name can't be a file's: empty, or with a character a file name can't have.
    pub fn render(&self, values: &BTreeMap<&str, (ColumnType, &str)>) -> Result<String, String> {
        let mut name = String::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => name.push_str(text),
                Part::Param {
                    name: param,
                    format,
                } => {
                    let Some(&(param_type, value)) = values.get(param.as_str()) else {
                        return Err(format!("there is no value for {{{param}}}"));
                    };
                    match format {
                        Some(format) => name.push_str(&format_value(param_type, format, value)?),
                        None => name.push_str(value),
                    }
                }
            }
        }
        check_file_name(&name)?;
        Ok(name)
    }
}

/// A value of each type that takes a format, to try a format on before any run.
fn sample(param_type: ColumnType) -> &'static str {
    match param_type {
        ColumnType::Timestamp => "2026-01-31 12:00:00",
        _ => "2026-01-31",
    }
}

/// A date or timestamp `value`, in its plain form, formatted with strftime's codes.
fn format_value(param_type: ColumnType, format: &str, value: &str) -> Result<String, String> {
    let formatted = match param_type {
        ColumnType::Date => {
            let date: Date = value.parse().map_err(|error| format!("{error}"))?;
            strtime::format(format, date)
        }
        ColumnType::Timestamp => {
            let at: DateTime = value.parse().map_err(|error| format!("{error}"))?;
            strtime::format(format, at)
        }
        _ => return Err("only a date or timestamp parameter takes a format".to_owned()),
    };
    formatted.map_err(|error| format!("{format:?} is not a format strftime reads: {error}"))
}

/// Whether `name` can be a file's name in the output folder, on every system.
fn check_file_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name == "." || name == ".." {
        return Err(format!("{name:?} is not a file name"));
    }
    if let Some(c) = name
        .chars()
        .find(|c| FORBIDDEN.contains(c) || c.is_control())
    {
        return Err(format!(
            "the file name {name:?} has {c:?} in it, which a file name can't have on every \
             system; files go in the run's output folder"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types() -> BTreeMap<&'static str, ColumnType> {
        [
            ("since", ColumnType::Date),
            ("at", ColumnType::Timestamp),
            ("account", ColumnType::Text),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn renders_parameters_and_their_formats() {
        let values: BTreeMap<&str, (ColumnType, &str)> = [
            ("since", (ColumnType::Date, "2026-10-01")),
            ("at", (ColumnType::Timestamp, "2026-10-01 08:30:00")),
            ("account", (ColumnType::Text, "4000")),
        ]
        .into_iter()
        .collect();
        let cases = [
            ("valuations-{since:%Y-%m}.xlsx", "valuations-2026-10.xlsx"),
            ("tb-{account}-{since}.csv", "tb-4000-2026-10-01.csv"),
            ("at {at:%Hh%M}.csv", "at 08h30.csv"),
            ("{{literal}}.csv", "{literal}.csv"),
        ];
        for (text, rendered) in cases {
            let name = FileName::parse(text).unwrap();
            assert_eq!(name.check(&types()), Ok(()), "{text}");
            assert_eq!(name.render(&values).as_deref(), Ok(rendered), "{text}");
        }
        let with_a_slash: BTreeMap<&str, (ColumnType, &str)> =
            [("account", (ColumnType::Text, "a/b"))]
                .into_iter()
                .collect();
        assert!(
            FileName::parse("{account}.csv")
                .unwrap()
                .render(&with_a_slash)
                .unwrap_err()
                .contains("has '/' in it")
        );
    }

    #[test]
    fn refuses_braces_names_and_formats_it_cant_use() {
        for text in ["{}.csv", "a}.csv", "{since.csv", "{{a}.csv"] {
            assert!(FileName::parse(text).is_err(), "{text}");
        }
        let cases = [
            ("{until}.csv", "{until} names no parameter of the report"),
            (
                "{account:%Y}.csv",
                "{account:%Y}: only a date or timestamp parameter takes a format",
            ),
            (
                "{since:%Q}.csv",
                "{since:%Q}: \"%Q\" is not a format strftime reads",
            ),
        ];
        for (text, problem) in cases {
            let checked = FileName::parse(text).unwrap().check(&types());
            assert!(
                checked
                    .as_ref()
                    .is_err_and(|error| error.starts_with(problem)),
                "{text}: {checked:?}"
            );
        }
        assert!(
            FileName::parse("{at:%H:%M}.csv")
                .unwrap()
                .render(
                    &[("at", (ColumnType::Timestamp, "2026-10-01 08:30:00"))]
                        .into_iter()
                        .collect()
                )
                .is_err()
        );
    }
}
