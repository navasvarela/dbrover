//! A diagram's tab: a toolbar, then the tables as boxes and their keys as lines, in a `Scene`
//! that pans and zooms.

use dbrover_core::TableDescription;
use egui::epaint::CubicBezierShape;
use egui::epaint::FontsView;
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Modal, Painter, Rect, Response,
    Scene, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};

use dbrover_store::DiagramKey;

use super::geometry::{self, HEADER_HEIGHT, MARK_WIDTH, PADDING, ROW_HEIGHT, TYPE_GAP, arrowhead};
use super::model::{BoxShape, Diagram, KeyLine, TableState};
use super::{DiagramTab, TabAction};
use crate::fonts;
use crate::form;
use crate::icons;
use crate::picture::PictureFormat;
use crate::theme::Palette;
use crate::widgets;

const HEADER_TEXT_SIZE: f32 = 13.0;
const COLUMN_TEXT_SIZE: f32 = 12.5;
const TYPE_TEXT_SIZE: f32 = 11.5;
const CORNER_RADIUS: u8 = 6;
/// Room under the last row, so it doesn't touch the border.
const BOTTOM_PADDING: f32 = 4.0;
const ARROW_SIZE: f32 = 7.0;
/// Around the diagram when **Fit** shows it all.
const FIT_MARGIN: f32 = 32.0;
/// From far enough out to see a large schema whole, to a little closer than 1:1; text blurs
/// past that.
const ZOOM_RANGE: std::ops::RangeInclusive<f32> = 0.05..=1.5;
/// How much of an error a box shows, in characters; the rest is in its tooltip.
const ERROR_PREVIEW: usize = 48;

pub(super) fn show(ui: &mut Ui, tab: &mut DiagramTab, key: &DiagramKey) -> Option<TabAction> {
    let palette = Palette::for_visuals(ui.visuals());
    let exported = toolbar(ui, tab);
    let deleted = confirm_delete(ui, tab, key);
    measure(ui, &mut tab.diagram);
    if tab.diagram.place() {
        tab.fit = true;
    }
    // A diagram is shown whole once its boxes are placed: on opening, and after Arrange.
    if tab.fit
        && let Some(bounds) = tab.diagram.bounds()
    {
        tab.scene_rect = bounds.expand(FIT_MARGIN);
        tab.fit = false;
    }
    let background = ui.available_rect_before_wrap();
    ui.painter().rect_filled(background, 0.0, palette.panel);
    let mut scene_rect = tab.scene_rect;
    Scene::new()
        .zoom_range(ZOOM_RANGE)
        .show(ui, &mut scene_rect, |ui| {
            draw(ui, &mut tab.diagram, &palette);
            #[cfg(test)]
            {
                tab.to_screen = ui
                    .ctx()
                    .layer_transform_to_global(ui.layer_id())
                    .unwrap_or_default();
            }
        });
    tab.scene_rect = scene_rect;
    deleted.then_some(TabAction::Delete).or(exported)
}

/// **Delete diagram…**'s question, while it is asked; whether it was confirmed.
fn confirm_delete(ui: &Ui, tab: &mut DiagramTab, key: &DiagramKey) -> bool {
    if !tab.confirming_delete {
        return false;
    }
    let modal = Modal::new(Id::new(("delete_diagram", key))).show(ui.ctx(), |ui| {
        ui.heading(format!("Delete the diagram {}?", key.name));
        ui.label("Its file is removed. The database and its tables are untouched.");
        ui.separator();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let deleted = form::danger_button(ui, "Delete", true).clicked();
            let cancelled = ui.button("Cancel").clicked();
            (deleted, cancelled)
        })
        .inner
    });
    let (deleted, cancelled) = modal.inner;
    if deleted || cancelled || modal.should_close() {
        tab.confirming_delete = false;
    }
    deleted
}

