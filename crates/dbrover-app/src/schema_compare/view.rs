//! Drawing a compare tab: the sides at the top, then the differences on the left and the
//! selected object's definitions side by side on the right.

use dbrover_schema::{
    Change, Comparison, Difference, ObjectKind, Script, TableDifferences, field_details,
    side_by_side,
};
use egui::{CentralPanel, Color32, ComboBox, Frame, Panel, RichText, ScrollArea, TextEdit, Ui};

use dbrover_core::DataSourceId;
use dbrover_sql::Dialect;

use super::{
    Candidate, CompareAction, CompareId, CompareTab, Outcome, Side, SideIndex, SideState, Source,
};
use crate::widgets::{self, IconButton};
use crate::{form, icons};

/// The differences list's starting width; it can be dragged.
const LIST_WIDTH: f32 = 340.0;
const SCHEMA_FIELD_WIDTH: f32 = 140.0;
const SNAPSHOT_ITEM: &str = "Snapshot file\u{2026}";
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
            let targets = script_targets(tab, candidates);
            action = action.or(toolbar(ui, comparison, script.as_ref(), &targets));
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

/// **Left** and **Right**, each a data source and a schema or a snapshot file, **Swap sides**
/// between them, then **Compare** or **Compare again**.
fn sides_bar(
    ui: &mut Ui,
    id: CompareId,
    tab: &mut CompareTab,
    candidates: &[Candidate],
) -> Option<CompareAction> {
    ui.horizontal(|ui| {
        let left_dialect = side_dialect(&tab.sides[0], candidates);
        let (mut changed, mut action) =
            side_picker(ui, id, SideIndex::Left, &mut tab.sides[0], candidates);
        let swap = IconButton::new(icons::ARROWS_LEFT_RIGHT, "Swap sides")
            .tooltip("Swap the sides: the script then turns the right into the left".to_owned())
            .show(ui);
        if swap.clicked() {
            tab.swap();
        }
        let same_engine = of_dialect(candidates, left_dialect);
        let (right_changed, right_action) =
            side_picker(ui, id, SideIndex::Right, &mut tab.sides[1], &same_engine);
        changed |= right_changed;
        action = action.or(right_action);
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
        let enabled = tab.readings().is_some() && !reading;
        let compare = form::primary_button(ui, label, enabled)
            .on_hover_text("Read both schemas and compare them")
            .clicked();
        action.or_else(|| compare.then(|| tab.start_reading()).flatten())
    })
    .inner
}

/// The engine a side is known to be of: its data source's, or a snapshot's once read.
fn side_dialect(side: &Side, candidates: &[Candidate]) -> Option<Dialect> {
    side.data_source()
        .and_then(|source| candidates.iter().find(|candidate| candidate.id == source))
        .map(|candidate| candidate.dialect)
        .or_else(|| side.schema().map(|schema| schema.dialect))
}

fn of_dialect(candidates: &[Candidate], dialect: Option<Dialect>) -> Vec<Candidate> {
    candidates
        .iter()
        .filter(|candidate| dialect.is_none_or(|dialect| candidate.dialect == dialect))
        .cloned()
        .collect()
}

/// What a side's picker chose this frame.
enum Pick {
    DataSource(DataSourceId),
    Snapshot,
}

