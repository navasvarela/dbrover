//! A statement's plan, drawn from the one value its explain returned: as a tree of steps with
//! the costliest highlighted, or as text (W10 in `docs/well-rounded.md`).

use std::collections::BTreeSet;

use dbrover_core::Row;
use dbrover_plan::{Heat, Line, Measure, Plan, PlanFormat, Step, format_rows};
use egui::{Label, RichText, ScrollArea, Sense, TextEdit, Ui, vec2};
use egui_extras::{Column, TableBuilder};

use crate::icons;
use crate::theme::Palette;

/// How far each level of the tree is indented.
const LEVEL_INDENT: f32 = 14.0;
/// The height kept under the tree for the selected step's facts.
const DETAILS_HEIGHT: f32 = 110.0;
const SHARE_BAR_WIDTH: f32 = 48.0;
const FIGURE_WIDTH: f32 = 70.0;
/// How strongly a highlighted step's row is tinted with the warning colour: the costliest
/// stands out at a glance, a warm one less, and both stay readable.
const COSTLIEST_TINT: f32 = 0.30;
const WARM_TINT: f32 = 0.12;

/// The plan of an explained statement, once it is read.
pub(super) struct PlanView {
    format: PlanFormat,
    /// `None` until the statement finishes; then the plan, or why its value isn't one.
    read: Option<Result<ReadPlan, String>>,
    shown_as: ShownAs,
}

/// Which view of the plan is shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ShownAs {
    #[default]
    Tree,
    Text,
}

struct ReadPlan {
    plan: Plan,
    /// The value as the database returned it, for copying.
    source: String,
    /// The text plan, written once.
    text: String,
    /// Steps folded in the tree, by their place in the plan's outline.
    folded: BTreeSet<usize>,
    /// The step whose facts are shown under the tree, by its place in the outline: the
    /// costliest at first.
    selected: Option<usize>,
}

impl PlanView {
    pub(super) fn new(format: PlanFormat) -> Self {
        Self {
            format,
            read: None,
            shown_as: ShownAs::default(),
        }
    }

    pub(super) fn format(&self) -> PlanFormat {
        self.format
    }

    /// Reads the plan from the rows the statement returned: one row of one value.
    pub(super) fn read(&mut self, rows: &[Row]) {
        let source = rows.first().and_then(|row| row.first()).cloned().flatten();
        let Some(source) = source else {
            self.read = Some(Err("the database returned no plan".to_owned()));
            return;
        };
        self.read = Some(
            self.format
                .parse(&source)
                .map(|plan| ReadPlan::new(plan, source))
                .map_err(|error| error.to_string()),
        );
    }

    /// Whether the plan was read, so it is drawn in place of the rows.
    pub(super) fn is_read(&self) -> bool {
        matches!(self.read, Some(Ok(_)))
    }

    /// Why the value the database returned couldn't be read as a plan.
    pub(super) fn failure(&self) -> Option<&str> {
        match &self.read {
            Some(Err(reason)) => Some(reason),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn plan(&self) -> Option<&Plan> {
        match &self.read {
            Some(Ok(read)) => Some(&read.plan),
            _ => None,
        }
    }
}

impl ReadPlan {
    fn new(plan: Plan, source: String) -> Self {
        let selected = plan
            .outline()
            .iter()
            .position(|line| line.weight.heat == Heat::Costliest);
        Self {
            text: plan.text(),
            plan,
            source,
            folded: BTreeSet::new(),
            selected,
        }
    }
}

/// Whether the plan's bar offers Explain analyse, which runs the statement again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AnalyseButton {
    /// The console's language can't analyse a plan.
    Hidden,
    Enabled,
    /// Something is running.
    Disabled,
}

/// Draws the plan, if it was read: the bar to switch views, analyse and copy, then the tree or
/// the text. Returns whether Explain analyse was clicked.
pub(super) fn show(ui: &mut Ui, view: &mut PlanView, analyse: AnalyseButton) -> bool {
    let notation = view.format.notation();
    let Some(Ok(read)) = &mut view.read else {
        return false;
    };
    let mut analyse_clicked = false;
    let palette = Palette::for_visuals(ui.visuals());
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut view.shown_as, ShownAs::Tree, "Tree");
        ui.selectable_value(&mut view.shown_as, ShownAs::Text, "Text");
        ui.separator();
        ui.label(summary(&read.plan));
        ui.separator();
        if analyse != AnalyseButton::Hidden {
            analyse_clicked = ui
                .add_enabled(
                    analyse == AnalyseButton::Enabled,
                    egui::Button::new("Explain analyse"),
                )
                .on_hover_text(
                    "Run the statement and show what each step actually did: its rows, loops \
                     and time",
                )
                .clicked();
        }
        if ui
            .button("Copy text")
            .on_hover_text("Copy the plan as text")
            .clicked()
        {
            ui.ctx().copy_text(read.text.clone());
        }
        if ui
            .button(format!("Copy {notation}"))
            .on_hover_text(format!(
                "Copy the plan as the database returned it, in {notation}, for tools that read it"
            ))
            .clicked()
        {
            ui.ctx().copy_text(read.source.clone());
        }
    });
    match view.shown_as {
        ShownAs::Tree => tree(ui, read, &palette),
        ShownAs::Text => text(ui, &read.text),
    }
    analyse_clicked
}

