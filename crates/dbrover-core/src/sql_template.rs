//! SQL with placeholders, as `actions.toml` writes it: `SELECT count(*) FROM {qualified_name}`.
//!
//! A placeholder is never pasted in as text. Names become quoted identifiers and values string
//! literals (or `NULL`), in the dialect's [`Quoting`], so a table called `x; DROP TABLE y`
//! stays one name and a value full of quotes stays one value. See `docs/hackability.md`.

use std::fmt;

use crate::{Cell, Quoting, Row};

/// A statement with placeholders, checked when it is read so a typo shows up as a problem with
/// the file rather than when the action runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlTemplate {
    /// As written, for showing the action to the user.
    source: String,
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Part {
    Text(String),
    Placeholder(Placeholder),
}

/// What a placeholder stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placeholder {
    /// The object's name as SQL refers to it, schema-qualified where it has a schema.
    QualifiedName,
    Database,
    Schema,
    /// The table or view the object is, or belongs to.
    Table,
    /// The column the object is, or the first selected column in the grid; `default` where
    /// there is none.
    Column {
        default: Option<String>,
    },
    /// The data source's name in DbRover, as a string literal: it names nothing in the database.
    DataSource,
    /// The first selected cell.
    Value,
    /// Every selected cell, as a comma-separated list of literals.
    Values,
    /// The named column's value in the first selected row.
    Row(String),
    /// The server's host, for commands only, like the other data source fields: SQL already
    /// runs on that server.
    Host,
    Port,
    User,
    /// The database file, for SQLite and DuckDB.
    File,
}

impl Placeholder {
    /// Whether the placeholder takes its value from the grid's cells.
    pub fn needs_cells(&self) -> bool {
        matches!(self, Self::Value | Self::Values | Self::Row(_))
    }

    /// Whether only a command can use it: the data source's own settings.
    pub fn is_data_source_field(&self) -> bool {
        matches!(self, Self::Host | Self::Port | Self::User | Self::File)
    }
}

impl fmt::Display for Placeholder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QualifiedName => f.write_str("{qualified_name}"),
            Self::Database => f.write_str("{database}"),
            Self::Schema => f.write_str("{schema}"),
            Self::Table => f.write_str("{table}"),
            Self::Column { default: None } => f.write_str("{column}"),
            Self::Column {
                default: Some(default),
            } => write!(f, "{{column:{default}}}"),
            Self::DataSource => f.write_str("{data_source}"),
            Self::Value => f.write_str("{value}"),
            Self::Values => f.write_str("{values}"),
            Self::Row(column) => write!(f, "{{row.{column}}}"),
            Self::Host => f.write_str("{host}"),
            Self::Port => f.write_str("{port}"),
            Self::User => f.write_str("{user}"),
            Self::File => f.write_str("{file}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("{{{0}}} is not a placeholder")]
    Unknown(String),
    #[error("{{row}} needs a column, as {{row.id}}")]
    RowWithoutColumn,
    #[error("{{row.}} names no column")]
    EmptyRowColumn,
    #[error("{{column:}} has no default; write one, as {{column:created_at}}")]
    EmptyDefault,
    #[error("{{{0}:…}}: only {{column}} takes a default")]
    DefaultNotAllowed(String),
    #[error("{0} is only for commands: SQL already runs on the data source")]
    CommandOnly(Placeholder),
    #[error("{0} is only for SQL: a command gets the selection on stdin")]
    SqlOnly(Placeholder),
    #[error("the command is empty; its first item is the program to run")]
    EmptyCommand,
    #[error("the program, the command's first item, is empty")]
    EmptyProgram,
    #[error(
        "the program, the command's first item, can't have placeholders, so what runs never \
         depends on what was selected"
    )]
    PlaceholderInProgram,
    #[error(
        "{0} can't be in a password command, which can only have {{host}}, {{port}}, {{user}} \
         and {{database}}"
    )]
    NotForPasswords(Placeholder),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("nothing here fills {0}")]
    Missing(Placeholder),
    #[error("the result has no column {0:?} for {{row.{0}}}")]
    NoSuchColumn(String),
}

