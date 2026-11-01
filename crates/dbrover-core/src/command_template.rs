//! A program and its arguments with placeholders, as a command action in `actions.toml` writes
//! them: `["pg_dump", "--table", "{qualified_name}", "{database}"]`.
//!
//! There is no shell. Each argument reaches the program as one argument, so a placeholder fills
//! in raw text, with nothing quoted, and a name full of spaces, quotes and semicolons still can't
//! become a second argument or a second command. See `docs/hackability.md`.

use crate::sql_template::{Part, parse_parts};
use crate::{
    Cell, ConnectionConfig, DuckDbDatabase, Placeholder, RenderError, TemplateError, TemplateValues,
};

/// A command, checked when it is read so a typo shows up as a problem with the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandTemplate {
    /// As written, for showing the command to the user and for recognising it once trusted.
    source: Vec<String>,
    /// Every argument after the program, which takes no placeholders.
    arguments: Vec<Vec<Part>>,
}

/// The data source's own settings a command may name, which SQL has no use for: the server it
/// connects to, or its file. Never a password: the settings hold none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataSourceFields {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    /// The database connected to, where the settings name one.
    pub database: Option<String>,
    /// SQLite's or DuckDB's file. `None` for a path that isn't valid UTF-8, which can't be
    /// written into an argument.
    pub file: Option<String>,
}

impl From<&ConnectionConfig> for DataSourceFields {
    /// Reads the settings as they are: resolve their `${NAME}` variables first.
    fn from(config: &ConnectionConfig) -> Self {
        let file = match config {
            ConnectionConfig::Sqlite(sqlite) => Some(&sqlite.path),
            ConnectionConfig::DuckDb(duckdb) => match &duckdb.database {
                DuckDbDatabase::File { path, .. } => Some(path),
                DuckDbDatabase::InMemory => None,
            },
            ConnectionConfig::Postgres(_)
            | ConnectionConfig::Mysql(_)
            | ConnectionConfig::SqlServer(_)
            | ConnectionConfig::ClickHouse(_)
            | ConnectionConfig::Redis(_)
            | ConnectionConfig::Mongo(_)
            | ConnectionConfig::Elasticsearch(_)
            | ConnectionConfig::Cassandra(_) => None,
        };
        let server = config.server();
        Self {
            host: server.map(|server| server.host.clone()),
            port: server.and_then(|server| server.port.number()),
            user: server.map(|server| server.user.clone()),
            database: server.map(|server| server.database.clone()),
            file: file.and_then(|path| path.to_str()).map(str::to_owned),
        }
    }
}

impl CommandTemplate {
    /// Reads a command: the program, then its arguments.
    ///
    /// # Errors
    ///
    /// An empty command or program, a program with placeholders, or the first placeholder that
    /// is misspelt, malformed, or only for SQL.
    pub fn parse(command: &[String]) -> Result<Self, TemplateError> {
        let (program, arguments) = command.split_first().ok_or(TemplateError::EmptyCommand)?;
        if program.trim().is_empty() {
            return Err(TemplateError::EmptyProgram);
        }
        let program_parts = parse_parts(program)?;
        if program_parts
            .iter()
            .any(|part| matches!(part, Part::Placeholder(_)))
        {
            return Err(TemplateError::PlaceholderInProgram);
        }
        let arguments = arguments
            .iter()
            .map(|argument| parse_argument(argument))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            source: command.to_vec(),
            arguments,
        })
    }

    /// The command as written.
    pub fn source(&self) -> &[String] {
        &self.source
    }

    /// The program to run, as written: a name looked up on `PATH`, or a path.
    pub fn program(&self) -> &str {
        self.source.first().map_or("", String::as_str)
    }

    pub fn placeholders(&self) -> impl Iterator<Item = &Placeholder> {
        self.arguments
            .iter()
            .flatten()
            .filter_map(|part| match part {
                Part::Placeholder(placeholder) => Some(placeholder),
                Part::Text(_) => None,
            })
    }

    /// The arguments after the program, each placeholder filled in as raw text.
    ///
    /// # Errors
    ///
    /// [`RenderError`] for the first placeholder nothing fills.
    pub fn render(
        &self,
        values: &TemplateValues,
        fields: &DataSourceFields,
    ) -> Result<Vec<String>, RenderError> {
        self.arguments
            .iter()
            .map(|parts| {
                let mut argument = String::new();
                for part in parts {
                    match part {
                        Part::Text(text) => argument.push_str(text),
                        Part::Placeholder(placeholder) => {
                            argument.push_str(&fill(placeholder, values, fields)?);
                        }
                    }
                }
                Ok(argument)
            })
            .collect()
    }
}