/// What kind of plan this is, and what it costs in all.
fn summary(plan: &Plan) -> String {
    let ran = plan.root.actual.map(|actual| actual.total_ms());
    match (ran, plan.root.estimate.total_cost) {
        (Some(Some(ms)), _) => format!("Analysed plan · ran in {ms:.3} ms"),
        (Some(None), _) => "Analysed plan".to_owned(),
        (None, Some(cost)) => format!("Estimated plan · cost {cost:.2}"),
        (None, None) => "Estimated plan".to_owned(),
    }
}

fn text(ui: &mut Ui, text: &str) {
    ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        ui.add(
            TextEdit::multiline(&mut &*text)
                .code_editor()
                .desired_width(f32::INFINITY),
        );
    });
}

/// The steps as a table, one row a step, indented under its parent, then the selected step's
/// facts.
fn tree(ui: &mut Ui, read: &mut ReadPlan, palette: &Palette) {
    let outline = read.plan.outline();
    let shown = unfolded(&outline, &read.folded);
    let figures = Figure::of(&read.plan);
    let share_hint = match read.plan.measure() {
        Measure::Cost => {
            "How much of the plan's estimated cost the step accounts for, without the steps under it"
        }
        Measure::Time => "How much of the plan's time the step took, without the steps under it",
    };
    let row_height = ui.spacing().interact_size.y;
    let tree_height = (ui.available_height() - DETAILS_HEIGHT).max(row_height * 3.0);
    let mut clicked = None;
    let mut toggled = None;
    let mut table = TableBuilder::new(ui)
        .id_salt("plan_tree")
        .max_scroll_height(tree_height)
        .auto_shrink([false, true])
        .column(Column::remainder().at_least(220.0).clip(true))
        .column(Column::exact(SHARE_BAR_WIDTH + 44.0));
    for _ in figures {
        table = table.column(Column::auto().at_least(FIGURE_WIDTH));
    }
    table
        .header(row_height, |mut header| {
            header.col(|ui| {
                ui.strong("Step");
            });
            header.col(|ui| {
                ui.strong("Share").on_hover_text(share_hint);
            });
            for figure in figures {
                header.col(|ui| {
                    ui.strong(figure.heading()).on_hover_text(figure.hint());
                });
            }
        })
        .body(|body| {
            body.rows(row_height, shown.len(), |mut row| {
                let Some(&index) = shown.get(row.index()) else {
                    return;
                };
                let Some(line) = outline.get(index) else {
                    return;
                };
                let tint = tint(line.weight.heat, palette);
                row.set_selected(read.selected == Some(index));
                row.col(|ui| {
                    fill(ui, tint);
                    let folded = read.folded.contains(&index);
                    // The step's place in the plan, not in the rows shown, names its toggle,
                    // so folding a step above it doesn't hand its clicks to another.
                    let cell = ui.push_id(("plan_step", index), |ui| {
                        ui.horizontal(|ui| step_cell(ui, line, folded, palette))
                            .inner
                    });
                    match cell.inner {
                        Some(CellClick::Toggle) => toggled = Some(index),
                        Some(CellClick::Select) => clicked = Some(index),
                        None => {}
                    }
                });
                row.col(|ui| {
                    fill(ui, tint);
                    ui.horizontal(|ui| share_cell(ui, line, palette));
                });
                for figure in figures {
                    row.col(|ui| {
                        fill(ui, tint);
                        figure.cell(ui, line.step, palette);
                    });
                }
            });
        });
    if let Some(index) = toggled {
        if !read.folded.remove(&index) {
            read.folded.insert(index);
        }
    } else if clicked.is_some() {
        read.selected = clicked;
    }
    ui.separator();
    selected_step(ui, read.selected.and_then(|index| outline.get(index)));
}

