//! [`TemplateEngine`] with [MiniJinja](https://docs.rs/minijinja): the one place DbRover names
//! MiniJinja's types.
//!
//! An [`Environment`] holds the three parts as templates named `header`, `row` and `footer`.
//! It has no loader, and MiniJinja is built without `multi_template`, so a template can't
//! include, extend or import anything, and reads no files.

use std::io::{self, Write};
use std::sync::{Arc, OnceLock};

use dbrover_core::Cell;
use dbrover_sql::Dialect;
use minijinja::syntax::SyntaxConfig;
use minijinja::value::{Enumerator, Object, Value};
use minijinja::{AutoEscape, Environment, Error, ErrorKind, UndefinedBehavior};

use super::{
    Escape, Part, PartContext, TemplateContext, TemplateEngine, TemplateError, TemplateLimits,
    TemplateSource, kind_name,
};

/// Compiles `source`'s parts, so a mistake in any of them shows when the file is read.
///
/// # Errors
///
/// [`TemplateError`] naming the part and line of the first mistake.
pub fn check(source: &TemplateSource) -> Result<(), TemplateError> {
    compile(source, None, TemplateLimits::default()).map(drop)
}

/// `source`'s parts, ready to render rows from a data source of `dialect`, which the SQL
/// filters quote for.
pub(crate) fn compile(
    source: &TemplateSource,
    dialect: Option<Dialect>,
    limits: TemplateLimits,
) -> Result<Box<dyn TemplateEngine>, TemplateError> {
    let mut env = Environment::new();
    // Jinja drops a template's last line break, which would run every row into the next: a
    // row template ends with the line break that separates rows.
    let syntax = SyntaxConfig::builder()
        .keep_trailing_newline(true)
        .build()
        .map_err(|error| TemplateError(error.to_string()))?;
    env.set_syntax(syntax);
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_fuel(Some(limits.fuel));
    let escape = match source.escape {
        Escape::None => AutoEscape::None,
        Escape::Html => AutoEscape::Html,
    };
    env.set_auto_escape_callback(move |_| escape.clone());
    env.set_formatter(|out, state, value| {
        // `NULL`, which MiniJinja would write as `none`.
        if value.is_none() {
            return Ok(());
        }
        minijinja::escape_formatter(out, state, value)
    });
    add_filters(&mut env, dialect);
    for (part, text) in [
        (Part::Header, &source.header),
        (Part::Row, &source.row),
        (Part::Footer, &source.footer),
    ] {
        env.add_template_owned(part.name(), text.clone())
            .map_err(|error| problem(part, &error))?;
    }
    Ok(Box::new(Jinja {
        env,
        limits,
        shared: OnceLock::new(),
    }))
}

struct Jinja {
    env: Environment<'static>,
    limits: TemplateLimits,
    /// What every part sees, built on the first render: one engine renders one export, whose
    /// columns and context don't change between parts.
    shared: OnceLock<Shared>,
}

/// The values every part's context starts from.
struct Shared {
    columns: Value,
    data_source: Value,
    table: Value,
    parameters: Value,
    exported_at: Value,
    /// The keys `record` has, in column order, shared by every row's.
    keys: Arc<[String]>,
}

impl TemplateEngine for Jinja {
    fn render(
        &self,
        context: &TemplateContext<'_>,
        out: &mut dyn Write,
    ) -> Result<(), TemplateError> {
        let part = context.part.part();
        let template = self
            .env
            .get_template(part.name())
            .map_err(|error| problem(part, &error))?;
        let shared = self.shared.get_or_init(|| Shared::new(context));
        let mut limited = Limited {
            out,
            left: self.limits.max_bytes,
            exceeded: false,
        };
        match template.render_captured_to(shared.context(&context.part), &mut limited) {
            Ok(_) => Ok(()),
            Err(_) if limited.exceeded => Err(TemplateError(format!(
                "the {} template wrote more than {} MB for one {}, which is more than any \
                 export needs: does it repeat without end?",
                part.name(),
                self.limits.max_bytes / (1024 * 1024),
                part_unit(part)
            ))),
            Err(error) => Err(problem(part, &error)),
        }
    }
}

impl Shared {
    fn new(context: &TemplateContext<'_>) -> Self {
        let columns = context
            .columns
            .iter()
            .map(|column| {
                Value::from_pairs([
                    ("name", Value::from(column.name.as_str())),
                    (
                        "type",
                        column
                            .type_name
                            .as_deref()
                            .map_or(Value::from(()), Value::from),
                    ),
                    ("kind", Value::from(kind_name(column.kind))),
                ])
            })
            .collect();
        let export = context.export;
        let (names, values): (Vec<String>, Vec<Value>) = export
            .parameters
            .iter()
            .map(|(name, value)| (name.clone(), text(value)))
            .unzip();
        Self {
            columns,
            data_source: Value::from(export.data_source.as_str()),
            table: export.table.as_deref().map_or(Value::from(()), Value::from),
            parameters: Value::from_object(Record {
                keys: names.into(),
                values,
            }),
            exported_at: Value::from(export.exported_at.as_str()),
            keys: context.keys.into(),
        }
    }