/// A side's data source and schema, or its snapshot file, and **Save snapshot…** for a live
/// side. Returns whether the side changed, and what to ask the app for.
fn side_picker(
    ui: &mut Ui,
    id: CompareId,
    which: SideIndex,
    side: &mut Side,
    candidates: &[Candidate],
) -> (bool, Option<CompareAction>) {
    let noun = which.label().to_lowercase();
    let label_id = ui.label(which.label()).id;
    let current = side.data_source();
    let shown = match &side.source {
        Source::Snapshot(_) => "Snapshot file",
        Source::Live { .. } => current
            .and_then(|id| candidates.iter().find(|candidate| candidate.id == id))
            .map_or("Pick a data source", |candidate| candidate.name.as_str()),
    };
    let mut pick = None;
    ComboBox::from_id_salt(("compare_data_source", id, which.label()))
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for candidate in candidates {
                if ui
                    .selectable_label(current == Some(candidate.id), candidate.name.as_str())
                    .clicked()
                {
                    pick = Some(Pick::DataSource(candidate.id));
                }
            }
            ui.separator();
            let snapshot = matches!(side.source, Source::Snapshot(_));
            if ui.selectable_label(snapshot, SNAPSHOT_ITEM).clicked() {
                pick = Some(Pick::Snapshot);
            }
        })
        .response
        .on_hover_text(format!("The {noun} side's data source, or a snapshot file"));
    let mut changed = false;
    let mut action = None;
    match pick {
        Some(Pick::DataSource(data_source)) => {
            let schema = match &side.source {
                Source::Live { schema, .. } => schema.clone(),
                Source::Snapshot(_) => String::new(),
            };
            side.source = Source::Live {
                data_source: Some(data_source),
                schema,
            };
            changed = current != Some(data_source);
        }
        Some(Pick::Snapshot) => {
            action = Some(CompareAction::PickSnapshot {
                tab: id,
                side: which,
            })
        }
        None => {}
    }
    match &mut side.source {
        Source::Live { schema, .. } => {
            changed |= ui
                .add(
                    TextEdit::singleline(schema)
                        .hint_text("schema")
                        .desired_width(SCHEMA_FIELD_WIDTH),
                )
                .labelled_by(label_id)
                .on_hover_text(format!("The {noun} side's schema"))
                .changed();
        }
        Source::Snapshot(path) => {
            ui.label(super::file_name(path))
                .on_hover_text(path.display().to_string());
        }
    }
    let live = side.live_schema();
    let save = IconButton::new(
        icons::FLOPPY_DISK,
        &format!("Save a snapshot of the {noun} side"),
    )
    .enabled(live.is_some())
    .show(ui);
    if save.clicked()
        && let Some(live) = live
    {
        action = action.or(Some(CompareAction::SaveSnapshot(live)));
    }
    if changed {
        side.state = SideState::Unread;
    }
    (changed, action)
}

/// A side that couldn't be read says why, under the bar; the other side still loads.
fn side_errors(ui: &mut Ui, tab: &CompareTab) {
    for (label, side) in ["Left", "Right"].into_iter().zip(&tab.sides) {
        if let SideState::Failed(error) = &side.state {
            ui.colored_label(ui.visuals().error_fg_color, format!("{label}: {error}"));
        }
    }
}

/// The data sources the script may open on: the left's own when it is live; when it is a
/// snapshot, any of the same engine, for the user to pick.
fn script_targets(tab: &CompareTab, candidates: &[Candidate]) -> ScriptTarget {
    let left = &tab.sides[0];
    match left.data_source() {
        Some(data_source) => ScriptTarget::Left(data_source),
        None => ScriptTarget::Pick(of_dialect(candidates, side_dialect(left, candidates))),
    }
}

enum ScriptTarget {
    Left(DataSourceId),
    Pick(Vec<Candidate>),
}

/// **Open script in console**, **Copy script**, and how many objects differ.
fn toolbar(
    ui: &mut Ui,
    comparison: &Comparison,
    script: Option<&Script>,
    target: &ScriptTarget,
) -> Option<CompareAction> {
    ui.horizontal(|ui| {
        let opened = open_script(ui, script, target);
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
        Some(CompareAction::OpenInConsole {
            data_source: opened?,
            text: text?,
        })
    })
    .inner
}

/// **Open script in console**: a button opening on the left's data source, or, when the left
/// is a snapshot, a menu of the data sources it may open on. The data source picked, if any.
fn open_script(
    ui: &mut Ui,
    script: Option<&Script>,
    target: &ScriptTarget,
) -> Option<DataSourceId> {
    const OPEN: &str = "Open script in console";
    match target {
        ScriptTarget::Left(data_source) => form::primary_button(ui, OPEN, script.is_some())
            .on_hover_text(
                "Put the script that turns the left into the right in a new console of the \
                 left's data source, to read and run there",
            )
            .clicked()
            .then_some(*data_source),
        ScriptTarget::Pick(candidates) => {
            let enabled = script.is_some() && !candidates.is_empty();
            ui.add_enabled_ui(enabled, |ui| {
                widgets::menu_button(ui, OPEN, |ui| {
                    let mut chosen = None;
                    for candidate in candidates {
                        if ui.button(candidate.name.as_str()).clicked() {
                            chosen = Some(candidate.id);
                            ui.close();
                        }
                    }
                    chosen
                })
                .inner
                .flatten()
            })
            .inner
        }
    }
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
                    rows_of(&comparison.tables, TableDifferences::details),
                ),
                (
                    ObjectKind::Index,
                    rows_of(&comparison.indexes, |fields| field_details(fields)),
                ),
                (
                    ObjectKind::View,
                    rows_of(&comparison.views, |fields| field_details(fields)),
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