/// The selected step's figures and facts, under the tree.
fn selected_step(ui: &mut Ui, selected: Option<&Line<'_>>) {
    ScrollArea::vertical()
        .id_salt("plan_details")
        .auto_shrink([false, true])
        .show(ui, |ui| match selected {
            Some(line) => {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(line.step.title());
                    ui.weak(figures_in_words(line.step));
                });
                details(ui, line.step);
            }
            None => {
                ui.weak("Select a step to see what the database says about it.");
            }
        });
}

/// A column of figures beside the steps: the estimate's, then, once the plan is analysed, what
/// happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Figure {
    Cost,
    Rows,
    ActualRows,
    Loops,
    Time,
}

impl Figure {
    const ESTIMATED: &[Self] = &[Self::Cost, Self::Rows];
    const ANALYSED: &[Self] = &[
        Self::Cost,
        Self::Rows,
        Self::ActualRows,
        Self::Loops,
        Self::Time,
    ];

    fn of(plan: &Plan) -> &'static [Self] {
        if plan.is_analysed() {
            Self::ANALYSED
        } else {
            Self::ESTIMATED
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Self::Cost => "Cost",
            Self::Rows => "Rows",
            Self::ActualRows => "Actual rows",
            Self::Loops => "Loops",
            Self::Time => "Time",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Cost => {
                "The estimated cost, in the database's own units, of the step and every step \
                 under it"
            }
            Self::Rows => "The rows the step was expected to return each time it ran",
            Self::ActualRows => {
                "The rows the step returned each time it ran, on average; flagged when ten \
                 times more or fewer than expected"
            }
            Self::Loops => "How many times the step ran",
            Self::Time => {
                "How long the step took, every time it ran and the steps under it included"
            }
        }
    }

    fn cell(self, ui: &mut Ui, step: &Step, palette: &Palette) {
        let actual = step.actual;
        let text = match self {
            Self::Cost => step.estimate.total_cost.map(|cost| format!("{cost:.2}")),
            Self::Rows => step.estimate.rows.map(format_rows),
            Self::ActualRows => {
                return actual_rows(ui, step, palette);
            }
            Self::Loops => actual.map(|actual| format_rows(actual.loops)),
            Self::Time => actual
                .and_then(|actual| actual.total_ms())
                .map(|ms| format!("{ms:.3} ms")),
        };
        if let Some(text) = text {
            figure(ui, RichText::new(text));
        }
    }
}

/// The rows the step returned, flagged when the estimate was ten times off, or that it never
/// ran.
fn actual_rows(ui: &mut Ui, step: &Step, palette: &Palette) {
    let Some(actual) = step.actual else {
        return;
    };
    if actual.loops == 0.0 {
        figure(ui, RichText::new("never ran").weak());
        return;
    }
    let Some(returned) = actual.rows else {
        return;
    };
    let rows = format_rows(returned);
    if !step.is_misestimated() {
        figure(ui, RichText::new(rows));
        return;
    }
    let expected = step.estimate.rows.map(format_rows).unwrap_or_default();
    let factor = step.misestimate().unwrap_or_default();
    let direction = if returned > step.estimate.rows.unwrap_or_default() {
        "more"
    } else {
        "fewer"
    };
    figure(
        ui,
        RichText::new(format!("{} {rows}", icons::WARNING)).color(palette.warning),
    )
    .on_hover_text(format!(
        "Expected {expected} rows, returned {rows}: {factor:.0} times {direction}. A wrong \
         estimate is the usual reason the database picks a slow plan."
    ));
}