/// What fills a template's placeholders, for one statement. `None` where the thing that was
/// acted on has no such name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateValues {
    /// Already quoted, as [`CatalogPath::quoted_name`](crate::CatalogPath::quoted_name) writes
    /// it: a function's name carries its argument list, which can't be quoted as one name.
    pub qualified_name: Option<String>,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
    pub column: Option<String>,
    pub data_source: String,
    pub cells: Option<SelectedCells>,
}

/// The grid's cells an action acts on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectedCells {
    /// Every column of the result, by name, for `{row.<column>}`.
    pub columns: Vec<String>,
    /// The selected columns, as indices into `columns`, left to right.
    pub selected: Vec<usize>,
    /// The rows, top to bottom, with every column's value.
    pub rows: Vec<Row>,
}

impl SelectedCells {
    pub(crate) fn first(&self) -> Option<&Cell> {
        self.rows.first()?.get(*self.selected.first()?)
    }

    fn all(&self) -> impl Iterator<Item = &Cell> {
        self.rows
            .iter()
            .flat_map(|row| self.selected.iter().filter_map(|&column| row.get(column)))
    }

    pub(crate) fn in_first_row(&self, column: &str) -> Result<&Cell, RenderError> {
        let index = self
            .columns
            .iter()
            .position(|name| name == column)
            .ok_or_else(|| RenderError::NoSuchColumn(column.to_owned()))?;
        self.rows
            .first()
            .and_then(|row| row.get(index))
            .ok_or(RenderError::Missing(Placeholder::Row(column.to_owned())))
    }
}

impl SqlTemplate {
    /// Reads `text`. A brace that doesn't start a placeholder, as in a JSON literal, is kept as
    /// written; `{{` stands for a `{` that would.
    ///
    /// # Errors
    ///
    /// The first placeholder that is misspelt or malformed, or that only commands take.
    pub fn parse(text: &str) -> Result<Self, TemplateError> {
        let parts = parse_parts(text)?;
        let command_only = parts.iter().find_map(|part| match part {
            Part::Placeholder(placeholder) if placeholder.is_data_source_field() => {
                Some(placeholder.clone())
            }
            Part::Placeholder(_) | Part::Text(_) => None,
        });
        if let Some(placeholder) = command_only {
            return Err(TemplateError::CommandOnly(placeholder));
        }
        Ok(Self {
            source: text.to_owned(),
            parts,
        })
    }

    /// The template as written.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn placeholders(&self) -> impl Iterator<Item = &Placeholder> {
        self.parts.iter().filter_map(|part| match part {
            Part::Placeholder(placeholder) => Some(placeholder),
            Part::Text(_) => None,
        })
    }

    /// The statement with every placeholder filled in: names quoted as identifiers, values as
    /// literals, the data source's name as a string.
    ///
    /// # Errors
    ///
    /// [`RenderError`] for the first placeholder `values` has nothing for.
    pub fn render(&self, values: &TemplateValues, quoting: Quoting) -> Result<String, RenderError> {
        let mut sql = String::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => sql.push_str(text),
                Part::Placeholder(placeholder) => {
                    sql.push_str(&fill(placeholder, values, quoting)?);
                }
            }
        }
        Ok(sql)
    }
}

