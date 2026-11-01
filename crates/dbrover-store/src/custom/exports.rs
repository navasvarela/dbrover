//! `exports.toml`: formats of the user's own, in the export dialog and **Copy as ›** (W8). See
//! `docs/exports.md`.

use std::sync::Arc;

use dbrover_core::{CommandTemplate, Placeholder};
use dbrover_export::{
    CustomFormat, CustomFormatId, CustomFormatKind, Escape, ExportFormat, Preset, ProgramFormat,
    TemplateSource, check_template,
};

use super::commands::DEFAULT_COMMAND_TIMEOUT;
use super::{Checked, entries, entry_label, serde_problem};
use crate::StatementTimeout;

/// Checks the text of `exports.toml`.
pub fn parse_exports(text: &str) -> Checked<Vec<CustomFormat>> {
    let mut checked = Checked::<Vec<CustomFormat>>::default();
    for (index, entry) in entries(text, "export", &mut checked.problems)
        .into_iter()
        .enumerate()
    {
        let label = entry_label("export", &entry, "id", index);
        let format = match entry {
            toml::Value::Table(table) => check(table),
            _ => Err("each export is a [[export]] table".to_owned()),
        };
        match format {
            Ok(format) if checked.value.iter().any(|kept| kept.id == format.id) => {
                checked.problems.push(format!(
                    "{label}: another export already has this id; the first one is kept"
                ));
            }
            Ok(format) => checked.value.push(format),
            Err(problem) => checked.problems.push(format!("{label}: {problem}")),
        }
    }
    checked
}

/// An entry's keys, taken one at a time, so that what is left over is what the entry's kind
/// reads as options, or doesn't know.
struct Fields(toml::Table);

impl Fields {
    fn string(&mut self, key: &str) -> Result<Option<String>, String> {
        match self.0.remove(key) {
            None => Ok(None),
            Some(toml::Value::String(text)) => Ok(Some(text)),
            Some(other) => Err(format!(
                "{key} must be a string, not a {}",
                other.type_str()
            )),
        }
    }

    fn required_string(&mut self, key: &str) -> Result<String, String> {
        self.string(key)?
            .ok_or_else(|| format!("missing field `{key}`"))
    }

    fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    fn boolean(&mut self, key: &str) -> Result<Option<bool>, String> {
        match self.0.remove(key) {
            None => Ok(None),
            Some(toml::Value::Boolean(value)) => Ok(Some(value)),
            Some(other) => Err(format!(
                "{key} must be true or false, not a {}",
                other.type_str()
            )),
        }
    }

    fn strings(&mut self, key: &str) -> Result<Vec<String>, String> {
        let not_a_list =
            || format!("{key} must be a list of strings, as [\"program\", \"--flag\"]");
        match self.0.remove(key) {
            Some(toml::Value::Array(items)) => items
                .into_iter()
                .map(|item| match item {
                    toml::Value::String(text) => Ok(text),
                    _ => Err(not_a_list()),
                })
                .collect(),
            _ => Err(not_a_list()),
        }
    }
}

/// The key that only each kind of export has, which says what an entry is.
const PRESET_KEY: &str = "format";
const TEMPLATE_KEY: &str = "row";
const PROGRAM_KEY: &str = "command";

