//! Tests of templates (W8c): golden output, the context, the filters, escaping, hostile values
//! and every kind of mistake.

use std::sync::Arc;

use dbrover_core::{Cell, ValueKind};
use dbrover_sql::Dialect;

use super::*;
use crate::template::{PartContext, jinja};

fn text(value: &str) -> Cell {
    Some(value.to_owned())
}

fn template(header: &str, row: &str, footer: &str) -> TemplateSource {
    TemplateSource {
        header: header.to_owned(),
        row: row.to_owned(),
        footer: footer.to_owned(),
        escape: Escape::None,
    }
}

fn context() -> ExportContext {
    ExportContext {
        data_source: "shop".to_owned(),
        table: Some("main.orders".to_owned()),
        parameters: vec![
            ("since".to_owned(), text("2026-01-01")),
            ("status".to_owned(), None),
        ],
        exported_at: "2026-11-01T12:00:00Z".to_owned(),
    }
}

fn spec(source: TemplateSource, dialect: Option<Dialect>) -> ExportSpec {
    ExportSpec::Template(Box::new(TemplateSpec {
        source: Arc::new(source),
        context: context(),
        dialect,
    }))
}

fn columns() -> Vec<ExportColumn> {
    vec![
        ExportColumn {
            name: "id".to_owned(),
            type_name: Some("INTEGER".to_owned()),
            kind: ValueKind::Integer,
        },
        ExportColumn::text("note"),
        ExportColumn::text("id"),
    ]
}

/// Values a template must write as they are, never as template code or markup of its own.
fn hostile_rows() -> Vec<Vec<Cell>> {
    vec![
        vec![text("1"), text("plain"), text("7")],
        vec![text("2"), text("a, \"quoted\"\nline"), None],
        vec![text("3"), text("{{ 7 * 7 }} <b>&</b> {% raw %}"), text("")],
        vec![text("4"), text("🦀\r\ncrab | ' ;"), None],
    ]
}

/// Renders `source` over `rows`, or the error it stops with.
fn rendered(source: TemplateSource, rows: &[Vec<Cell>]) -> Result<String, String> {
    try_render(&spec(source, Some(Dialect::Sqlite)), &columns(), rows)
        .unwrap()
        .map_err(|error| error.to_string())
}

#[test]
fn writes_the_plans_confluence_table() {
    let confluence = template(
        "||{% for column in columns %} {{ column.name }} ||{% endfor %}\n",
        "|{% for value in row %} {{ value }} |{% endfor %}\n",
        "",
    );
    assert_eq!(
        rendered(confluence, &hostile_rows()).unwrap(),
        "|| id || note || id ||\n\
         | 1 | plain | 7 |\n\
         | 2 | a, \"quoted\"\nline |  |\n\
         | 3 | {{ 7 * 7 }} <b>&</b> {% raw %} |  |\n\
         | 4 | 🦀\r\ncrab | ' ; |  |\n"
    );
}

#[test]
fn writes_xml_with_every_value_escaped() {
    let xml = TemplateSource {
        escape: Escape::Html,
        ..template(
            "<rows table=\"{{ table }}\">\n",
            "  <row n=\"{{ index }}\">{% for column in columns %}<{{ column.name|safe }}>\
             {{ row[loop.index0] }}</{{ column.name|safe }}>{% endfor %}</row>\n",
            "</rows>\n",
        )
    };
    let rows = &hostile_rows()[2..3];
    assert_eq!(
        rendered(xml, rows).unwrap(),
        "<rows table=\"main.orders\">\n  <row n=\"1\"><id>3</id><note>{{ 7 * 7 }} \
         &lt;b&gt;&amp;&lt;&#x2f;b&gt; {% raw %}</note><id></id></row>\n</rows>\n"
    );
}

#[test]
fn every_part_sees_the_export_and_each_its_own() {
    let source = template(
        "{{ data_source }} {{ table }} {{ exported_at }} since={{ parameters.since }} \
         status={{ parameters.status }}{% for name in parameters %} {{ name }}{% endfor %}\n\
         {% for column in columns %}{{ column.name }}:{{ column.type }}:{{ column.kind }} \
         {% endfor %}\n",
        "{{ index }} {{ record.id }} {{ record.id_2 }} {{ record['note'] | length }}\n",
        "{{ count }} rows from {{ table }}\n",
    );
    assert_eq!(
        rendered(source, &hostile_rows()[..2]).unwrap(),
        "shop main.orders 2026-11-01T12:00:00Z since=2026-01-01 status= since status\n\
         id:INTEGER:integer note::text id::text \n\
         1 1 7 5\n\
         2 2  16\n\
         2 rows from main.orders\n"
    );
}

#[test]
fn null_is_written_as_nothing_and_can_be_told_apart() {
    let source = template(
        "",
        "{{ row[2] }}|{{ row[2] if row[2] is not none else 'NULL' }}|{{ row[2] is none }}\n",
        "",
    );
    assert_eq!(
        rendered(source, &hostile_rows()[..3]).unwrap(),
        // Jinja writes booleans as Python does.
        "7|7|False\n|NULL|True\n||False\n"
    );
}