fn parse_argument(argument: &str) -> Result<Vec<Part>, TemplateError> {
    let parts = parse_parts(argument)?;
    let sql_only = parts.iter().find_map(|part| match part {
        Part::Placeholder(placeholder @ Placeholder::Values) => Some(placeholder.clone()),
        Part::Placeholder(_) | Part::Text(_) => None,
    });
    match sql_only {
        Some(placeholder) => Err(TemplateError::SqlOnly(placeholder)),
        None => Ok(parts),
    }
}

fn fill(
    placeholder: &Placeholder,
    values: &TemplateValues,
    fields: &DataSourceFields,
) -> Result<String, RenderError> {
    let missing = || RenderError::Missing(placeholder.clone());
    let text = |value: Option<&String>| value.cloned().ok_or_else(missing);
    let cells = || values.cells.as_ref().ok_or_else(missing);
    match placeholder {
        Placeholder::QualifiedName => text(values.qualified_name.as_ref()),
        Placeholder::Database => text(values.database.as_ref().or(fields.database.as_ref())),
        Placeholder::Schema => text(values.schema.as_ref()),
        Placeholder::Table => text(values.table.as_ref()),
        Placeholder::Column { default } => text(values.column.as_ref().or(default.as_ref())),
        Placeholder::DataSource => Ok(values.data_source.clone()),
        Placeholder::Value => cells()?.first().map(raw).ok_or_else(missing),
        Placeholder::Row(column) => Ok(raw(cells()?.in_first_row(column)?)),
        Placeholder::Host => text(fields.host.as_ref()),
        Placeholder::Port => fields.port.map(|port| port.to_string()).ok_or_else(missing),
        Placeholder::User => text(fields.user.as_ref()),
        Placeholder::File => text(fields.file.as_ref()),
        // Refused when the command is read.
        Placeholder::Values => Err(missing()),
    }
}