/// **Arrange**, **Fit**, **Export ›** and **Delete diagram…**, then how loading and the last
/// save and export went. Returns the export chosen, if one was.
fn toolbar(ui: &mut Ui, tab: &mut DiagramTab) -> Option<TabAction> {
    let palette = Palette::for_visuals(ui.visuals());
    let mut action = None;
    widgets::toolbar(ui, |ui| {
        let arrange = widgets::icon_text(ui, icons::GRAPH, palette.primary, "Arrange");
        let arrange = widgets::name(ui.button(arrange), WidgetType::Button, "Arrange", true);
        if arrange.on_hover_text("Lay every table out again").clicked() {
            tab.diagram.arrange();
        }
        let fit = widgets::icon_text(ui, icons::FRAME_CORNERS, palette.text, "Fit");
        let fit = widgets::name(ui.button(fit), WidgetType::Button, "Fit", true);
        if fit.on_hover_text("Show the whole diagram").clicked() {
            tab.fit = true;
        }
        let export = widgets::icon_text(ui, icons::EXPORT, palette.text, "Export");
        let menu = ui.menu_button(export, |ui| {
            for format in PictureFormat::ALL {
                if ui.button(format.label()).clicked() {
                    action = Some(TabAction::Export(format));
                }
            }
        });
        widgets::name(menu.response, WidgetType::Button, "Export", true);
        let delete = widgets::icon_text(ui, icons::TRASH, palette.text, "Delete diagram…");
        let delete = widgets::name(
            ui.button(delete),
            WidgetType::Button,
            "Delete diagram…",
            true,
        );
        if delete.clicked() {
            tab.confirming_delete = true;
        }
        ui.add_space(8.0);
        if let Some(error) = &tab.save_error {
            ui.colored_label(palette.error, format!("Could not save: {error}"));
        }
        match &tab.exported {
            Some(Ok(path)) => {
                ui.weak(format!("Exported to {}", path.display()));
            }
            Some(Err(error)) => {
                ui.colored_label(palette.error, format!("Could not export: {error}"));
            }
            None => {}
        }
        let (described, total) = tab.diagram.progress();
        if tab.diagram.is_loading() {
            ui.spinner();
            ui.weak(format!("Loading {described} of {total} tables…"));
        } else {
            ui.weak(match total {
                1 => "1 table".to_owned(),
                total => format!("{total} tables"),
            });
        }
    });
    action
}

/// What a box shows under its header.
enum Rows<'a> {
    Columns(&'a TableDescription),
    Note(String),
}

fn rows(state: &TableState) -> Rows<'_> {
    match state {
        TableState::Described(description) => Rows::Columns(description),
        TableState::Loading => Rows::Note("Loading…".to_owned()),
        TableState::Missing => Rows::Note("missing".to_owned()),
        TableState::Failed(error) => Rows::Note(error_preview(error)),
    }
}

fn error_preview(error: &str) -> String {
    match error.char_indices().nth(ERROR_PREVIEW) {
        Some((cut, _)) => format!("{}…", &error[..cut]),
        None => error.to_owned(),
    }
}

/// Measures the boxes whose text changed since they were last measured.
fn measure(ui: &Ui, diagram: &mut Diagram) {
    let unmeasured: Vec<usize> = (0..diagram.tables().len())
        .filter(|&index| diagram.tables()[index].shape.is_none())
        .collect();
    for index in unmeasured {
        let table = &diagram.tables()[index];
        let label = diagram.label(&table.name);
        let shape = ui.fonts_mut(|fonts| box_shape(fonts, &label, &rows(&table.state)));
        diagram.set_shape(index, shape);
    }
}

fn box_shape(fonts: &mut FontsView<'_>, label: &str, rows: &Rows<'_>) -> BoxShape {
    let header = PADDING + MARK_WIDTH + text_width(fonts, label, header_font()) + PADDING;
    let (names, types, count) = match rows {
        Rows::Columns(description) => {
            let columns = &description.columns;
            let names = widest(
                fonts,
                columns.iter().map(|c| c.name.as_str()),
                column_font(),
            );
            let types = widest(
                fonts,
                columns.iter().map(|c| c.data_type.as_str()),
                type_font(),
            );
            (names, types, columns.len())
        }
        Rows::Note(note) => (text_width(fonts, note, column_font()), 0.0, 1),
    };
    let type_x = PADDING + MARK_WIDTH + names + TYPE_GAP;
    let body = type_x + types + PADDING;
    let height = HEADER_HEIGHT + ROW_HEIGHT * count.max(1) as f32 + BOTTOM_PADDING;
    BoxShape {
        size: vec2(header.max(body).ceil(), height),
        type_x,
    }
}

