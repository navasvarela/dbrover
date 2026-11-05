//! Drawing a compare tab: the sides at the top, then the differences on the left and the
//! selected object's definitions side by side on the right.

use dbrover_schema::{
    Change, Comparison, Difference, FieldChange, ObjectKind, Script, TableDifferences, side_by_side,
};
use egui::{CentralPanel, Color32, ComboBox, Frame, Panel, RichText, ScrollArea, TextEdit, Ui};

use super::{Candidate, CompareAction, CompareId, CompareTab, Outcome, Side, SideState};
use crate::widgets::IconButton;
use crate::{form, icons};

/// The differences list's starting width; it can be dragged.
const LIST_WIDTH: f32 = 340.0;
const SCHEMA_FIELD_WIDTH: f32 = 140.0;
const KINDS: [ObjectKind; 3] = [ObjectKind::Table, ObjectKind::Index, ObjectKind::View];

pub(super) fn show(
    ui: &mut Ui,
    id: CompareId,
    tab: &mut CompareTab,
    candidates: &[Candidate],
) -> Option<CompareAction> {
    tab.compare_if_read(candidates);
    let mut action = sides_bar(ui, id, tab, candidates);
    side_errors(ui, tab);
    ui.separator();
    match &tab.outcome {
        None => {
            if !tab.sides.iter().any(Side::is_reading) {
                ui.weak("Pick both sides, then Compare.");
            }
            return action;
        }
        Some(Outcome::Refused(error)) => {
            ui.colored_label(ui.visuals().error_fg_color, error.to_string());
            return action;
        }
        Some(Outcome::Compared { comparison, script }) => {
            let left = tab.sides[0].data_source;
            action = action.or(toolbar(ui, comparison, script.as_ref(), left));
        }
    }
    Panel::left(egui::Id::new(("compare_differences", id)))
        .resizable(true)
        .default_size(LIST_WIDTH)
        .frame(Frame::new())
        .show(ui, |ui| differences(ui, tab));
    CentralPanel::default()
        .frame(Frame::new())
        .show(ui, |ui| definitions(ui, tab, candidates));
    action
}

/// **Left** and **Right**, each a data source and a schema, **⇄** between them, then
/// **Compare** or **Compare again**.
fn sides_bar(
    ui: &mut Ui,
    id: CompareId,
    tab: &mut CompareTab,
    candidates: &[Candidate],
) -> Option<CompareAction> {
    ui.horizontal(|ui| {
        let left_dialect = tab.sides[0]
            .data_source
            .and_then(|source| candidates.iter().find(|candidate| candidate.id == source))
            .map(|candidate| candidate.dialect);
        let mut changed = side_picker(ui, (id, "left"), "Left", &mut tab.sides[0], candidates);
        let swap = IconButton::new(icons::ARROWS_LEFT_RIGHT, "Swap sides")
            .tooltip("Swap the sides: the script then turns the right into the left".to_owned())
            .show(ui);
        if swap.clicked() {
            tab.swap();
        }
        let same_engine: Vec<Candidate> = candidates
            .iter()
            .filter(|candidate| left_dialect.is_none_or(|dialect| candidate.dialect == dialect))
            .cloned()
            .collect();
        changed |= side_picker(ui, (id, "right"), "Right", &mut tab.sides[1], &same_engine);
        if changed {
            tab.outcome = None;
            tab.selected = None;
        }
        let reading = tab.sides.iter().any(Side::is_reading);
        if reading {
            ui.spinner();
        }
        let label = if tab.outcome.is_some() {
            "Compare again"
        } else {
            "Compare"
        };
        let enabled = tab.picked().is_some() && !reading;
        form::primary_button(ui, label, enabled)
            .on_hover_text("Read both schemas and compare them")
            .clicked()
            .then(|| tab.start_reading())
            .flatten()
    })
    .inner
}