    fn context(&self, part: &PartContext<'_>) -> Value {
        let mut pairs = vec![
            ("columns", self.columns.clone()),
            ("data_source", self.data_source.clone()),
            ("table", self.table.clone()),
            ("parameters", self.parameters.clone()),
            ("exported_at", self.exported_at.clone()),
        ];
        match part {
            PartContext::Header => {}
            PartContext::Row { values, index } => {
                let values: Vec<Value> = values.iter().map(text).collect();
                pairs.push(("row", Value::from(values.clone())));
                pairs.push((
                    "record",
                    Value::from_object(Record {
                        keys: self.keys.clone(),
                        values,
                    }),
                ));
                pairs.push(("index", Value::from(*index)));
            }
            PartContext::Footer { count } => pairs.push(("count", Value::from(*count))),
        }
        Value::from_pairs(pairs)
    }
}

/// A cell as a template sees it: its text, or `none` for `NULL`.
fn text(cell: &Cell) -> Value {
    cell.as_deref().map_or(Value::from(()), Value::from)
}

/// Values by name, in the order they were given: a row's `record`, a run's `parameters`.
#[derive(Debug)]
struct Record {
    keys: Arc<[String]>,
    values: Vec<Value>,
}

impl Object for Record {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let key = key.as_str()?;
        let index = self.keys.iter().position(|name| name == key)?;
        self.values.get(index).cloned()
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Values(
            self.keys
                .iter()
                .map(|key| Value::from(key.as_str()))
                .collect(),
        )
    }
}

/// The filters added for exports, beside MiniJinja's own.
fn add_filters(env: &mut Environment<'static>, dialect: Option<Dialect>) {
    env.add_filter("json", json);
    env.add_filter("csv", csv);
    env.add_filter("sql_literal", move |value: &Value| {
        let quoting = sql_dialect(dialect, "sql_literal")?.quoting();
        Ok::<_, Error>(match value.as_str() {
            _ if value.is_none() => "NULL".to_owned(),
            Some(text) => quoting.string_literal(text),
            None => quoting.string_literal(&value.to_string()),
        })
    });
    env.add_filter("sql_name", move |value: &Value| {
        let quoting = sql_dialect(dialect, "sql_name")?.quoting();
        Ok::<_, Error>(quoting.identifier(&value.to_string()))
    });
}

fn sql_dialect(dialect: Option<Dialect>, filter: &str) -> Result<Dialect, Error> {
    dialect.ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidOperation,
            format!("{filter} quotes for a SQL database, and these rows come from another kind"),
        )
    })
}

/// A JSON string, or `null` for `NULL`; numbers and booleans a template made stay as they are.
fn json(value: &Value) -> Result<String, Error> {
    let encoded = if value.is_none() {
        Ok("null".to_owned())
    } else if let Some(text) = value.as_str() {
        serde_json::to_string(text)
    } else if value.is_number() || value.kind() == minijinja::value::ValueKind::Bool {
        Ok(value.to_string())
    } else {
        serde_json::to_string(&value.to_string())
    };
    encoded.map_err(|error| Error::new(ErrorKind::InvalidOperation, error.to_string()))
}

/// A CSV field, quoted as RFC 4180 asks when it holds `separator` (a comma unless given), a
/// quote or a line break; empty for `NULL`.
fn csv(value: &Value, separator: Option<&str>) -> Result<String, Error> {
    if value.is_none() {
        return Ok(String::new());
    }
    let separator = separator.unwrap_or(",");
    let text = value.to_string();
    let needs_quotes =
        text.contains(separator) || text.contains(['"', '\r', '\n']) || separator.is_empty();
    Ok(if needs_quotes {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    })
}

/// A sentence for the user saying what went wrong where.
fn problem(part: Part, error: &Error) -> TemplateError {
    let what = match error.kind() {
        ErrorKind::OutOfFuel => format!(
            "it took too many steps for one {}: does a loop never end?",
            part_unit(part)
        ),
        ErrorKind::UndefinedError => format!(
            "{}; the names a template has are listed in the manual",
            described(error)
        ),
        _ => described(error),
    };
    match error.line() {
        Some(line) => TemplateError(format!("the {} template, line {line}: {what}", part.name())),
        None => TemplateError(format!("the {} template: {what}", part.name())),
    }
}

/// MiniJinja's own words for `error`: its kind, and the detail when it has one.
fn described(error: &Error) -> String {
    match error.detail() {
        Some(detail) => format!("{}: {detail}", error.kind()),
        None => error.kind().to_string(),
    }
}

/// What one render of `part` writes, for messages.
fn part_unit(part: Part) -> &'static str {
    match part {
        Part::Row => "row",
        Part::Header => "header",
        Part::Footer => "footer",
    }
}

/// A writer that refuses to write more than `left` bytes, so one render can't fill the disk.
struct Limited<'a> {
    out: &'a mut dyn Write,
    left: usize,
    exceeded: bool,
}

impl Write for Limited<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.left {
            self.exceeded = true;
            return Err(io::Error::other("the template wrote too much"));
        }
        let written = self.out.write(buf)?;
        self.left -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}