fn check(table: toml::Table) -> Result<CustomFormat, String> {
    let mut fields = Fields(table);
    let id = fields.required_string("id")?;
    let id = CustomFormatId::new(&id).ok_or_else(|| {
        "an id may only have lowercase letters, digits, \"-\" and \"_\", and starts with a \
         letter or digit"
            .to_owned()
    })?;
    let name = fields.required_string("name")?;
    if name.trim().is_empty() {
        return Err("the name is empty".to_owned());
    }
    let description = fields
        .string("description")?
        .filter(|text| !text.trim().is_empty());
    let extension = fields
        .string("extension")?
        .map(checked_extension)
        .transpose()?;
    let kinds = [PRESET_KEY, TEMPLATE_KEY, PROGRAM_KEY].map(|key| fields.has(key));
    if kinds.iter().filter(|&&has| has).count() > 1 {
        return Err(
            "an export is one of a preset (format), a template (row) or a program \
                    (command), not more"
                .to_owned(),
        );
    }
    let (kind, extension) = match kinds {
        [true, _, _] => {
            let format = fields.required_string(PRESET_KEY)?;
            let preset = preset(&format, fields)?;
            let extension = extension.unwrap_or_else(|| preset.format.extension().to_owned());
            (CustomFormatKind::Preset(preset), extension)
        }
        [_, true, _] => {
            let extension = extension.ok_or_else(|| {
                "a template needs an extension for its files, such as extension = \"txt\""
                    .to_owned()
            })?;
            (CustomFormatKind::Template(template(fields)?), extension)
        }
        [_, _, true] => {
            let extension = extension.ok_or_else(|| {
                "a program needs the extension of the files it writes, such as extension = \
                 \"pdf\""
                    .to_owned()
            })?;
            (CustomFormatKind::Program(program(fields)?), extension)
        }
        [false, false, false] => {
            return Err(
                "the export is none of a preset, a template or a program: write \
                        format = \"csv\" for a preset of a built-in format, row = \"…\" for a \
                        template, or command = [\"…\"] for a program"
                    .to_owned(),
            );
        }
    };
    Ok(CustomFormat {
        id,
        name,
        description,
        extension,
        kind,
    })
}

/// A program from the entry's `command`, `timeout` and `text`.
fn program(mut fields: Fields) -> Result<Arc<ProgramFormat>, String> {
    let command =
        CommandTemplate::parse(&fields.strings(PROGRAM_KEY)?).map_err(|error| error.to_string())?;
    if let Some(placeholder) = command
        .placeholders()
        .find(|placeholder| !fills(placeholder))
    {
        return Err(format!(
            "{placeholder} can't be filled for an export; a program gets the rows on stdin, \
             and its arguments can name {{table}}, {{schema}}, {{database}}, {{data_source}}, \
             {{host}}, {{port}}, {{user}} and {{file}}"
        ));
    }
    let timeout = match fields.string("timeout")? {
        Some(text) => StatementTimeout::try_from(text)
            .map_err(|error| format!("timeout: {error}"))?
            .duration(),
        None => DEFAULT_COMMAND_TIMEOUT,
    };
    let text = fields.boolean("text")?.unwrap_or(false);
    if let Some(unknown) = fields.0.keys().next() {
        return Err(format!(
            "{unknown:?} is not part of a program; a program has command, timeout and text"
        ));
    }
    Ok(Arc::new(ProgramFormat {
        command,
        timeout,
        text,
    }))
}

/// Whether an export can fill `placeholder`: what it knows of the data source and the table,
/// but no cells, which go to stdin.
fn fills(placeholder: &Placeholder) -> bool {
    match placeholder {
        Placeholder::Database
        | Placeholder::Schema
        | Placeholder::Table
        | Placeholder::DataSource
        | Placeholder::Host
        | Placeholder::Port
        | Placeholder::User
        | Placeholder::File => true,
        Placeholder::QualifiedName
        | Placeholder::Column { .. }
        | Placeholder::Value
        | Placeholder::Values
        | Placeholder::Row(_) => false,
    }
}

/// A template of the entry's `header`, `row` and `footer`, checked by compiling each part.
fn template(mut fields: Fields) -> Result<Arc<TemplateSource>, String> {
    let row = fields.required_string(TEMPLATE_KEY)?;
    let header = fields.string("header")?.unwrap_or_default();
    let footer = fields.string("footer")?.unwrap_or_default();
    let escape = match fields.string("escape")?.as_deref() {
        None | Some("none") => Escape::None,
        Some("html") => Escape::Html,
        Some(other) => {
            return Err(format!("escape is \"none\" or \"html\", not {other:?}"));
        }
    };
    if let Some(unknown) = fields.0.keys().next() {
        return Err(format!(
            "{unknown:?} is not part of a template; a template has header, row, footer and \
             escape"
        ));
    }
    let source = TemplateSource {
        header,
        row,
        footer,
        escape,
    };
    check_template(&source).map_err(|error| error.to_string())?;
    Ok(Arc::new(source))
}

