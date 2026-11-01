//! The chart tab beside the grid (W12): a chart of the loaded rows' columns, in the grid's
//! order and through its filter, drawn from what `dbrover_chart` lays out.

use std::sync::Arc;

use dbrover_chart::{
    Anchor, ChartData, ChartKind, Choice, Column, Drawing, MAX_POINTS, Mark, Paint, Role, Size,
    TextMeasure,
};
use dbrover_core::{ColumnInfo, Row, ValueKind};
use dbrover_sql::Dialect;
use egui::{Align2, Color32, FontId, Galley, Painter, Pos2, Sense, Stroke, TextStyle, Ui, Vec2};

use crate::theme::Palette;

/// The series' colours, in the order series take them, for the light theme and the dark one:
/// eight hues whose neighbours stay apart for colour-blind readers, each stepped for its
/// theme's background. A ninth series takes the first colour again.
const SERIES_LIGHT: [Color32; 8] = [
    Color32::from_rgb(0x2a, 0x78, 0xd6),
    Color32::from_rgb(0xeb, 0x68, 0x34),
    Color32::from_rgb(0x1b, 0xaf, 0x7a),
    Color32::from_rgb(0xed, 0xa1, 0x00),
    Color32::from_rgb(0xe8, 0x7b, 0xa4),
    Color32::from_rgb(0x00, 0x83, 0x00),
    Color32::from_rgb(0x62, 0x50, 0xd6),
    Color32::from_rgb(0xe3, 0x49, 0x48),
];
const SERIES_DARK: [Color32; 8] = [
    Color32::from_rgb(0x39, 0x87, 0xe5),
    Color32::from_rgb(0xd9, 0x59, 0x26),
    Color32::from_rgb(0x19, 0x9e, 0x70),
    Color32::from_rgb(0xc9, 0x85, 0x00),
    Color32::from_rgb(0xd5, 0x51, 0x81),
    Color32::from_rgb(0x00, 0x83, 0x00),
    Color32::from_rgb(0x90, 0x85, 0xe9),
    Color32::from_rgb(0xe6, 0x67, 0x67),
];
const LINE_WIDTH: f32 = 2.0;
/// Bars this wide or wider get rounded ends; narrower ones would round into slivers.
const ROUNDED_BAR_WIDTH: f32 = 8.0;
const BAR_ROUNDING: u8 = 2;

/// Whether the chart is shown in place of the grid, and the chart itself once it has been.
#[derive(Default)]
pub struct ChartTab {
    shown: bool,
    /// The chart of the console's previous run, kept for this one if its columns are the same.
    carried: Option<Remembered>,
    view: Option<ChartView>,
}

/// A chart's columns and choice, kept from one run of a console to the next.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Remembered {
    columns: Vec<String>,
    choice: Choice,
}

impl ChartTab {
    /// The tab a statement of the console's next run starts with: shown if this one was, with
    /// this chart's choice for a result with the same columns.
    pub fn carried_over(&self) -> Self {
        let remembered = self.view.as_ref().and_then(ChartView::remembered);
        Self {
            shown: self.shown,
            carried: remembered.or_else(|| self.carried.clone()),
            view: None,
        }
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    pub fn set_shown(&mut self, shown: bool) {
        self.shown = shown;
    }

    #[cfg(test)]
    pub fn choice(&self) -> Option<&Choice> {
        self.view.as_ref()?.choice.as_ref()
    }

    /// The chart as last laid out.
    #[cfg(test)]
    pub fn drawing(&self) -> Option<&Drawing> {
        self.view
            .as_ref()?
            .drawing
            .as_ref()
            .map(|(_, drawing)| &**drawing)
    }
}

/// The result a chart is drawn from.
pub struct Source<'a> {
    pub columns: &'a [ColumnInfo],
    pub rows: &'a [Row],
    /// The grid's order: indexes into `rows`, sorted and filtered.
    pub order: &'a [usize],
    /// The language's SQL dialect, which names its types' kinds; `None` reads every column's
    /// role from its values.
    pub dialect: Option<Dialect>,
}