fn widest<'a>(
    fonts: &mut FontsView<'_>,
    texts: impl Iterator<Item = &'a str>,
    font: FontId,
) -> f32 {
    texts
        .map(|text| text_width(fonts, text, font.clone()))
        .fold(0.0, f32::max)
}

fn text_width(fonts: &mut FontsView<'_>, text: &str, font: FontId) -> f32 {
    fonts
        .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER)
        .size()
        .x
}

fn header_font() -> FontId {
    fonts::semibold(HEADER_TEXT_SIZE)
}

fn column_font() -> FontId {
    FontId::proportional(COLUMN_TEXT_SIZE)
}

fn type_font() -> FontId {
    FontId::monospace(TYPE_TEXT_SIZE)
}

/// What the user did to a table's box this frame.
enum TableEdit {
    Move(usize, Vec2),
    AddRelated(usize),
    Remove(usize),
}

/// Lets each box be dragged or right-clicked, then draws the lines under the boxes.
fn draw(ui: &mut Ui, diagram: &mut Diagram, palette: &Palette) {
    let mut hovered = None;
    let mut edits = Vec::new();
    for index in 0..diagram.tables().len() {
        let Some(rect) = diagram.rect(index) else {
            continue;
        };
        let table = &diagram.tables()[index];
        let label = diagram.label(&table.name);
        let id = Id::new(("diagram_table", &table.name));
        let response = ui.interact(rect, id, Sense::click_and_drag());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, &label));
        if response.hovered() || response.dragged() {
            hovered = Some(index);
        }
        if response.dragged() {
            edits.push(TableEdit::Move(index, response.drag_delta()));
        }
        let response = match &table.state {
            TableState::Failed(error) => response.on_hover_text(error),
            _ => response,
        };
        table_menu(&response, index, &mut edits);
    }
    let painter = ui.painter();
    for line in diagram.key_lines() {
        let lit = hovered.is_some_and(|index| index == line.from || index == line.to);
        paint_line(painter, diagram, line, lit, palette);
    }
    for index in 0..diagram.tables().len() {
        paint_box(painter, diagram, index, hovered == Some(index), palette);
    }
    // After drawing, since removing a table moves the ones after it.
    for edit in edits {
        match edit {
            TableEdit::Move(index, delta) => diagram.move_table(index, delta),
            TableEdit::AddRelated(index) => diagram.add_related(index),
            TableEdit::Remove(index) => diagram.remove(index),
        }
    }
}

fn table_menu(response: &Response, index: usize, edits: &mut Vec<TableEdit>) {
    response.context_menu(|ui| {
        if widgets::menu_item(ui, icons::GRAPH, "Add related tables").clicked() {
            edits.push(TableEdit::AddRelated(index));
        }
        if widgets::menu_item(ui, icons::X, "Remove from diagram").clicked() {
            edits.push(TableEdit::Remove(index));
        }
    });
}

fn paint_line(painter: &Painter, diagram: &Diagram, line: KeyLine, lit: bool, palette: &Palette) {
    let Some(path) = diagram.key_path(line) else {
        return;
    };
    let (color, width) = if lit {
        (palette.primary, 2.0)
    } else {
        (palette.text_weak, 1.2)
    };
    for curve in &path {
        painter.add(CubicBezierShape::from_points_stroke(
            *curve,
            false,
            Color32::TRANSPARENT,
            Stroke::new(width, color),
        ));
    }
    if let Some(last) = path.last() {
        let head = arrowhead(last, ARROW_SIZE);
        painter.add(Shape::convex_polygon(head.to_vec(), color, Stroke::NONE));
    }
}

