//! The `chart` tool (`docs/charts-as-code.md`, CH4): one read statement, run as `query` runs it,
//! drawn as DbRover's chart tab draws it and a report's chart is written, and answered as a
//! PNG an MCP client shows, or as SVG text.
//!
//! A chart shows rows as surely as a table does, so it reads no more of them than `query`
//! returns: the project's row limit cuts the rows, and the chart and its reply say so.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use dbrover_chart::{ChartData, ChartSpec, Column, Role, Size, arrange, file_svg};
use dbrover_core::{ColumnInfo, Row, ValueKind};
use dbrover_picture::Fonts;
use dbrover_sql::Dialect;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::server::ToolError;

/// The picture's size in points: a report's chart's default, 16 by 9.
const SIZE: Size = Size {
    width: 800.0,
    height: 450.0,
};

/// A PNG's pixels a point, as **Export ›** offers it first: sharp on a high-density screen.
const PNG_SCALE: f32 = 2.0;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(crate) struct ChartArgs {
    /// The data source's name, as list_data_sources gives it (or its id).
    pub(crate) data_source: String,
    /// One statement that only reads, as query takes it, whose rows the chart draws. Group and
    /// order in the statement: SELECT month, SUM(amount) AS net … GROUP BY month ORDER BY month.
    pub(crate) sql: String,
    /// The chart, by the keys a chart block above a statement takes in DbRover: kind ("bars",
    /// "lines" or "scatter"), x (a column), y (a column or a list of them), series (a column
    /// whose values split y into a series each), stack (true to stack bars), title, x_title,
    /// y_title, y_zero, sort ("net desc") and limit (rows, after sort). Every key is optional:
    /// one left out takes the value DbRover's chart tab starts with, bars over text, lines over
    /// dates.
    #[serde(default)]
    pub(crate) chart: Map<String, Value>,
    /// png (the default): an image at 2 pixels a point, which clients show; svg: the SVG as
    /// text.
    #[serde(default)]
    pub(crate) format: ChartFormat,
}

/// What the chart tool answers with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
pub(crate) enum ChartFormat {
    /// A PNG, as MCP image content.
    #[default]
    Png,
    /// The SVG, as text.
    Svg,
}

impl ChartFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Svg => "svg",
        }
    }
}

/// The spec in `chart`, read as a block's is.
///
/// # Errors
///
/// [`ToolError`] naming every key with a problem, before anything runs.
pub(crate) fn spec(chart: &Map<String, Value>) -> Result<ChartSpec, ToolError> {
    ChartSpec::from_json(chart).map_err(|problems| {
        let details: Vec<String> = problems
            .iter()
            .map(|problem| format!("chart.{}: {}", problem.key, problem.detail))
            .collect();
        ToolError::invalid(details.join("; "))
    })
}

/// A statement's rows, as the chart reads them.
pub(crate) struct Read<'a> {
    pub(crate) columns: &'a [ColumnInfo],
    pub(crate) rows: &'a [Row],
    /// The project's row limit cut the rows.
    pub(crate) truncated: bool,
    pub(crate) max_rows: usize,
    /// The data source's SQL dialect, which names its types' kinds.
    pub(crate) dialect: Option<Dialect>,
}

/// The chart, drawn as SVG, and what the reply says beside it.
pub(crate) struct Drawn {
    svg: String,
    summary: Value,
}

/// Draws `read`'s rows as `spec` says, as a report's chart file is drawn.
///
/// # Errors
///
/// [`ToolError`] when the spec names a column the rows don't have, or one that can't be where
/// it puts it.
pub(crate) fn draw(spec: &ChartSpec, read: &Read<'_>) -> Result<Drawn, ToolError> {
    let columns: Vec<Column> = read
        .columns
        .iter()
        .map(|column| Column {
            name: column.name.clone(),
            kind: read.dialect.map_or(ValueKind::Undeclared, |dialect| {
                dialect.value_kind(column.type_name.as_deref())
            }),
        })
        .collect();
    let roles: Vec<Role> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            Role::of(
                column.kind,
                read.rows.iter().filter_map(|row| row.get(index)),
            )
        })
        .collect();
    let resolved = spec.resolve(&columns, &roles).map_err(|problems| {
        let details: Vec<String> = problems.iter().map(ToString::to_string).collect();
        ToolError::invalid(details.join("; "))
    })?;
    let order: Vec<usize> = (0..read.rows.len()).collect();
    let order = arrange(read.rows, &order, &roles, resolved.sort, resolved.limit);
    let rows: Vec<&Row> = order
        .iter()
        .filter_map(|&index| read.rows.get(index))
        .collect();
    let data = ChartData::read(&columns, &roles, &rows, &resolved.choice);
    let mut note = data.note();
    if let Some(arranged) = resolved.arrangement(&columns) {
        note.push_str(&format!(" · {arranged}"));
    }
    let cut = read.truncated.then(|| {
        format!(
            "only the first {} rows were read, the project's row limit",
            read.max_rows
        )
    });
    if let Some(cut) = &cut {
        note.push_str(&format!(" · {cut}"));
    }
    let svg = file_svg(&data, &resolved.options, SIZE, &note)
        .map_err(|error| ToolError::invalid(error.to_string()))?;
    let name = |index: usize| columns.get(index).map(|column| column.name.clone());
    let choice = &resolved.choice;
    let summary = json!({
        "kind": choice.kind.spec_name(),
        "x": name(choice.x),
        "y": choice.ys.iter().filter_map(|&y| name(y)).collect::<Vec<_>>(),
        "series": choice.series.and_then(name),
        "rows_read": read.rows.len(),
        "points": data.len(),
        "truncated": read.truncated,
        "truncated_reason": cut,
        "left_out": data.left_out(),
        "note": note,
        "width": SIZE.width,
        "height": SIZE.height,
    });
    Ok(Drawn { svg, summary })
}