/// A preset of the format whose id is `format`, with the entry's remaining keys as its options.
fn preset(format: &str, options: Fields) -> Result<Preset, String> {
    let format = ExportFormat::from_id(format).ok_or_else(|| {
        let ids: Vec<&str> = ExportFormat::ALL
            .into_iter()
            .map(ExportFormat::id)
            .collect();
        format!(
            "{format:?} is not a format; the formats are {}",
            ids.join(", ")
        )
    })?;
    let names = Preset::option_names(format);
    if let Some(unknown) = options.0.keys().find(|key| !names.contains(key)) {
        return Err(if names.is_empty() {
            format!(
                "{} has no options, so {unknown:?} can't be set",
                format.name()
            )
        } else {
            format!(
                "{unknown:?} is not an option of {}; its options are {}",
                format.name(),
                names.join(", ")
            )
        });
    }
    Preset::read(format, toml::Value::Table(options.0)).map_err(|error| serde_problem(&error))
}

/// The extension as a file name ends with it, without its dot: letters, digits, `-` and `_`,
/// with dots only between them (`tar.gz`).
fn checked_extension(extension: String) -> Result<String, String> {
    let valid = !extension.is_empty()
        && extension.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        });
    if valid {
        Ok(extension)
    } else if extension.starts_with('.') {
        Err(format!(
            "write the extension without its dot, as extension = {:?}",
            extension.trim_start_matches('.')
        ))
    } else {
        Err(format!(
            "{extension:?} is not an extension: use letters, digits, \"-\" and \"_\""
        ))
    }
}

#[cfg(test)]
mod tests {
    use dbrover_export::{CsvOptions, FieldSeparator};

    use super::*;

    #[test]
    fn reads_the_plans_preset() {
        let checked = parse_exports(
            r#"
            [[export]]
            id = "excel-semicolon"
            name = "CSV for Excel (semicolon)"
            format = "csv"
            separator = ";"
            bom = true
            "#,
        );
        assert!(checked.problems.is_empty(), "{:?}", checked.problems);
        let [format] = checked.value.as_slice() else {
            panic!("{:?}", checked.value);
        };
        assert_eq!(format.id.as_str(), "excel-semicolon");
        assert_eq!(format.name, "CSV for Excel (semicolon)");
        assert_eq!(format.description, None);
        assert_eq!(format.extension, "csv");
        let CustomFormatKind::Preset(preset) = &format.kind else {
            panic!("{format:?}");
        };
        assert_eq!(preset.format, ExportFormat::Csv);
        assert_eq!(
            preset.options.csv,
            CsvOptions {
                separator: FieldSeparator::SEMICOLON,
                bom: true,
                ..CsvOptions::default()
            }
        );
    }

    #[test]
    fn keeps_a_description_and_an_extension_of_its_own() {
        let checked = parse_exports(
            "[[export]]\nid = \"orders\"\nname = \"Orders\"\ndescription = \"For finance\"\n\
             extension = \"tsv.txt\"\nformat = \"tsv\"\nheader = false\n",
        );
        assert!(checked.problems.is_empty(), "{:?}", checked.problems);
        let format = &checked.value[0];
        assert_eq!(format.description.as_deref(), Some("For finance"));
        assert_eq!(format.extension, "tsv.txt");
    }

