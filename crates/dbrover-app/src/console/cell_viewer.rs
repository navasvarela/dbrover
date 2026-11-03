//! The value panel beside a console's results, showing the selected cell's whole value, for
//! text too long for the grid. It follows the grid's selection. JSON objects and arrays show
//! as a tree, or as text pretty-printed, and XML (SQL Server's plans and `FOR XML` results) is
//! indented.

use dbrover_document::Value;
use egui::{Id, RichText, ScrollArea, TextEdit};

use super::{CellAt, ViewedCell};
use crate::icons;
use crate::json_tree::JsonTree;
use crate::widgets::IconButton;

/// The value panel: shut, or open on the cell it shows, if any yet.
#[derive(Default)]
pub enum ValuePanel {
    #[default]
    Closed,
    Open(Option<ViewedCell>),
}

impl ValuePanel {
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open(_))
    }

    /// The grid toolbar's switch: opens the panel empty, until a cell is selected, or shuts it.
    pub fn toggle(&mut self) {
        *self = match self {
            Self::Closed => Self::Open(None),
            Self::Open(_) => Self::Closed,
        };
    }

    /// Shows `cell`, opening the panel if it is shut.
    pub fn view(&mut self, cell: ViewedCell) {
        *self = Self::Open(Some(cell));
    }

    /// Whether the open panel should show the cell at `at`, the grid's selection, instead of
    /// the one it shows.
    pub fn follows_to(&self, at: CellAt) -> bool {
        match self {
            Self::Closed => false,
            Self::Open(shown) => shown.as_ref().is_none_or(|shown| shown.at != at),
        }
    }
}

/// Draws the open panel's contents: the cell's column and **Close**, then its value.
pub fn show(ui: &mut egui::Ui, panel: &mut ValuePanel, console: u64) {
    let ValuePanel::Open(shown) = panel else {
        return;
    };
    let title = shown.as_ref().map_or_else(
        || "Value".to_owned(),
        |viewed| format!("Value of {}", viewed.column),
    );
    let close = ui
        .horizontal(|ui| {
            ui.strong(title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                IconButton::new(icons::X, "Close the value panel")
                    .show(ui)
                    .clicked()
            })
            .inner
        })
        .inner;
    match shown {
        Some(viewed) => contents(ui, viewed, console),
        None => {
            ui.weak("Select a cell to see its whole value here.");
        }
    }
    if close {
        *panel = ValuePanel::Closed;
    }
}

/// How a value with a tree is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShownAs {
    Tree,
    Text,
}

/// `text` read as JSON, if it is an object or an array: a scalar has nothing to unfold.
pub fn json_tree(text: &str) -> Option<Value> {
    if !text.trim_start().starts_with(['{', '[']) {
        return None;
    }
    Value::parse(text)
        .ok()
        .filter(|value| matches!(value, Value::Object(_) | Value::Array(_)))
}

fn contents(ui: &mut egui::Ui, viewed: &mut ViewedCell, console: u64) {
    let Some(value) = &viewed.value else {
        ui.label(RichText::new("NULL").italics().weak());
        return;
    };
    if viewed.tree.is_none() {
        copyable_text_view(ui, value);
        return;
    }
    ui.horizontal(|ui| {
        ui.selectable_value(&mut viewed.shown_as, ShownAs::Tree, "Tree");
        ui.selectable_value(&mut viewed.shown_as, ShownAs::Text, "Text");
    });
    match viewed.shown_as {
        ShownAs::Tree => tree_view(ui, viewed, console),
        ShownAs::Text => copyable_text_view(ui, value),
    }
}

/// A JSON value as a tree, with **Copy** for it as stored.
fn tree_view(ui: &mut egui::Ui, viewed: &ViewedCell, console: u64) {
    let (Some(stored), Some(value)) = (&viewed.value, &viewed.tree) else {
        return;
    };
    ui.horizontal(|ui| {
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(stored.to_owned());
        }
        ui.weak("Right-click a field to copy it or its path.");
    });
    ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        JsonTree {
            id: Id::new(("cell_viewer_tree", console)),
            root: &viewed.column,
            value,
            syntax: viewed.syntax,
        }
        .show(ui);
    });
}

/// A [`text_view`] with **Copy** for the value as stored above it.
fn copyable_text_view(ui: &mut egui::Ui, value: &str) {
    if ui.button("Copy").clicked() {
        ui.ctx().copy_text(value.to_owned());
    }
    text_view(ui, value);
}

/// Long text laid out for reading, JSON and XML indented. The cell viewer's, and a command
/// action's output's.
pub fn text_view(ui: &mut egui::Ui, value: &str) {
    let pretty = pretty(value);
    if let Some((structure, _)) = &pretty {
        ui.weak(structure.note());
    }
    read_only_text(ui, pretty.as_ref().map_or(value, |(_, text)| text.as_str()));
}