struct ChartView {
    columns: Vec<Column>,
    roles: Vec<Role>,
    /// How many rows `roles` were read from: more rows may show a column isn't all numbers.
    roles_from: usize,
    /// `None` when there is no column of numbers to chart.
    choice: Option<Choice>,
    data: Option<Charted>,
    drawing: Option<(Size, Arc<Drawing>)>,
}

/// The points of a choice, and what they were read from.
struct Charted {
    choice: Choice,
    order: Vec<usize>,
    data: ChartData,
}

impl ChartView {
    fn new(source: &Source<'_>, carried: Option<&Remembered>) -> Self {
        let columns = chart_columns(source.columns, source.dialect);
        let roles = roles(&columns, source.rows);
        let names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
        let choice = match carried {
            Some(remembered) if remembered.columns == names => Some(remembered.choice.clone()),
            _ => Choice::starting(&roles),
        };
        Self {
            columns,
            roles,
            roles_from: source.rows.len(),
            choice,
            data: None,
            drawing: None,
        }
    }

    fn remembered(&self) -> Option<Remembered> {
        Some(Remembered {
            columns: self.names(),
            choice: self.choice.clone()?,
        })
    }

    fn names(&self) -> Vec<String> {
        self.columns
            .iter()
            .map(|column| column.name.clone())
            .collect()
    }

    fn shows(&self, columns: &[ColumnInfo]) -> bool {
        self.columns.len() == columns.len()
            && self
                .columns
                .iter()
                .zip(columns)
                .all(|(chart, result)| chart.name == result.name)
    }

    /// Reads the roles again once more rows have loaded, keeping the choice. A role that
    /// changed places X differently, so the points are read again too.
    fn refresh_roles(&mut self, rows: &[Row]) {
        if self.roles_from == rows.len() {
            return;
        }
        let roles = roles(&self.columns, rows);
        if roles != self.roles {
            self.roles = roles;
            self.data = None;
            self.drawing = None;
        }
        self.roles_from = rows.len();
    }

    /// The chart's points, read again only when the choice or the grid's rows change.
    fn data(&mut self, source: &Source<'_>) -> Option<&ChartData> {
        let choice = self.choice.as_ref()?;
        let fresh = self
            .data
            .as_ref()
            .is_some_and(|charted| &charted.choice == choice && charted.order == source.order);
        if !fresh {
            let rows: Vec<&Row> = source
                .order
                .iter()
                .filter_map(|&index| source.rows.get(index))
                .collect();
            self.data = Some(Charted {
                choice: choice.clone(),
                order: source.order.to_vec(),
                data: ChartData::read(&self.columns, &self.roles, &rows, choice),
            });
            self.drawing = None;
        }
        self.data.as_ref().map(|charted| &charted.data)
    }
}

fn chart_columns(columns: &[ColumnInfo], dialect: Option<Dialect>) -> Vec<Column> {
    columns
        .iter()
        .map(|column| Column {
            name: column.name.clone(),
            kind: dialect.map_or(ValueKind::Undeclared, |dialect| {
                dialect.value_kind(column.type_name.as_deref())
            }),
        })
        .collect()
}

/// Each column's role, from the first rows a chart can show.
fn roles(columns: &[Column], rows: &[Row]) -> Vec<Role> {
    let rows = &rows[..rows.len().min(MAX_POINTS)];
    columns
        .iter()
        .enumerate()
        .map(|(index, column)| Role::of(column.kind, rows.iter().filter_map(|row| row.get(index))))
        .collect()
}

impl ChartTab {
    /// The chart's view of `source`, made again when its columns change.
    fn view(&mut self, source: &Source<'_>) -> &mut ChartView {
        let stale = !self
            .view
            .as_ref()
            .is_some_and(|view| view.shows(source.columns));
        if stale {
            let carried = match self.view.take() {
                Some(view) => view.remembered(),
                None => self.carried.clone(),
            };
            self.view = Some(ChartView::new(source, carried.as_ref()));
        }
        let view = self
            .view
            .get_or_insert_with(|| ChartView::new(source, None));
        view.refresh_roles(source.rows);
        view
    }
}