    #[test]
    fn skips_and_names_each_broken_export() {
        let cases = [
            (
                "name = \"N\"\nformat = \"csv\"",
                "export 1: missing field `id`",
            ),
            (
                "id = \"Big\"\nname = \"N\"\nformat = \"csv\"",
                "export \"Big\": an id may only have lowercase letters, digits, \"-\" and \
                 \"_\", and starts with a letter or digit",
            ),
            (
                "id = \"a\"\nformat = \"csv\"",
                "export \"a\": missing field `name`",
            ),
            (
                "id = \"a\"\nname = \" \"\nformat = \"csv\"",
                "export \"a\": the name is empty",
            ),
            (
                "id = \"a\"\nname = 3\nformat = \"csv\"",
                "export \"a\": name must be a string, not a integer",
            ),
            (
                "id = \"a\"\nname = \"N\"",
                "export \"a\": the export is none of a preset, a template or a program: \
                 write format = \"csv\" for a preset of a built-in format, row = \"…\" for a \
                 template, or command = [\"…\"] for a program",
            ),
            (
                "id = \"a\"\nname = \"N\"\nformat = \"csv\"\nrow = \"x\"",
                "export \"a\": an export is one of a preset (format), a template (row) or a \
                 program (command), not more",
            ),
            (
                "id = \"a\"\nname = \"N\"\ncommand = [\"pdf\"]",
                "export \"a\": a program needs the extension of the files it writes, such as \
                 extension = \"pdf\"",
            ),
            (
                "id = \"a\"\nname = \"N\"\nextension = \"pdf\"\ncommand = \"pdf --a\"",
                "export \"a\": command must be a list of strings, as [\"program\", \"--flag\"]",
            ),
            (
                "id = \"a\"\nname = \"N\"\nextension = \"pdf\"\ncommand = [\"pdf\", \"{value}\"]",
                "export \"a\": {value} can't be filled for an export; a program gets the rows \
                 on stdin, and its arguments can name {table}, {schema}, {database}, \
                 {data_source}, {host}, {port}, {user} and {file}",
            ),
            (
                "id = \"a\"\nname = \"N\"\nextension = \"pdf\"\ncommand = [\"pdf\"]\ntext = \"yes\"",
                "export \"a\": text must be true or false, not a string",
            ),
            (
                "id = \"a\"\nname = \"N\"\nextension = \"pdf\"\ncommand = [\"pdf\"]\nstdin = \"json\"",
                "export \"a\": \"stdin\" is not part of a program; a program has command, \
                 timeout and text",
            ),
            (
                "id = \"a\"\nname = \"N\"\nrow = \"x\"",
                "export \"a\": a template needs an extension for its files, such as \
                 extension = \"txt\"",
            ),
            (
                "id = \"a\"\nname = \"N\"\nextension = \"txt\"\nrow = \"x\"\nescape = \"xml\"",
                "export \"a\": escape is \"none\" or \"html\", not \"xml\"",
            ),
            (
                "id = \"a\"\nname = \"N\"\nextension = \"txt\"\nrow = \"x\"\nheadr = \"h\"",
                "export \"a\": \"headr\" is not part of a template; a template has header, \
                 row, footer and escape",
            ),
            (
                "id = \"a\"\nname = \"N\"\nextension = \"txt\"\nrow = \"{% for x in row %}\"",
                "export \"a\": the row template, line 1: syntax error: unexpected end of \
                 input, expected end of block",
            ),
            (
                "id = \"a\"\nname = \"N\"\nformat = \"excel\"",
                "export \"a\": \"excel\" is not a format; the formats are csv, xlsx, tsv, \
                 markdown, sql-insert, json, jsonl, parquet, html, sql-update",
            ),
            (
                "id = \"a\"\nname = \"N\"\nformat = \"csv\"\nseperator = \";\"",
                "export \"a\": \"seperator\" is not an option of CSV; its options are \
                 separator, quoting, header, null_text, line_ending, bom",
            ),
            (
                "id = \"a\"\nname = \"N\"\nformat = \"json\"\nheader = true",
                "export \"a\": JSON has no options, so \"header\" can't be set",
            ),
            (
                "id = \"a\"\nname = \"N\"\nformat = \"csv\"\nheader = \"yes\"",
                "export \"a\": invalid type: string \"yes\", expected a boolean",
            ),
            (
                "id = \"a\"\nname = \"N\"\nformat = \"csv\"\nextension = \".txt\"",
                "export \"a\": write the extension without its dot, as extension = \"txt\"",
            ),
            (
                "id = \"a\"\nname = \"N\"\nformat = \"csv\"\nextension = \"a/b\"",
                "export \"a\": \"a/b\" is not an extension: use letters, digits, \"-\" and \"_\"",
            ),
        ];
        for (entry, problem) in cases {
            let checked = parse_exports(&format!("[[export]]\n{entry}\n"));
            assert_eq!(checked.problems, [problem], "{entry}");
            assert!(checked.value.is_empty(), "{entry}");
        }
    }