fn paint_box(painter: &Painter, diagram: &Diagram, index: usize, lit: bool, palette: &Palette) {
    let (Some(rect), Some(table)) = (diagram.rect(index), diagram.tables().get(index)) else {
        return;
    };
    let radius = CornerRadius::same(CORNER_RADIUS);
    painter.rect_filled(rect, radius, palette.field);
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_HEIGHT));
    let header_radius = CornerRadius {
        nw: CORNER_RADIUS,
        ne: CORNER_RADIUS,
        sw: 0,
        se: 0,
    };
    painter.rect_filled(header, header_radius, palette.selection);
    let border = if lit { palette.primary } else { palette.border };
    if table.state == TableState::Missing {
        dashed_border(painter, rect, Stroke::new(1.0, palette.warning));
    } else {
        painter.rect_stroke(rect, radius, Stroke::new(1.0, border), StrokeKind::Inside);
    }
    let middle = header.center().y;
    let table_icon = icons::node(dbrover_core::NodeKind::Table);
    painter.text(
        pos2(rect.left() + PADDING, middle),
        Align2::LEFT_CENTER,
        table_icon.glyph,
        fonts::icon(HEADER_TEXT_SIZE),
        table_icon.role.color(palette),
    );
    painter.text(
        pos2(rect.left() + PADDING + MARK_WIDTH, middle),
        Align2::LEFT_CENTER,
        diagram.label(&table.name),
        header_font(),
        palette.text,
    );
    let type_x = table.shape.map_or(0.0, |shape| shape.type_x);
    match rows(&table.state) {
        Rows::Columns(description) => paint_columns(painter, rect, type_x, description, palette),
        Rows::Note(note) => {
            let color = match table.state {
                TableState::Failed(_) => palette.error,
                _ => palette.text_weak,
            };
            painter.text(
                pos2(
                    rect.left() + PADDING + MARK_WIDTH,
                    geometry::row_middle(rect, Some(0)),
                ),
                Align2::LEFT_CENTER,
                note,
                column_font(),
                color,
            );
        }
    }
}

fn paint_columns(
    painter: &Painter,
    rect: Rect,
    type_x: f32,
    description: &TableDescription,
    palette: &Palette,
) {
    for (row, column) in description.columns.iter().enumerate() {
        let y = geometry::row_middle(rect, Some(row));
        if let Some((glyph, color)) = column_mark(description, &column.name, palette) {
            painter.text(
                pos2(rect.left() + PADDING, y),
                Align2::LEFT_CENTER,
                glyph,
                fonts::icon(COLUMN_TEXT_SIZE),
                color,
            );
        }
        painter.text(
            pos2(rect.left() + PADDING + MARK_WIDTH, y),
            Align2::LEFT_CENTER,
            &column.name,
            column_font(),
            palette.text,
        );
        painter.text(
            pos2(rect.left() + type_x, y),
            Align2::LEFT_CENTER,
            &column.data_type,
            type_font(),
            palette.text_weak,
        );
    }
}

/// A key for a primary key's column, coloured as the explorer colours keys, or a link for a
/// foreign key's; nothing for any other column.
fn column_mark(
    description: &TableDescription,
    column: &str,
    palette: &Palette,
) -> Option<(&'static str, Color32)> {
    if description.primary_key.iter().any(|key| key == column) {
        let key = icons::node(dbrover_core::NodeKind::Constraint);
        return Some((key.glyph, key.role.color(palette)));
    }
    let in_foreign_key = description
        .foreign_keys
        .iter()
        .any(|key| key.columns.iter().any(|c| c == column));
    in_foreign_key.then_some((icons::LINK_SIMPLE, palette.secondary))
}

fn dashed_border(painter: &Painter, rect: Rect, stroke: Stroke) {
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    painter.extend(Shape::dashed_line(&corners, stroke, 6.0, 4.0));
}