/// A side's data source and schema. Returns whether either changed.
fn side_picker(
    ui: &mut Ui,
    salt: (CompareId, &str),
    label: &str,
    side: &mut Side,
    candidates: &[Candidate],
) -> bool {
    let label_id = ui.label(label).id;
    let before = side.data_source;
    let shown = side
        .data_source
        .and_then(|id| candidates.iter().find(|candidate| candidate.id == id))
        .map_or("Pick a data source", |candidate| candidate.name.as_str());
    ComboBox::from_id_salt(("compare_data_source", salt.0, salt.1))
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for candidate in candidates {
                ui.selectable_value(
                    &mut side.data_source,
                    Some(candidate.id),
                    candidate.name.as_str(),
                );
            }
        })
        .response
        .on_hover_text(format!("The {} side's data source", label.to_lowercase()));
    let edited = ui
        .add(
            TextEdit::singleline(&mut side.schema)
                .hint_text("schema")
                .desired_width(SCHEMA_FIELD_WIDTH),
        )
        .labelled_by(label_id)
        .on_hover_text(format!("The {} side's schema", label.to_lowercase()))
        .changed();
    let changed = edited || before != side.data_source;
    if changed {
        side.state = SideState::Unread;
    }
    changed
}

/// A side that couldn't be read says why, under the bar; the other side still loads.
fn side_errors(ui: &mut Ui, tab: &CompareTab) {
    for (label, side) in ["Left", "Right"].into_iter().zip(&tab.sides) {
        if let SideState::Failed(error) = &side.state {
            ui.colored_label(ui.visuals().error_fg_color, format!("{label}: {error}"));
        }
    }
}

/// **Open script in console**, **Copy script**, and how many objects differ.
fn toolbar(
    ui: &mut Ui,
    comparison: &Comparison,
    script: Option<&Script>,
    left: Option<dbrover_core::DataSourceId>,
) -> Option<CompareAction> {
    ui.horizontal(|ui| {
        let can_open = script.is_some() && left.is_some();
        let open = form::primary_button(ui, "Open script in console", can_open)
            .on_hover_text("Put the script that turns the left into the right in a new console of the left's data source, to read and run there");
        if ui
            .add_enabled(script.is_some(), egui::Button::new("Copy script"))
            .clicked()
            && let Some(script) = script
        {
            ui.ctx().copy_text(script.sql.clone());
        }
        let count = comparison.len();
        if count == 0 {
            ui.label("The schemas are the same.");
        } else {
            let plural = if count == 1 { "" } else { "s" };
            ui.label(format!("{count} difference{plural}"));
        }
        if let Some(script) = script
            && !script.not_written.is_empty()
        {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{} not written in the script; see its comments.",
                    script.not_written.len()
                ),
            );
        }
        let text = script.map(|script| script.sql.clone());
        match (open.clicked(), left, text) {
            (true, Some(data_source), Some(text)) => {
                Some(CompareAction::OpenInConsole { data_source, text })
            }
            _ => None,
        }
    })
    .inner
}

/// The differences, grouped by kind, each with its mark and summary; a changed table lists
/// what changed in it.
fn differences(ui: &mut Ui, tab: &mut CompareTab) {
    let Some(Outcome::Compared { comparison, .. }) = &tab.outcome else {
        return;
    };
    ui.horizontal(|ui| {
        for kind in KINDS {
            let mut shown = !tab.hidden.contains(&kind);
            if ui.checkbox(&mut shown, kind.plural()).changed() {
                if shown {
                    tab.hidden.remove(&kind);
                } else {
                    tab.hidden.insert(kind);
                }
            }
        }
    });
    let mut picked = None;
    ScrollArea::vertical()
        .id_salt("compare_differences")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if comparison.is_empty() {
                ui.weak("The schemas are the same.");
            }
            let lists = [
                (
                    ObjectKind::Table,
                    rows_of(&comparison.tables, table_summary),
                ),
                (
                    ObjectKind::Index,
                    rows_of(&comparison.indexes, |fields| fields_summary(fields)),
                ),
                (
                    ObjectKind::View,
                    rows_of(&comparison.views, |fields| fields_summary(fields)),
                ),
            ];
            for (kind, rows) in lists {
                if rows.is_empty() || tab.hidden.contains(&kind) {
                    continue;
                }
                ui.add_space(4.0);
                ui.strong(format!("{} ({})", kind.plural(), rows.len()));
                for row in rows {
                    let selected = tab.selected.as_ref() == Some(&(kind, row.name.to_owned()));
                    let label = format!("{} {}", row.mark, row.name);
                    let response = ui.selectable_label(selected, label);
                    if response.clicked() {
                        picked = Some((kind, row.name.to_owned()));
                    }
                    if !row.summary.is_empty() {
                        ui.indent(("summary", kind, row.name), |ui| {
                            for line in row.summary {
                                ui.weak(line);
                            }
                        });
                    }
                }
            }
        });
    if picked.is_some() {
        tab.selected = picked;
    }
}