/// The kind, X and Y, above the chart.
pub fn controls(ui: &mut Ui, tab: &mut ChartTab, source: &Source<'_>) {
    let view = tab.view(source);
    let Some(choice) = &mut view.choice else {
        return;
    };
    for kind in ChartKind::ALL {
        ui.selectable_value(&mut choice.kind, kind, kind.label());
    }
    ui.separator();
    let mut x = choice.x;
    egui::ComboBox::from_id_salt("chart_x")
        .selected_text(format!("X: {}", column_name(&view.columns, choice.x)))
        .show_ui(ui, |ui| {
            for (index, column) in view.columns.iter().enumerate() {
                ui.selectable_value(&mut x, index, &column.name);
            }
        });
    if x != choice.x {
        choice.set_x(x);
    }
    let numbers: Vec<usize> = (0..view.columns.len())
        .filter(|&index| index != choice.x && view.roles.get(index) == Some(&Role::Number))
        .collect();
    ui.menu_button(y_label(&view.columns, &choice.ys), |ui| {
        if numbers.is_empty() {
            ui.weak("No other column holds numbers");
        }
        for index in numbers {
            let mut ticked = choice.ys.contains(&index);
            let name = column_name(&view.columns, index);
            if ui.checkbox(&mut ticked, name).changed() {
                choice.toggle_y(index);
            }
        }
    });
}

fn y_label(columns: &[Column], ys: &[usize]) -> String {
    match ys {
        [] => "Y: none".to_owned(),
        [only] => format!("Y: {}", column_name(columns, *only)),
        [first, rest @ ..] => format!(
            "Y: {} and {} more",
            column_name(columns, *first),
            rest.len()
        ),
    }
}

fn column_name(columns: &[Column], index: usize) -> &str {
    columns.get(index).map_or("", |column| column.name.as_str())
}

/// The chart, filling the rest of the panel, with a line under it saying what was charted.
pub fn show(ui: &mut Ui, tab: &mut ChartTab, source: &Source<'_>) {
    let view = tab.view(source);
    if view.choice.is_none() {
        ui.weak("A chart needs a column of numbers");
        return;
    }
    let Some(data) = view.data(source) else {
        return;
    };
    let note = summary(data);
    if data.is_empty() {
        ui.weak(note);
        return;
    }
    let font = TextStyle::Small.resolve(ui.style());
    let note_height = ui.text_style_height(&TextStyle::Body) + ui.spacing().item_spacing.y;
    let available = ui.available_size() - Vec2::new(0.0, note_height);
    let (response, painter) = ui.allocate_painter(available.max(Vec2::ZERO), Sense::hover());
    let size = Size {
        width: response.rect.width(),
        height: response.rect.height(),
    };
    let ChartView {
        data: Some(charted),
        drawing: laid_out,
        ..
    } = &mut *view
    else {
        return;
    };
    let measure = Measure {
        painter: &painter,
        font: &font,
    };
    let drawing = drawing(laid_out, &charted.data, size, &measure);
    let paints = Paints::of(&Palette::for_visuals(ui.visuals()));
    paint(&painter, response.rect.min, &drawing, &paints, &font);
    hover(ui, &response, &drawing, view, source, &paints);
    ui.weak(note);
}

/// `data` laid out in `size`, from `laid_out` when it was laid out in that size already.
fn drawing(
    laid_out: &mut Option<(Size, Arc<Drawing>)>,
    data: &ChartData,
    size: Size,
    measure: &Measure<'_>,
) -> Arc<Drawing> {
    if let Some((in_size, drawing)) = laid_out
        && *in_size == size
    {
        return Arc::clone(drawing);
    }
    let drawing = Arc::new(dbrover_chart::layout(data, size, measure));
    *laid_out = Some((size, Arc::clone(&drawing)));
    drawing
}