/// The places in the outline of the steps not hidden under a folded step.
fn unfolded(outline: &[Line<'_>], folded: &BTreeSet<usize>) -> Vec<usize> {
    let mut shown = Vec::new();
    let mut index = 0;
    while let Some(line) = outline.get(index) {
        shown.push(index);
        index += if folded.contains(&index) {
            line.descendants + 1
        } else {
            1
        };
    }
    shown
}

/// What a click in a step's cell asks for.
enum CellClick {
    /// Fold or unfold the steps under it.
    Toggle,
    /// Show its facts under the tree.
    Select,
}

/// The step's toggle, heading, operation and target, indented to its depth, its facts shown on
/// hover.
///
/// The step's words take its clicks, rather than the table's row: a row that senses clicks
/// takes them from the toggle on top of it, and folding never happened.
fn step_cell(ui: &mut Ui, line: &Line<'_>, folded: bool, palette: &Palette) -> Option<CellClick> {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a plan is never deep enough for its depth to lose precision as a float"
    )]
    let indent = line.depth as f32 * LEVEL_INDENT;
    ui.add_space(indent);
    let step = line.step;
    let toggled = if step.children.is_empty() {
        ui.add_space(ui.spacing().icon_width);
        false
    } else {
        crate::explorer::expander(ui, !folded, &step.title()).clicked()
    };
    if let Some(heading) = &step.heading {
        ui.weak(heading);
    }
    let mut words = ui.add(
        Label::new(RichText::new(&step.operation).strong())
            .selectable(false)
            .sense(Sense::click()),
    );
    if let Some(target) = &step.target {
        words |= ui.add(
            Label::new(target)
                .selectable(false)
                .truncate()
                .sense(Sense::click()),
        );
    }
    if line.weight.heat == Heat::Costliest {
        ui.label(RichText::new("Costliest").small().color(palette.warning));
    }
    let selected = words.on_hover_ui(|ui| details(ui, step)).clicked();
    if toggled {
        Some(CellClick::Toggle)
    } else if selected {
        Some(CellClick::Select)
    } else {
        None
    }
}

/// The step's own share of the plan, as a bar and a percentage.
fn share_cell(ui: &mut Ui, line: &Line<'_>, palette: &Palette) {
    let share = line.weight.share.clamp(0.0, 1.0);
    let (rect, _) = ui.allocate_exact_size(
        vec2(SHARE_BAR_WIDTH, ui.spacing().interact_size.y * 0.4),
        Sense::hover(),
    );
    let color = match line.weight.heat {
        Heat::Costliest | Heat::Warm => palette.warning,
        Heat::Cool => palette.text_weak,
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a share between 0 and 1 fits a float of either size"
    )]
    let filled = rect.width() * share as f32;
    ui.painter()
        .rect_filled(rect, 2.0, palette.tint(palette.text_weak, 0.15));
    ui.painter().rect_filled(
        egui::Rect::from_min_size(rect.min, vec2(filled, rect.height())),
        2.0,
        color,
    );
    ui.label(format!("{:.0}%", share * 100.0));
}

/// A number in a column, right-aligned so its digits line up.
fn figure(ui: &mut Ui, text: RichText) -> egui::Response {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(text)
    })
    .inner
}