/// A line of the differences list.
struct Row<'a> {
    name: &'a str,
    mark: &'static str,
    /// What differs, a line each; empty for an object on one side only.
    summary: Vec<String>,
}

fn rows_of<'a, D>(
    differences: &'a [Difference<D>],
    summary: impl Fn(&D) -> Vec<String>,
) -> Vec<Row<'a>> {
    differences
        .iter()
        .map(|difference| Row {
            name: &difference.name,
            mark: difference.change.mark(),
            summary: match &difference.change {
                Change::Differs(details) => summary(details),
                Change::OnlyLeft => vec!["only on the left".to_owned()],
                Change::OnlyRight => vec!["only on the right".to_owned()],
            },
        })
        .collect()
}

/// "2 columns added, 1 changed", then each column and constraint that differs.
fn table_summary(table: &TableDifferences) -> Vec<String> {
    let mut lines = vec![table.summary()];
    for column in &table.columns {
        match &column.change {
            Change::Differs(fields) => {
                for field in fields {
                    lines.push(format!("~ {}: {field}", column.name));
                }
            }
            change => lines.push(format!("{} column {}", change.mark(), column.name)),
        }
    }
    for constraint in &table.constraints {
        let line = match &constraint.change {
            Change::Differs(change) => format!("~ {change}"),
            change => format!("{} {} {}", change.mark(), constraint.kind, constraint.name),
        };
        lines.push(line);
    }
    lines
}

fn fields_summary<F: std::fmt::Display + Copy + 'static>(fields: &[FieldChange<F>]) -> Vec<String> {
    fields.iter().map(ToString::to_string).collect()
}

/// The selected object's definition from each side, side by side, the lines that differ
/// marked.
fn definitions(ui: &mut Ui, tab: &CompareTab, candidates: &[Candidate]) {
    let Some((kind, name)) = &tab.selected else {
        ui.weak("Select a difference to see its definition on each side.");
        return;
    };
    let [left, right] = &tab.sides;
    let left_text = left
        .schema()
        .and_then(|schema| schema.definition(*kind, name))
        .unwrap_or_default();
    let right_text = right
        .schema()
        .and_then(|schema| schema.definition(*kind, name))
        .unwrap_or_default();
    let changed = ui.visuals().warn_fg_color.gamma_multiply(0.18);
    ScrollArea::both()
        .id_salt("compare_definitions")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("compare_definitions_grid")
                .num_columns(2)
                .spacing([16.0, 0.0])
                .show(ui, |ui| {
                    ui.strong(format!("Left: {}", super::describe(left, candidates)));
                    ui.strong(format!("Right: {}", super::describe(right, candidates)));
                    ui.end_row();
                    for row in side_by_side(&left_text, &right_text) {
                        let background = if row.differs {
                            changed
                        } else {
                            Color32::TRANSPARENT
                        };
                        ui.label(code(row.left, background));
                        ui.label(code(row.right, background));
                        ui.end_row();
                    }
                });
        });
}

fn code(line: Option<&str>, background: Color32) -> RichText {
    RichText::new(line.unwrap_or(""))
        .monospace()
        .background_color(background)
}
