//! The report's **Compare** (`docs/reports-at-scale.md`, "Comparing two runs", RS1f): two of
//! the runs the history keeps, each step's numbers side by side with the large changes marked,
//! the parameters and limits that differ, and whether the report's file changed.

use dbrover_store::{ReportName, ReportRun, RunOutcome};
use egui::{Grid, RichText, Ui};
use jiff::tz::TimeZone;

use super::compare::{self, Change, Comparison, Differs};
use super::insights::seconds;
use super::runs_table::STARTED_FORMAT;
use super::{ComparedRuns, Reports};
use crate::icons;
use crate::theme::Palette;
use crate::widgets;

pub fn show(ui: &mut Ui, reports: &mut Reports, name: &ReportName) {
    let palette = Palette::for_visuals(ui.visuals());
    ui.add_space(12.0);
    let runs = match reports.runs(name) {
        Some(Ok(runs)) => runs.clone(),
        Some(Err(error)) => {
            ui.colored_label(palette.error, format!("The runs can't be read: {error}"));
            return;
        }
        None => {
            ui.weak("Loading…");
            return;
        }
    };
    if runs.len() < 2 {
        ui.weak("Two runs are needed to compare. Each run is kept with what each step took.");
        return;
    }
    let last = runs.len() - 1;
    let chosen = reports
        .state(name)
        .map(|state| state.compare_runs)
        .unwrap_or_default();
    let chosen = ComparedRuns {
        before: chosen.before.min(last),
        after: chosen.after.min(last),
    };
    let mut picked = chosen;
    Grid::new("report_compare_runs")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            picked.before = pick_run(ui, "Before", "report_compare_before", &runs, chosen.before);
            ui.end_row();
            picked.after = pick_run(ui, "After", "report_compare_after", &runs, chosen.after);
            ui.end_row();
        });
    if picked != chosen {
        reports.state_mut(name).compare_runs = picked;
    }
    if picked.before == picked.after {
        ui.add_space(6.0);
        ui.weak("Pick two different runs.");
        return;
    }
    let comparison = compare::compare(&runs[picked.before], &runs[picked.after]);
    ui.add_space(8.0);
    file_line(ui, &palette, &comparison);
    changes(ui, &palette, &comparison);
    for (heading, differs) in [
        ("Parameters that differ", &comparison.params),
        ("Limits that differ", &comparison.limits),
    ] {
        if !differs.is_empty() {
            ui.add_space(8.0);
            ui.strong(heading);
            settings(ui, heading, differs);
        }
    }
}

fn pick_run(ui: &mut Ui, caption: &str, id: &str, runs: &[ReportRun], chosen: usize) -> usize {
    let time_zone = TimeZone::system();
    let label = |run: &ReportRun| {
        let started = run.started_at.to_zoned(time_zone.clone());
        let outcome = match run.outcome {
            RunOutcome::Finished => "finished",
            RunOutcome::Failed(_) => "failed",
            RunOutcome::Cancelled => "cancelled",
        };
        let millis = u64::try_from(run.elapsed.as_millis()).unwrap_or(u64::MAX);
        format!(
            "{}, {outcome} in {}",
            started.strftime(STARTED_FORMAT),
            seconds(millis)
        )
    };
    let mut picked = chosen;
    let caption = ui.label(caption);
    egui::ComboBox::from_id_salt(id)
        .selected_text(label(&runs[chosen]))
        .show_ui(ui, |ui| {
            for (index, run) in runs.iter().enumerate() {
                ui.selectable_value(&mut picked, index, label(run));
            }
        })
        .response
        .labelled_by(caption.id);
    picked
}

fn file_line(ui: &mut Ui, palette: &Palette, comparison: &Comparison) {
    match comparison.file_changed {
        Some(true) => widgets::status(
            ui,
            icons::GIT_DIFF,
            palette.warning,
            "The report's file changed between the two runs.",
        ),
        Some(false) => {
            ui.weak("The two runs ran the same report file.");
        }
        None => {}
    }
    ui.add_space(4.0);
}

/// A row a number: the step, what, before, after and the change, marked when large.
fn changes(ui: &mut Ui, palette: &Palette, comparison: &Comparison) {
    Grid::new("report_compare_steps")
        .striped(true)
        .num_columns(5)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            for heading in ["Step", "", "Before", "After", "Change"] {
                ui.weak(heading);
            }
            ui.end_row();
            let run = comparison
                .run
                .iter()
                .map(|change| ("Run".to_owned(), change));
            let steps = comparison.steps.iter().flat_map(|step| {
                step.changes
                    .iter()
                    .map(|change| (step.step.to_string(), change))
            });
            let mut last_step = String::new();
            for (step, change) in run.chain(steps) {
                if step == last_step {
                    ui.label("");
                } else {
                    ui.label(&step);
                    last_step = step;
                }
                change_row(ui, palette, change);
                ui.end_row();
            }
        });
}

fn change_row(ui: &mut Ui, palette: &Palette, change: &Change) {
    ui.weak(change.what);
    ui.label(change.before_text());
    ui.label(change.after_text());
    let ratio = change.describe_ratio();
    if change.marked() {
        ui.label(RichText::new(format!("{ratio} ▲")).color(palette.warning))
            .on_hover_text("Changed by more than half");
    } else {
        ui.weak(ratio);
    }
}

fn settings(ui: &mut Ui, id: &str, differs: &[Differs]) {
    Grid::new(("report_compare_settings", id))
        .num_columns(3)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            for differ in differs {
                ui.weak(&differ.name);
                ui.label(differ.before.as_deref().unwrap_or("—"));
                ui.label(differ.after.as_deref().unwrap_or("—"));
                ui.end_row();
            }
        });
}