/// A cell as an argument: its text, or nothing for `NULL`, as a program reads an empty value.
fn raw(cell: &Cell) -> String {
    cell.clone().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{Port, PostgresConfig, SelectedCells, ServerConfig, SqliteConfig, TlsMode};

    fn command(items: &[&str]) -> Result<CommandTemplate, TemplateError> {
        let items: Vec<String> = items.iter().map(|&item| item.to_owned()).collect();
        CommandTemplate::parse(&items)
    }

    fn postgres() -> DataSourceFields {
        DataSourceFields::from(&ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                host: "db.example.com".to_owned(),
                port: Port::Number(5433),
                database: "shop".to_owned(),
                user: "ada".to_owned(),
                tls: TlsMode::default(),
                ssh: None,
                password_command: None,
            },
        }))
    }

    #[test]
    fn reads_the_plans_examples() {
        let jq = command(&["jq", "-r", ".[] | .email"]).unwrap();
        assert_eq!(jq.program(), "jq");
        assert!(
            jq.placeholders().next().is_none(),
            "jq's braces are its own"
        );

        let pgcli = command(&[
            "open",
            "-a",
            "Terminal",
            "--args",
            "pgcli",
            "postgres://{user}@{host}:{port}/{database}",
        ])
        .unwrap();
        let found: Vec<String> = pgcli.placeholders().map(ToString::to_string).collect();
        assert_eq!(found, ["{user}", "{host}", "{port}", "{database}"]);
        assert_eq!(
            pgcli
                .render(&TemplateValues::default(), &postgres())
                .unwrap()
                .last()
                .map(String::as_str),
            Some("postgres://ada@db.example.com:5433/shop")
        );
    }

    #[test]
    fn reports_each_command_it_cant_run() {
        let cases: [(&[&str], &str); 5] = [
            (
                &[],
                "the command is empty; its first item is the program to run",
            ),
            (&[" "], "the program, the command's first item, is empty"),
            (
                &["{table}-tool"],
                "the program, the command's first item, can't have placeholders, so what runs \
                 never depends on what was selected",
            ),
            (
                &["grep", "{values}"],
                "{values} is only for SQL: a command gets the selection on stdin",
            ),
            (&["echo", "{tabel}"], "{tabel} is not a placeholder"),
        ];
        for (items, message) in cases {
            assert_eq!(
                command(items).unwrap_err().to_string(),
                message,
                "{items:?}"
            );
        }
    }

    /// The hostile name from the hackability plan's exit criterion: a quote, a space and a
    /// semicolon, which a shell would split or end the command at.
    #[test]
    fn a_hostile_name_stays_one_argument_as_written() {
        let hostile = "it's; DROP TABLE y; --";
        let values = TemplateValues {
            qualified_name: Some(format!("\"main\".\"{hostile}\"")),
            schema: Some("main".to_owned()),
            table: Some(hostile.to_owned()),
            data_source: "o'brien's shop".to_owned(),
            ..TemplateValues::default()
        };
        let template = command(&[
            "tool",
            "{table}",
            "--name={qualified_name}",
            "{data_source}",
        ])
        .unwrap();

        let arguments = template
            .render(&values, &DataSourceFields::default())
            .unwrap();

        assert_eq!(
            arguments,
            [
                hostile.to_owned(),
                format!("--name=\"main\".\"{hostile}\""),
                "o'brien's shop".to_owned(),
            ]
        );
    }

    #[test]
    fn fills_cells_as_their_text_and_null_as_nothing() {
        let values = TemplateValues {
            cells: Some(SelectedCells {
                columns: vec!["id".to_owned(), "email".to_owned()],
                selected: vec![1],
                rows: vec![vec![Some("7".to_owned()), None]],
            }),
            ..TemplateValues::default()
        };
        let template = command(&["mail", "{value}", "{row.id}"]).unwrap();
        assert_eq!(
            template
                .render(&values, &DataSourceFields::default())
                .unwrap(),
            ["", "7"]
        );
    }

    #[test]
    fn the_nodes_database_wins_over_the_one_connected_to() {
        let template = command(&["dump", "{database}"]).unwrap();
        let on_node = TemplateValues {
            database: Some("archive".to_owned()),
            ..TemplateValues::default()
        };
        assert_eq!(template.render(&on_node, &postgres()).unwrap(), ["archive"]);
        assert_eq!(
            template
                .render(&TemplateValues::default(), &postgres())
                .unwrap(),
            ["shop"]
        );
    }

    #[test]
    fn a_file_has_no_server_and_a_server_no_file() {
        let sqlite = DataSourceFields::from(&ConnectionConfig::Sqlite(SqliteConfig {
            path: PathBuf::from("/data/shop.db"),
            read_only: false,
        }));
        let values = TemplateValues::default();
        assert_eq!(
            command(&["sqlite3", "{file}"])
                .unwrap()
                .render(&values, &sqlite)
                .unwrap(),
            ["/data/shop.db"]
        );
        let error = command(&["ssh", "{host}"])
            .unwrap()
            .render(&values, &sqlite)
            .unwrap_err();
        assert_eq!(error.to_string(), "nothing here fills {host}");
        let error = command(&["sqlite3", "{file}"])
            .unwrap()
            .render(&values, &postgres())
            .unwrap_err();
        assert_eq!(error.to_string(), "nothing here fills {file}");
    }
}