/// Splits `text` into literal text and placeholders. A brace that doesn't start a placeholder is
/// kept as written; `{{` stands for a `{` that would.
pub(crate) fn parse_parts(text: &str) -> Result<Vec<Part>, TemplateError> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('{') {
        literal.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("{{") {
            literal.push('{');
            rest = after;
        } else if let Some((written, after)) = placeholder_at(tail) {
            if !literal.is_empty() {
                parts.push(Part::Text(std::mem::take(&mut literal)));
            }
            parts.push(Part::Placeholder(classify(written)?));
            rest = after;
        } else {
            literal.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    literal.push_str(rest);
    if !literal.is_empty() {
        parts.push(Part::Text(literal));
    }
    Ok(parts)
}

/// The text inside a placeholder starting `tail`, and what follows it: `{` then a lowercase
/// name, optionally `.` or `:` and more on the same line, then `}`. `None` for any other brace.
fn placeholder_at(tail: &str) -> Option<(&str, &str)> {
    let inner = tail.strip_prefix('{')?;
    let name_len = inner
        .find(|c: char| !(c.is_ascii_lowercase() || c == '_'))
        .unwrap_or(inner.len());
    if name_len == 0 {
        return None;
    }
    let after_name = &inner[name_len..];
    let close = if after_name.starts_with('}') {
        name_len
    } else if after_name.starts_with(['.', ':']) {
        let end = after_name.find(['}', '{', '\n'])?;
        if !after_name[end..].starts_with('}') {
            return None;
        }
        name_len + end
    } else {
        return None;
    };
    Some((&inner[..close], &inner[close + 1..]))
}

fn classify(written: &str) -> Result<Placeholder, TemplateError> {
    let split = written.find(['.', ':']);
    let (name, separator, argument) = match split {
        Some(at) => (
            &written[..at],
            written[at..].chars().next(),
            &written[at + 1..],
        ),
        None => (written, None, ""),
    };
    let plain = match name {
        "qualified_name" => Some(Placeholder::QualifiedName),
        "database" => Some(Placeholder::Database),
        "schema" => Some(Placeholder::Schema),
        "table" => Some(Placeholder::Table),
        "column" => Some(Placeholder::Column { default: None }),
        "data_source" => Some(Placeholder::DataSource),
        "value" => Some(Placeholder::Value),
        "values" => Some(Placeholder::Values),
        "host" => Some(Placeholder::Host),
        "port" => Some(Placeholder::Port),
        "user" => Some(Placeholder::User),
        "file" => Some(Placeholder::File),
        _ => None,
    };
    match (name, separator, plain) {
        ("row", None, _) => Err(TemplateError::RowWithoutColumn),
        ("row", Some('.'), _) if argument.is_empty() => Err(TemplateError::EmptyRowColumn),
        ("row", Some('.'), _) => Ok(Placeholder::Row(argument.to_owned())),
        ("column", Some(':'), _) if argument.trim().is_empty() => Err(TemplateError::EmptyDefault),
        ("column", Some(':'), _) => Ok(Placeholder::Column {
            default: Some(argument.to_owned()),
        }),
        (_, None, Some(placeholder)) => Ok(placeholder),
        (_, Some(':'), Some(_)) => Err(TemplateError::DefaultNotAllowed(name.to_owned())),
        _ => Err(TemplateError::Unknown(written.to_owned())),
    }
}

fn fill(
    placeholder: &Placeholder,
    values: &TemplateValues,
    quoting: Quoting,
) -> Result<String, RenderError> {
    let missing = || RenderError::Missing(placeholder.clone());
    let name = |name: &Option<String>| {
        name.as_deref()
            .map(|name| quoting.identifier(name))
            .ok_or_else(missing)
    };
    let cells = || values.cells.as_ref().ok_or_else(missing);
    match placeholder {
        Placeholder::QualifiedName => values.qualified_name.clone().ok_or_else(missing),
        Placeholder::Database => name(&values.database),
        Placeholder::Schema => name(&values.schema),
        Placeholder::Table => name(&values.table),
        Placeholder::Column { default } => values
            .column
            .as_deref()
            .or(default.as_deref())
            .map(|column| quoting.identifier(column))
            .ok_or_else(missing),
        Placeholder::DataSource => Ok(quoting.string_literal(&values.data_source)),
        Placeholder::Value => cells()?
            .first()
            .map(|cell| literal(cell, quoting))
            .ok_or_else(missing),
        Placeholder::Values => {
            let all: Vec<String> = cells()?.all().map(|cell| literal(cell, quoting)).collect();
            if all.is_empty() {
                return Err(missing());
            }
            Ok(all.join(", "))
        }
        Placeholder::Row(column) => Ok(literal(cells()?.in_first_row(column)?, quoting)),
        Placeholder::Host | Placeholder::Port | Placeholder::User | Placeholder::File => {
            Err(missing())
        }
    }
}

fn literal(cell: &Cell, quoting: Quoting) -> String {
    match cell {
        None => "NULL".to_owned(),
        Some(text) => quoting.string_literal(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BareNames;

    const BACKTICKS: Quoting = Quoting {
        identifier_open: '`',
        identifier_close: '`',
        backslash_escapes: true,
        string_prefix: "",
        bare_names: BareNames::Lowercase,
    };

    const BRACKETS: Quoting = Quoting {
        identifier_open: '[',
        identifier_close: ']',
        backslash_escapes: false,
        string_prefix: "N",
        bare_names: BareNames::Lowercase,
    };

    fn render(sql: &str, values: &TemplateValues, quoting: Quoting) -> String {
        SqlTemplate::parse(sql)
            .unwrap()
            .render(values, quoting)
            .unwrap()
    }

    fn table(schema: &str, name: &str) -> TemplateValues {
        TemplateValues {
            qualified_name: Some(format!(
                "{}.{}",
                Quoting::STANDARD.identifier(schema),
                Quoting::STANDARD.identifier(name)
            )),
            schema: Some(schema.to_owned()),
            table: Some(name.to_owned()),
            data_source: "shop".to_owned(),
            ..TemplateValues::default()
        }
    }

    #[test]
    fn reads_every_placeholder() {
        let template = SqlTemplate::parse(
            "{qualified_name} {database} {schema} {table} {column} {column:created_at} \
             {data_source} {value} {values} {row.Order Id}",
        )
        .unwrap();
        let found: Vec<String> = template.placeholders().map(ToString::to_string).collect();
        assert_eq!(
            found,
            [
                "{qualified_name}",
                "{database}",
                "{schema}",
                "{table}",
                "{column}",
                "{column:created_at}",
                "{data_source}",
                "{value}",
                "{values}",
                "{row.Order Id}",
            ]
        );
    }

    #[test]
    fn reports_each_malformed_placeholder() {
        let cases = [
            ("SELECT {tabel}", "{tabel} is not a placeholder"),
            ("SELECT {table.x}", "{table.x} is not a placeholder"),
            ("SELECT {row}", "{row} needs a column, as {row.id}"),
            ("SELECT {row.}", "{row.} names no column"),
            (
                "SELECT {column:}",
                "{column:} has no default; write one, as {column:created_at}",
            ),
            (
                "SELECT {table:users}",
                "{table:…}: only {column} takes a default",
            ),
            (
                "SELECT {host}",
                "{host} is only for commands: SQL already runs on the data source",
            ),
        ];
        for (sql, message) in cases {
            let error = SqlTemplate::parse(sql).unwrap_err();
            assert_eq!(error.to_string(), message, "{sql}");
        }
    }

    #[test]
    fn leaves_other_braces_alone_and_reads_a_doubled_one_as_one() {
        let values = TemplateValues::default();
        let sql = r#"SELECT '{"a": {"b": 1}}', '{{table}', '}', '{ x }', '{A}'"#;
        assert_eq!(
            render(sql, &values, Quoting::STANDARD),
            r#"SELECT '{"a": {"b": 1}}', '{table}', '}', '{ x }', '{A}'"#
        );
        assert!(
            SqlTemplate::parse(sql)
                .unwrap()
                .placeholders()
                .next()
                .is_none()
        );
        assert!(
            SqlTemplate::parse("SELECT {row.id\n}")
                .unwrap()
                .placeholders()
                .next()
                .is_none(),
            "a placeholder ends on its own line"
        );
    }

    /// The hostile names from the hackability plan's exit criterion, in each kind of quoting.
    #[test]
    fn hostile_names_stay_one_quoted_name() {
        let hostile = "x\"; DROP TABLE y; --`]";
        let values = TemplateValues {
            schema: Some("it's".to_owned()),
            table: Some(hostile.to_owned()),
            column: Some(hostile.to_owned()),
            data_source: "o'brien\\".to_owned(),
            ..TemplateValues::default()
        };
        let sql = "SELECT {column} FROM {schema}.{table} WHERE {data_source} IS NOT NULL";
        assert_eq!(
            render(sql, &values, Quoting::STANDARD),
            "SELECT \"x\"\"; DROP TABLE y; --`]\" FROM \"it's\".\"x\"\"; DROP TABLE y; --`]\" \
             WHERE 'o''brien\\' IS NOT NULL"
        );
        assert_eq!(
            render(sql, &values, BACKTICKS),
            "SELECT `x\"; DROP TABLE y; --``]` FROM `it's`.`x\"; DROP TABLE y; --``]` \
             WHERE 'o''brien\\\\' IS NOT NULL"
        );
        assert_eq!(
            render(sql, &values, BRACKETS),
            "SELECT [x\"; DROP TABLE y; --`]]] FROM [it's].[x\"; DROP TABLE y; --`]]] \
             WHERE N'o''brien\\' IS NOT NULL"
        );
    }

    #[test]
    fn a_column_falls_back_on_its_default() {
        let sql = "SELECT * FROM {qualified_name} ORDER BY {column:created_at} DESC";
        let on_table = table("public", "orders");
        assert_eq!(
            render(sql, &on_table, Quoting::STANDARD),
            "SELECT * FROM \"public\".\"orders\" ORDER BY \"created_at\" DESC"
        );
        let on_column = TemplateValues {
            column: Some("placed at".to_owned()),
            ..on_table
        };
        assert_eq!(
            render(sql, &on_column, Quoting::STANDARD),
            "SELECT * FROM \"public\".\"orders\" ORDER BY \"placed at\" DESC"
        );
    }

    #[test]
    fn values_become_literals() {
        let values = TemplateValues {
            column: Some("id".to_owned()),
            cells: Some(SelectedCells {
                columns: vec!["id".to_owned(), "note".to_owned()],
                selected: vec![1],
                rows: vec![
                    vec![Some("7".to_owned()), Some("it's".to_owned())],
                    vec![Some("8".to_owned()), None],
                ],
            }),
            ..table("public", "orders")
        };
        assert_eq!(
            render(
                "SELECT {value}, {row.id} WHERE note IN ({values})",
                &values,
                Quoting::STANDARD
            ),
            "SELECT 'it''s', '7' WHERE note IN ('it''s', NULL)"
        );
    }

    #[test]
    fn says_what_nothing_fills() {
        let on_table = table("public", "orders");
        let cases = [
            ("SELECT {value}", "nothing here fills {value}"),
            ("SELECT {column}", "nothing here fills {column}"),
            ("USE {database}", "nothing here fills {database}"),
        ];
        for (sql, message) in cases {
            let error = SqlTemplate::parse(sql)
                .unwrap()
                .render(&on_table, Quoting::STANDARD)
                .unwrap_err();
            assert_eq!(error.to_string(), message, "{sql}");
        }
        let cells = TemplateValues {
            cells: Some(SelectedCells {
                columns: vec!["id".to_owned()],
                selected: vec![0],
                rows: vec![vec![Some("1".to_owned())]],
            }),
            ..on_table
        };
        let error = SqlTemplate::parse("SELECT {row.email}")
            .unwrap()
            .render(&cells, Quoting::STANDARD)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "the result has no column \"email\" for {row.email}"
        );
    }
}
