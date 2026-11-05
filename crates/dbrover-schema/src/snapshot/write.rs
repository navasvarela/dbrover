//! The snapshot writer: one table of the file per table, index and view, one column, key or
//! check per line, and nothing that changes on its own.
//!
//! Written by hand rather than with `toml::to_string`, whose choice between inline and
//! standard tables, and whose spacing, follow its own rules and may change with a version. The
//! file's layout is a promise to Git: a snapshot taken twice of an unchanged schema is the same
//! file, byte for byte.

use dbrover_core::IdentityGeneration;
use dbrover_sql::{ColumnSpec, ForeignKey, ReferentialAction, ViewKind};

use super::{FORMAT, engine_name};
use crate::{Index, Schema, Table, View};

const HEADER: &str = "\
# A schema snapshot written by DbRover. Compare it with a live schema in DbRover's schema
# compare, or with `dbrover schema compare`. Regenerated whole each time; edits are lost.
";

/// The snapshot file of `schema`.
pub fn write_snapshot(schema: &Schema) -> String {
    let mut file = String::from(HEADER);
    file.push_str(&format!(
        "\n[snapshot]\nformat = {FORMAT}\nengine = {}\nschema = {}\n",
        string(engine_name(schema.dialect)),
        string(&schema.name)
    ));
    for (name, table) in &schema.tables {
        file.push_str(&format!("\n[tables.{}]\n", key(name)));
        write_table(&mut file, table, &schema.name);
    }
    for (name, index) in &schema.indexes {
        file.push_str(&format!("\n[indexes.{}]\n", key(name)));
        write_index(&mut file, index);
    }
    for (name, view) in &schema.views {
        file.push_str(&format!("\n[views.{}]\n", key(name)));
        write_view(&mut file, view);
    }
    file
}

fn write_table(file: &mut String, table: &Table, schema: &str) {
    let spec = &table.spec;
    let columns: Vec<String> = spec.columns.iter().map(column).collect();
    array_of_tables(file, "columns", &columns);
    if !spec.primary_key.is_empty() {
        let mut fields = Vec::new();
        if let Some(name) = &table.primary_key_name {
            fields.push(("name", string(name)));
        }
        fields.push(("columns", strings(&spec.primary_key)));
        file.push_str(&format!("primary_key = {}\n", inline(&fields)));
    }
    let unique: Vec<String> = spec
        .unique_keys
        .iter()
        .map(|key| {
            let mut fields = named(key.name.as_deref());
            fields.push(("columns", strings(&key.columns)));
            inline(&fields)
        })
        .collect();
    array_of_tables(file, "unique_keys", &unique);
    let foreign: Vec<String> = spec
        .foreign_keys
        .iter()
        .map(|key| foreign_key(key, schema))
        .collect();
    array_of_tables(file, "foreign_keys", &foreign);
    let checks: Vec<String> = spec
        .checks
        .iter()
        .map(|check| {
            let mut fields = named(check.name.as_deref());
            fields.push(("condition", string(&check.expression)));
            inline(&fields)
        })
        .collect();
    array_of_tables(file, "checks", &checks);
}

fn column(column: &ColumnSpec) -> String {
    let mut fields = vec![
        ("name", string(&column.name)),
        ("type", string(&column.data_type)),
    ];
    if !column.nullable {
        fields.push(("nullable", "false".to_owned()));
    }
    if let Some(default) = &column.default {
        fields.push(("default", string(default)));
    }
    if let Some(identity) = column.identity {
        let generation = string(generation_name(identity.generation));
        let value = if identity.start == 1 && identity.increment == 1 {
            generation
        } else {
            inline(&[
                ("generation", generation),
                ("start", identity.start.to_string()),
                ("increment", identity.increment.to_string()),
            ])
        };
        fields.push(("identity", value));
    }
    inline(&fields)
}

fn foreign_key(key: &ForeignKey, schema: &str) -> String {
    let mut fields = named(key.name.as_deref());
    fields.push(("columns", strings(&key.columns)));
    fields.push(("references", string(&key.target.name)));
    if let Some(target_schema) = key.target.schema.as_deref().filter(|s| *s != schema) {
        fields.push(("references_schema", string(target_schema)));
    }
    fields.push(("referenced_columns", strings(&key.target_columns)));
    for (field, action) in [("on_delete", key.on_delete), ("on_update", key.on_update)] {
        if action != ReferentialAction::NoAction {
            fields.push((field, string(&action.sql().to_lowercase())));
        }
    }
    inline(&fields)
}