struct Measure<'a> {
    painter: &'a Painter,
    font: &'a FontId,
}

impl Measure<'_> {
    fn galley(&self, text: &str) -> Arc<Galley> {
        self.painter
            .layout_no_wrap(text.to_owned(), self.font.clone(), Color32::PLACEHOLDER)
    }
}

impl TextMeasure for Measure<'_> {
    fn width(&self, text: &str) -> f32 {
        self.galley(text).size().x
    }

    fn line_height(&self) -> f32 {
        self.galley("Ag").size().y
    }
}

/// The colours of a chart's marks in the current theme.
struct Paints {
    series: [Color32; 8],
    axis: Color32,
    grid: Color32,
    label: Color32,
    title: Color32,
    guide: Color32,
}

impl Paints {
    fn of(palette: &Palette) -> Self {
        Self {
            series: if palette.dark {
                SERIES_DARK
            } else {
                SERIES_LIGHT
            },
            axis: palette.text_weak,
            grid: palette.border,
            label: palette.text_weak,
            title: palette.text,
            guide: palette.text_weak.gamma_multiply(0.5),
        }
    }

    fn colour(&self, paint: Paint) -> Color32 {
        match paint {
            Paint::Series(index) => self.series[index % self.series.len()],
            Paint::Axis => self.axis,
            Paint::Grid => self.grid,
            Paint::Label => self.label,
            Paint::Title => self.title,
        }
    }
}

fn paint(painter: &Painter, origin: Pos2, drawing: &Drawing, paints: &Paints, font: &FontId) {
    let at = |point: dbrover_chart::Point| origin + Vec2::new(point.x, point.y);
    for mark in &drawing.marks {
        match mark {
            Mark::Rect { rect, paint } => {
                let rounding = if rect.width() >= ROUNDED_BAR_WIDTH {
                    BAR_ROUNDING
                } else {
                    0
                };
                painter.rect_filled(
                    egui::Rect::from_min_max(at(rect.min), at(rect.max)),
                    rounding,
                    paints.colour(*paint),
                );
            }
            Mark::Line { from, to, paint } => {
                painter.line_segment(
                    [at(*from), at(*to)],
                    Stroke::new(1.0, paints.colour(*paint)),
                );
            }
            Mark::Polyline { points, paint } => {
                painter.add(egui::Shape::line(
                    points.iter().map(|point| at(*point)).collect(),
                    Stroke::new(LINE_WIDTH, paints.colour(*paint)),
                ));
            }
            Mark::Circle {
                centre,
                radius,
                paint,
            } => {
                painter.circle_filled(at(*centre), *radius, paints.colour(*paint));
            }
            Mark::Text {
                at: point,
                text,
                anchor,
                paint,
            } => {
                let align = match anchor {
                    Anchor::Start => Align2::LEFT_CENTER,
                    Anchor::Middle => Align2::CENTER_CENTER,
                    Anchor::End => Align2::RIGHT_CENTER,
                };
                painter.text(at(*point), align, text, font.clone(), paints.colour(*paint));
            }
        }
    }
}

/// A line down the chart at the point under the pointer, and its row's values beside it.
fn hover(
    ui: &Ui,
    response: &egui::Response,
    drawing: &Drawing,
    view: &ChartView,
    source: &Source<'_>,
    paints: &Paints,
) {
    let Some(pointer) = response.hover_pos() else {
        return;
    };
    let local = pointer - response.rect.min;
    let Some(point) = drawing.hit(dbrover_chart::Point::new(local.x, local.y)) else {
        return;
    };
    if let Some(x) = drawing.x_of(point) {
        let x = response.rect.min.x + x;
        let top = response.rect.min.y + drawing.plot.min.y;
        let bottom = response.rect.min.y + drawing.plot.max.y;
        ui.painter().line_segment(
            [Pos2::new(x, top), Pos2::new(x, bottom)],
            Stroke::new(1.0, paints.guide),
        );
    }
    let Some(lines) = point_values(view, source, point) else {
        return;
    };
    response.clone().on_hover_ui_at_pointer(|ui| {
        egui::Grid::new("chart_point")
            .num_columns(2)
            .show(ui, |ui| {
                for (name, value) in lines {
                    ui.weak(name);
                    ui.label(value);
                    ui.end_row();
                }
            });
    });
}