/// Text to read, select and copy from, but not to change, filling the space left.
pub fn read_only_text(ui: &mut egui::Ui, mut shown: &str) {
    ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        // A `&str` buffer makes the editor read-only but still selectable.
        ui.add(
            TextEdit::multiline(&mut shown)
                .code_editor()
                .desired_width(f32::INFINITY),
        );
    });
}

/// A value the viewer lays out for reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Structure {
    Json,
    Xml,
}

impl Structure {
    fn note(self) -> &'static str {
        match self {
            Self::Json => "JSON, formatted for reading; Copy copies it as stored.",
            Self::Xml => "XML, indented for reading; Copy copies it as stored.",
        }
    }
}

fn pretty(value: &str) -> Option<(Structure, String)> {
    pretty_json(value)
        .map(|text| (Structure::Json, text))
        .or_else(|| pretty_xml(value).map(|text| (Structure::Xml, text)))
}

/// Spaces per level of nesting in indented XML, as JSON gets.
const XML_INDENT: usize = 2;

/// `value` with one tag per line, indented by how deeply it nests, if it is XML: SQL Server's
/// `SHOWPLAN_XML` and `FOR XML` give one long line. Text inside an element stays on its
/// line (`<a>text</a>`). `None` if the tags don't nest, so anything else is shown as stored.
fn pretty_xml(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if !trimmed.starts_with('<') || !trimmed.ends_with('>') {
        return None;
    }
    let mut lines = Vec::new();
    let mut depth: usize = 0;
    let mut rest = trimmed;
    while !rest.is_empty() {
        if !rest.starts_with('<') {
            let text_end = rest.find('<').unwrap_or(rest.len());
            let text = rest[..text_end].trim();
            if !text.is_empty() {
                lines.push((depth, text.to_owned()));
            }
            rest = &rest[text_end..];
            continue;
        }
        let end = tag_end(rest)?;
        let tag = &rest[..end];
        rest = &rest[end..];
        if tag.starts_with("</") {
            depth = depth.checked_sub(1)?;
            lines.push((depth, tag.to_owned()));
        } else if tag.ends_with("/>") || tag.starts_with("<?") || tag.starts_with("<!") {
            lines.push((depth, tag.to_owned()));
        } else {
            let text_end = rest.find('<').unwrap_or(rest.len());
            let (text, after) = rest.split_at(text_end);
            if !text.trim().is_empty() && after.starts_with("</") {
                let close = tag_end(after)?;
                lines.push((depth, format!("{tag}{}{}", text.trim(), &after[..close])));
                rest = &after[close..];
            } else {
                lines.push((depth, tag.to_owned()));
                depth += 1;
            }
        }
    }
    (depth == 0).then(|| {
        lines
            .iter()
            .map(|(depth, line)| format!("{}{line}", " ".repeat(depth * XML_INDENT)))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// Where the tag `text` starts with ends: just past its `>`, which doesn't count inside a
/// quoted attribute value.
fn tag_end(text: &str) -> Option<usize> {
    let mut quote = None;
    for (index, c) in text.char_indices().skip(1) {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(index + 1),
            (None, _) => {}
        }
    }
    None
}

/// `value` indented, if it is a JSON object or array, with its numbers as written. Scalars are
/// left alone: a number or a quoted word looks the same either way.
fn pretty_json(value: &str) -> Option<String> {
    json_tree(value).map(|tree| tree.to_pretty_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_prints_objects_and_arrays_keeping_key_order() {
        assert_eq!(
            pretty_json(r#"{"b":1.10,"a":[true,null]}"#).as_deref(),
            Some("{\n  \"b\": 1.10,\n  \"a\": [\n    true,\n    null\n  ]\n}")
        );
        for not_json in ["42", "\"word\"", "{not json", "plain text"] {
            assert_eq!(pretty_json(not_json), None, "{not_json}");
        }
    }

    #[test]
    fn indents_xml_keeping_text_with_its_element() {
        let plan = r#"<?xml version="1.0"?><ShowPlanXML Version="1.564"><Stmt Text="a > 1"><Op/><Name>scan</Name></Stmt></ShowPlanXML>"#;
        assert_eq!(
            pretty(plan),
            Some((
                Structure::Xml,
                "<?xml version=\"1.0\"?>\n<ShowPlanXML Version=\"1.564\">\n  <Stmt Text=\"a > 1\">\n    \
                 <Op/>\n    <Name>scan</Name>\n  </Stmt>\n</ShowPlanXML>"
                    .to_owned()
            ))
        );
        for not_xml in ["<a><b></a>", "<unclosed", "a < b and c > d", "</a>"] {
            assert_eq!(pretty_xml(not_xml), None, "{not_xml}");
        }
    }
}