fn write_index(file: &mut String, index: &Index) {
    file.push_str(&format!("table = {}\n", string(&index.table)));
    if index.unique {
        file.push_str("unique = true\n");
    }
    file.push_str(&format!("method = {}\n", string(&index.method)));
    file.push_str(&format!("key = {}\n", strings(&index.key)));
    if !index.include.is_empty() {
        file.push_str(&format!("include = {}\n", strings(&index.include)));
    }
    if let Some(predicate) = &index.predicate {
        file.push_str(&format!("where = {}\n", string(predicate)));
    }
}

fn write_view(file: &mut String, view: &View) {
    file.push_str(&format!("kind = {}\n", string(view_kind_name(view.kind))));
    let reads: Vec<String> = view.reads.iter().cloned().collect();
    if !reads.is_empty() {
        file.push_str(&format!("reads = {}\n", strings(&reads)));
    }
    file.push_str(&format!("query = {}\n", multi_line_string(&view.query)));
}

pub(super) fn generation_name(generation: IdentityGeneration) -> &'static str {
    match generation {
        IdentityGeneration::Always => "always",
        IdentityGeneration::ByDefault => "by default",
    }
}

pub(super) fn view_kind_name(kind: ViewKind) -> &'static str {
    match kind {
        ViewKind::View => "view",
        ViewKind::Materialized => "materialized view",
    }
}

fn named(name: Option<&str>) -> Vec<(&'static str, String)> {
    name.map(|name| vec![("name", string(name))])
        .unwrap_or_default()
}

/// `key = [` then one item a line, or nothing at all for no items.
fn array_of_tables(file: &mut String, key: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    file.push_str(&format!("{key} = [\n"));
    for item in items {
        file.push_str(&format!("  {item},\n"));
    }
    file.push_str("]\n");
}

fn inline(fields: &[(&str, String)]) -> String {
    let fields: Vec<String> = fields
        .iter()
        .map(|(key, value)| format!("{key} = {value}"))
        .collect();
    format!("{{ {} }}", fields.join(", "))
}

fn strings(items: &[String]) -> String {
    let items: Vec<String> = items.iter().map(|item| string(item)).collect();
    format!("[{}]", items.join(", "))
}

/// A key bare where TOML allows it, quoted otherwise.
fn key(name: &str) -> String {
    let bare = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare { name.to_owned() } else { string(name) }
}

/// A TOML basic string.
fn string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push('\t'),
            c if c.is_control() => quoted.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// A TOML multi-line basic string, starting on the line after its opening quotes (TOML drops
/// that first line break), so a query reads as it was printed.
///
/// A quote is written as it is unless it would make a third in a row, or end the text, where it
/// would run into the closing quotes; there it is escaped.
fn multi_line_string(text: &str) -> String {
    let mut quoted = String::from("\"\"\"\n");
    let mut run = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            run = 0;
        }
        match c {
            '"' => {
                run += 1;
                if run > 2 || chars.peek().is_none() {
                    quoted.push_str("\\\"");
                    run = 0;
                } else {
                    quoted.push('"');
                }
            }
            '\\' => quoted.push_str("\\\\"),
            '\n' | '\t' => quoted.push(c),
            c if c.is_control() => quoted.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => quoted.push(c),
        }
    }
    quoted.push_str("\"\"\"");
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_bare_where_toml_allows() {
        assert_eq!(key("orders_2026-11"), "orders_2026-11");
        assert_eq!(key("Order Items"), "\"Order Items\"");
        assert_eq!(key("a.b"), "\"a.b\"");
        assert_eq!(key(""), "\"\"");
    }

    #[test]
    fn strings_escape_what_toml_needs() {
        assert_eq!(string("a \"b\" \\ c\nd"), r#""a \"b\" \\ c\nd""#);
        assert_eq!(string("\u{7}"), r#""\u0007""#);
    }

    #[test]
    fn a_multi_line_string_never_closes_early() {
        assert_eq!(
            multi_line_string(" SELECT '\"\"\"' AS q\n  FROM \"T\""),
            "\"\"\"\n SELECT '\"\"\\\"' AS q\n  FROM \"T\\\"\"\"\""
        );
    }
}