#[test]
fn the_export_filters_quote_for_their_formats() {
    let source = template(
        "{% for column in columns %}{{ column.name|sql_name }} {% endfor %}\n",
        "{{ row[1]|sql_literal }} {{ row[2]|sql_literal }} {{ row[1]|csv }} \
         {{ row[1]|csv(';') }} {{ row[1]|json }} {{ row[2]|json }} {{ (row[0]|int)|json }}\n",
        "",
    );
    let rows = vec![
        vec![text("1"), text("it's, \"so\""), None],
        vec![text("2"), text("a;b"), text("x")],
    ];
    let quoted = |dialect| {
        try_render(&spec(source.clone(), Some(dialect)), &columns(), &rows)
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        quoted(Dialect::Sqlite),
        "\"id\" \"note\" \"id\" \n\
         'it''s, \"so\"' NULL \"it's, \"\"so\"\"\" \"it's, \"\"so\"\"\" \"it's, \\\"so\\\"\" null 1\n\
         'a;b' 'x' a;b \"a;b\" \"a;b\" \"x\" 2\n"
    );
    assert!(
        quoted(Dialect::Mysql).starts_with("`id` `note` `id` \n'it''s, \"so\"' NULL"),
        "{}",
        quoted(Dialect::Mysql)
    );
    assert!(quoted(Dialect::SqlServer).starts_with("[id] [note] [id] \nN'it''s"));

    let without_sql = try_render(&spec(source, None), &columns(), &rows)
        .unwrap()
        .unwrap_err()
        .to_string();
    assert_eq!(
        without_sql,
        "the header template, line 1: invalid operation: sql_name quotes for a SQL database, \
         and these rows come from another kind"
    );
}

#[test]
fn says_where_a_template_is_wrong_when_it_is_read() {
    let cases = [
        (
            template("{% for column in columns %}", "x", ""),
            "the header template, line 1: syntax error: unexpected end of input",
        ),
        (
            template("", "\n{{ value | }}", ""),
            "the row template, line 2: syntax error",
        ),
        (
            template("", "{% include 'secrets.txt' %}", ""),
            "the row template, line 1: syntax error",
        ),
    ];
    for (source, problem) in cases {
        let error = check_template(&source).unwrap_err().to_string();
        assert!(error.starts_with(problem), "{error}");
    }
    check_template(&template(
        "{{ columns|length }}",
        "{{ row }}",
        "{{ count }}",
    ))
    .unwrap();
}

#[test]
fn a_name_that_doesnt_exist_stops_the_export_naming_the_row() {
    let typo = template("", "{{ recrod.id }}\n", "");
    let error = rendered(typo, &hostile_rows()).unwrap_err();
    assert!(
        error.starts_with("row 1: the row template, line 1: undefined value"),
        "{error}"
    );
    let footer_row = template("", "x", "{{ row }}");
    let error = rendered(footer_row, &hostile_rows()).unwrap_err();
    assert!(
        error.starts_with("the footer template, line 1: undefined value"),
        "{error}"
    );
}

#[test]
fn a_loop_that_never_ends_runs_out_of_fuel() {
    let endless = template(
        "",
        "{% for i in range(100000) %}{% for j in range(100000) %}{% endfor %}{% endfor %}",
        "",
    );
    let started = std::time::Instant::now();
    assert_eq!(
        rendered(endless, &hostile_rows()).unwrap_err(),
        "row 1: the row template, line 1: it took too many steps for one row: does a loop \
         never end?"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn a_render_that_writes_too_much_stops() {
    let limits = TemplateLimits {
        fuel: 10_000,
        max_bytes: 100,
    };
    let engine = jinja::compile(
        &template("", "{% for i in range(1000) %}x{% endfor %}", ""),
        None,
        limits,
    )
    .unwrap();
    let export = context();
    let columns = columns();
    let row = [text("1"), None, None];
    let context = TemplateContext {
        columns: &columns,
        keys: &[],
        export: &export,
        part: PartContext::Row {
            values: &row,
            index: 1,
        },
    };
    let mut out = Vec::new();
    let error = engine.render(&context, &mut out).unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("the row template wrote more than"),
        "{error}"
    );
    assert!(out.len() <= 100);
}

#[test]
fn a_file_written_a_page_at_a_time_reads_as_one_written_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let spec = spec(
        template(
            "<{{ columns|length }}>\n",
            "{{ index }}:{{ row|join(',') }}\n",
            "</{{ count }}>\n",
        ),
        Some(Dialect::Sqlite),
    );
    let rows = hostile_rows();
    let whole = try_render(&spec, &columns(), &rows).unwrap().unwrap();
    let path = dir.path().join("rows.txt");
    let mut writer = ExportWriter::create(&path, &spec, &columns()).unwrap();
    for page in rows.chunks(3) {
        writer.write(page).unwrap();
    }
    writer.finish().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), whole);
    assert!(whole.ends_with("</4>\n"));
}