/// The step's figures in words, for the selected step's facts.
fn figures_in_words(step: &Step) -> String {
    let estimate = step.estimate;
    let cost = match (estimate.startup_cost, estimate.total_cost) {
        (Some(startup), Some(total)) => {
            Some(format!("cost {startup:.2} to first row, {total:.2} in all"))
        }
        (None, Some(total)) => Some(format!("cost {total:.2}")),
        (_, None) => None,
    };
    let rows = estimate
        .rows
        .map(|rows| format!("{} rows expected", format_rows(rows)));
    let actual = step.actual.map(|actual| {
        if actual.loops == 0.0 {
            return "never ran".to_owned();
        }
        let returned = actual.rows.map_or_else(
            || "ran".to_owned(),
            |rows| format!("{} rows returned", format_rows(rows)),
        );
        let took = actual
            .time
            .map(|time| format!(" in {:.3} ms", time.last_row_ms))
            .unwrap_or_default();
        format!("{returned}{took}, {} times", format_rows(actual.loops))
    });
    cost.into_iter()
        .chain(rows)
        .chain(actual)
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Everything the database says about the step besides its figures.
fn details(ui: &mut Ui, step: &Step) {
    if step.details.is_empty() {
        ui.weak("Nothing more about this step.");
        return;
    }
    egui::Grid::new(("plan_step_details", step.title()))
        .num_columns(2)
        .show(ui, |ui| {
            for detail in &step.details {
                ui.weak(&detail.label);
                ui.add(Label::new(&detail.value).wrap());
                ui.end_row();
            }
        });
}

fn tint(heat: Heat, palette: &Palette) -> Option<egui::Color32> {
    match heat {
        Heat::Costliest => Some(palette.tint(palette.warning, COSTLIEST_TINT)),
        Heat::Warm => Some(palette.tint(palette.warning, WARM_TINT)),
        Heat::Cool => None,
    }
}

/// Paints the cell's background, out to its neighbours so the row reads as one band.
fn fill(ui: &Ui, tint: Option<egui::Color32>) {
    if let Some(color) = tint {
        let rect = ui
            .max_rect()
            .expand2(vec2(ui.spacing().item_spacing.x, 0.0));
        ui.painter().rect_filled(rect, 0.0, color);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn plan_rows(text: &str) -> Vec<Row> {
        vec![vec![Some(text.to_owned())]]
    }

    #[test]
    fn reads_the_plan_from_its_one_value_or_says_why_not() {
        let mut view = PlanView::new(PlanFormat::PostgresJson);
        view.read(&plan_rows(
            r#"[{"Plan": {"Node Type": "Result", "Total Cost": 0.01}}]"#,
        ));
        assert!(view.is_read());
        assert_eq!(view.plan().unwrap().root.operation, "Result");

        view.read(&plan_rows("Result  (cost=0.00..0.01 rows=1 width=4)"));
        assert!(!view.is_read());
        assert!(view.failure().unwrap().contains("isn't JSON"));

        view.read(&[]);
        assert_eq!(view.failure(), Some("the database returned no plan"));
    }

    /// The join PostgreSQL's test server planned for W10a's fixtures.
    const JOIN: &str = include_str!("../../../dbrover-plan/fixtures/postgres/join.json");

    #[test]
    fn draws_the_steps_folds_them_and_shows_the_selected_steps_facts() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;

        let mut view = PlanView::new(PlanFormat::PostgresJson);
        view.read(&plan_rows(JOIN));
        let mut harness = crate::test_support::ui_harness(move |ui| {
            show(ui, &mut view, AnalyseButton::Enabled);
        });
        harness.get_by_label("Costliest");
        // The costliest step is selected at first, so its facts show under the tree.
        assert!(harness.query_all_by_label("on orders o").count() >= 1);

        harness.get_by_label("Collapse Hash Join").click();
        harness.run();
        assert!(harness.query_by_label("on customers c").is_none());
        harness.get_by_label("Expand Hash Join").click();
        harness.run();
        harness.get_by_label("on customers c");

        harness.get_by_label("Hash Join").click();
        harness.run();
        harness.get_by_label("(o.customer_id = c.id)");

        harness.get_by_label("Text").click();
        harness.run();
        let text = harness
            .get_by_role(Role::MultilineTextInput)
            .value()
            .unwrap_or_default();
        assert!(text.contains("->  Hash Join"), "{text}");
    }

    #[test]
    fn hides_the_steps_under_a_folded_one() {
        let plan = PlanFormat::PostgresJson
            .parse(
                r#"[{"Plan": {"Node Type": "Limit", "Plans": [
                    {"Node Type": "Sort", "Plans": [{"Node Type": "Seq Scan"}]},
                    {"Node Type": "Result"}]}}]"#,
            )
            .unwrap();
        let outline = plan.outline();
        assert_eq!(unfolded(&outline, &BTreeSet::new()), [0, 1, 2, 3]);
        assert_eq!(unfolded(&outline, &BTreeSet::from([1])), [0, 1, 3]);
        assert_eq!(unfolded(&outline, &BTreeSet::from([0, 1])), [0]);
    }
}