impl Drawn {
    /// The reply: the picture as `format`, and what was charted as JSON beside it, also the
    /// structured content.
    pub(crate) fn reply(
        self,
        format: ChartFormat,
        elapsed_ms: u128,
    ) -> Result<CallToolResult, ToolError> {
        let mut summary = self.summary;
        if let Value::Object(object) = &mut summary {
            object.insert("format".to_owned(), json!(format.name()));
            object.insert("elapsed_ms".to_owned(), json!(elapsed_ms));
        }
        let picture = match format {
            ChartFormat::Png => {
                let png = dbrover_picture::png(&self.svg, PNG_SCALE, &Fonts::bundled())
                    .map_err(|error| ToolError::invalid(error.to_string()))?;
                ContentBlock::image(STANDARD.encode(png), "image/png")
            }
            ChartFormat::Svg => ContentBlock::text(self.svg),
        };
        let mut result =
            CallToolResult::success(vec![picture, ContentBlock::text(summary.to_string())]);
        result.structured_content = Some(summary);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(names: &[&str]) -> Vec<ColumnInfo> {
        names
            .iter()
            .map(|name| ColumnInfo {
                name: (*name).to_owned(),
                type_name: None,
            })
            .collect()
    }

    fn rows(count: usize) -> Vec<Row> {
        (0..count)
            .map(|n| vec![Some(format!("c{n:03}")), Some(format!("{n}"))])
            .collect()
    }

    fn read<'a>(columns: &'a [ColumnInfo], rows: &'a [Row], truncated: bool) -> Read<'a> {
        Read {
            columns,
            rows,
            truncated,
            max_rows: rows.len(),
            dialect: None,
        }
    }

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(object) => object,
            _ => Map::new(),
        }
    }

    #[test]
    fn says_every_key_with_a_problem() {
        let error = spec(&object(json!({ "kind": "area", "ys": "net" }))).unwrap_err();

        assert_eq!(
            error.message,
            "chart.kind: `kind` is bars, lines or scatter, not \"area\"; chart.ys: no key `ys`; \
             did you mean `y`?"
        );
    }

    #[test]
    fn draws_the_rows_and_says_what_it_drew() {
        let columns = columns(&["category", "net"]);
        let rows = rows(3);
        let spec = spec(&object(json!({ "title": "Net" }))).unwrap();

        let drawn = draw(&spec, &read(&columns, &rows, false)).unwrap();

        assert!(drawn.svg.contains(">Net</text>"), "{}", drawn.svg);
        assert_eq!(drawn.summary["kind"], "bars");
        assert_eq!(drawn.summary["x"], "category");
        assert_eq!(drawn.summary["y"], json!(["net"]));
        assert_eq!(drawn.summary["points"], 3);
        assert_eq!(drawn.summary["truncated"], false);
        assert_eq!(drawn.summary["note"], "Charting 3 rows");
    }

    #[test]
    fn says_the_row_limit_cut_the_rows_in_the_picture_and_the_reply() {
        let columns = columns(&["category", "net"]);
        let rows = rows(2);

        let drawn = draw(&ChartSpec::default(), &read(&columns, &rows, true)).unwrap();

        let cut = "only the first 2 rows were read, the project's row limit";
        assert_eq!(drawn.summary["truncated"], true);
        assert_eq!(drawn.summary["truncated_reason"], cut);
        assert!(
            drawn
                .svg
                .contains(&format!("Charting 2 rows · {cut}</text>")),
            "{}",
            drawn.svg
        );
    }

    #[test]
    fn a_column_the_rows_dont_have_is_the_calls_problem() {
        let columns = columns(&["category", "net"]);
        let rows = rows(2);
        let spec = spec(&object(json!({ "y": "nett" }))).unwrap();

        let Err(error) = draw(&spec, &read(&columns, &rows, false)) else {
            panic!("nett isn't a column");
        };

        assert_eq!(
            error.message,
            "chart: `y = \"nett\"` isn't a column of this result: category, net"
        );
    }

    #[test]
    fn answers_with_a_png_or_the_svg_and_what_was_charted() {
        let columns = columns(&["category", "net"]);
        let rows = rows(3);
        for format in [ChartFormat::Png, ChartFormat::Svg] {
            let drawn = draw(&ChartSpec::default(), &read(&columns, &rows, false)).unwrap();

            let result = drawn.reply(format, 5).unwrap();

            let picture = result.content.first();
            match (format, picture) {
                (ChartFormat::Png, Some(ContentBlock::Image(image))) => {
                    assert_eq!(image.mime_type, "image/png");
                    let png = STANDARD.decode(&image.data).unwrap();
                    assert!(png.starts_with(b"\x89PNG"));
                }
                (ChartFormat::Svg, Some(ContentBlock::Text(text))) => {
                    assert!(text.text.starts_with("<svg "));
                }
                (format, other) => panic!("{format:?}: {other:?}"),
            }
            let summary = result.structured_content.unwrap();
            assert_eq!(summary["format"], format.name());
            assert_eq!(summary["elapsed_ms"], 5);
        }
    }
}