    #[test]
    fn reads_the_plans_template() {
        let checked = parse_exports(
            r#"
            [[export]]
            id = "confluence"
            name = "Confluence table"
            extension = "txt"
            header = "||{% for column in columns %} {{ column.name }} ||{% endfor %}\n"
            row = "|{% for value in row %} {{ value }} |{% endfor %}\n"
            footer = ""

            [[export]]
            id = "page"
            name = "Page"
            extension = "html"
            escape = "html"
            row = '''
            <p>{{ record.title }}</p>
            '''
            "#,
        );
        assert!(checked.problems.is_empty(), "{:?}", checked.problems);
        let [confluence, page] = checked.value.as_slice() else {
            panic!("{:?}", checked.value);
        };
        let CustomFormatKind::Template(template) = &confluence.kind else {
            panic!("{confluence:?}");
        };
        assert_eq!(template.footer, "");
        assert_eq!(template.escape, Escape::None);
        assert!(confluence.writes_text());
        let CustomFormatKind::Template(template) = &page.kind else {
            panic!("{page:?}");
        };
        assert_eq!(template.escape, Escape::Html);
        assert_eq!(template.header, "");
    }

    #[test]
    fn reads_the_plans_program() {
        let checked = parse_exports(
            r#"
            [[export]]
            id = "pdf-report"
            name = "PDF report"
            extension = "pdf"
            command = ["/Users/me/bin/rows-to-pdf", "--title", "{table}"]

            [[export]]
            id = "yaml"
            name = "YAML"
            extension = "yaml"
            command = ["yq", "-P"]
            timeout = "5s"
            text = true
            "#,
        );
        assert!(checked.problems.is_empty(), "{:?}", checked.problems);
        let [pdf, yaml] = checked.value.as_slice() else {
            panic!("{:?}", checked.value);
        };
        let CustomFormatKind::Program(program) = &pdf.kind else {
            panic!("{pdf:?}");
        };
        assert_eq!(program.command.program(), "/Users/me/bin/rows-to-pdf");
        assert_eq!(program.timeout, DEFAULT_COMMAND_TIMEOUT);
        assert!(!pdf.writes_text());
        let CustomFormatKind::Program(program) = &yaml.kind else {
            panic!("{yaml:?}");
        };
        assert_eq!(program.timeout, std::time::Duration::from_secs(5));
        assert!(yaml.writes_text());
    }

    #[test]
    fn keeps_the_first_of_two_exports_with_one_id() {
        let checked = parse_exports(
            "[[export]]\nid = \"a\"\nname = \"First\"\nformat = \"csv\"\n\
             [[export]]\nid = \"a\"\nname = \"Second\"\nformat = \"tsv\"\n",
        );
        assert_eq!(
            checked.problems,
            ["export \"a\": another export already has this id; the first one is kept"]
        );
        assert_eq!(checked.value.len(), 1);
        assert_eq!(checked.value[0].name, "First");
    }
}