/// The X column's and each series' name and value, as the grid shows them, at `point`.
fn point_values(
    view: &ChartView,
    source: &Source<'_>,
    point: usize,
) -> Option<Vec<(String, String)>> {
    let charted = view.data.as_ref()?;
    let row = charted
        .data
        .rows
        .get(point)
        .and_then(|&index| charted.order.get(index))
        .and_then(|&index| source.rows.get(index))?;
    let line = |column: usize| {
        let name = column_name(&view.columns, column).to_owned();
        let value = row
            .get(column)
            .and_then(Option::as_deref)
            .unwrap_or("NULL")
            .to_owned();
        (name, value)
    };
    Some(
        std::iter::once(charted.choice.x)
            .chain(charted.choice.ys.iter().copied())
            .map(line)
            .collect(),
    )
}

/// What was charted out of the rows, and what was left out, for the line under the chart:
/// "Charting 12 rows · 1 row left out: no month value".
pub fn summary(data: &ChartData) -> String {
    let summary = data.summary;
    if summary.rows == 0 {
        return "No rows to chart".to_owned();
    }
    let mut parts = vec![match summary.limit {
        Some(limit) => format!(
            "Charting the first {limit} of {} rows; group them in the query to chart them all",
            summary.rows
        ),
        None => format!("Charting {}", rows(summary.rows)),
    }];
    if summary.without_x > 0 {
        parts.push(format!(
            "{} left out: no {} value",
            rows(summary.without_x),
            data.x_name
        ));
    }
    for series in &data.series {
        let gaps = series.gaps();
        if gaps > 0 {
            let noun = if gaps == 1 { "gap" } else { "gaps" };
            parts.push(format!(
                "{gaps} {noun} in {}: null or not a number",
                series.name
            ));
        }
    }
    parts.join(" · ")
}

fn rows(count: usize) -> String {
    match count {
        1 => "1 row".to_owned(),
        count => format!("{count} rows"),
    }
}

#[cfg(test)]
mod tests {
    use dbrover_chart::{Series, Summary, XValues};

    use super::*;

    fn data(summary: Summary, gaps: usize) -> ChartData {
        ChartData {
            kind: ChartKind::Bars,
            x_name: "month".to_owned(),
            x: XValues::Categories(Vec::new()),
            series: vec![Series {
                name: "total".to_owned(),
                values: vec![None; gaps],
            }],
            rows: Vec::new(),
            summary,
        }
    }

    #[test]
    fn says_what_was_charted_and_what_was_left_out() {
        let all = Summary {
            rows: 12,
            limit: None,
            without_x: 0,
        };
        assert_eq!(summary(&data(all, 0)), "Charting 12 rows");
        let some = Summary {
            rows: 1200,
            limit: Some(250),
            without_x: 1,
        };
        assert_eq!(
            summary(&data(some, 2)),
            "Charting the first 250 of 1200 rows; group them in the query to chart them all \
             · 1 row left out: no month value · 2 gaps in total: null or not a number"
        );
        let none = Summary {
            rows: 0,
            limit: None,
            without_x: 0,
        };
        assert_eq!(summary(&data(none, 0)), "No rows to chart");
    }

    #[test]
    fn names_the_y_columns_in_its_menu() {
        let columns: Vec<Column> = ["month", "total", "refunds"]
            .into_iter()
            .map(|name| Column {
                name: name.to_owned(),
                kind: ValueKind::Undeclared,
            })
            .collect();
        assert_eq!(y_label(&columns, &[]), "Y: none");
        assert_eq!(y_label(&columns, &[1]), "Y: total");
        assert_eq!(y_label(&columns, &[1, 2]), "Y: total and 1 more");
    }
}
