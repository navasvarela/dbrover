//! The report's **Runs**: its newest runs side by side, a column each, with the step that
//! dominates each run marked, the step that dominates them all named, the hints, and **Clear
//! history** (`docs/reporting.md`, "The report's tab in RP4").

use std::collections::BTreeSet;

use dbrover_core::ByteCount;
use dbrover_report::grouped;
use dbrover_store::{ReportName, ReportRun, RunOutcome, StepMetrics};
use egui::{Grid, RichText, Ui};
use jiff::tz::TimeZone;

use super::insights::{self, StepKey};
use super::{ReportAction, Reports};
use crate::icons;
use crate::theme::Palette;
use crate::widgets;

/// How a run's start heads its column, in the system's time zone.
const STARTED_FORMAT: &str = "%Y-%m-%d %H:%M";

pub fn show(
    ui: &mut Ui,
    reports: &mut Reports,
    name: &ReportName,
    actions: &mut Vec<ReportAction>,
) {
    let palette = Palette::for_visuals(ui.visuals());
    ui.add_space(12.0);
    let listed = match reports.runs(name) {
        Some(Ok(runs)) => runs.len(),
        None | Some(Err(_)) => 0,
    };
    heading(ui, &palette, reports, name, listed, actions);
    let runs = match reports.runs(name) {
        None => {
            ui.weak("Loading…");
            return;
        }
        Some(Err(error)) => {
            ui.colored_label(palette.error, format!("The runs can't be read: {error}"));
            return;
        }
        Some(Ok(runs)) if runs.is_empty() => {
            ui.weak("No run yet. Each run is kept here with what each step took.");
            return;
        }
        Some(Ok(runs)) => runs.clone(),
    };
    table(ui, &palette, name, &runs);
    ui.weak("An output is written as its rows come, so its time overlaps the step before it.");
    if let Some(dominant) = insights::dominant_throughout(&runs) {
        ui.add_space(4.0);
        ui.label(format!(
            "{} takes the most time in every run listed: {:.0}% of the newest.",
            dominant.step,
            dominant.share * 100.0
        ));
    }
    for hint in insights::hints(&runs) {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(widgets::glyph(icons::LIGHTBULB).color(palette.warning));
            ui.label(hint.to_string());
        });
    }
}

fn table(ui: &mut Ui, palette: &Palette, name: &ReportName, runs: &[ReportRun]) {
    let time_zone = TimeZone::system();
    let dominants: Vec<Option<StepKey>> = runs
        .iter()
        .map(|run| insights::dominant(run).map(|dominant| dominant.step))
        .collect();
    Grid::new(("report_runs", name.as_str()))
        .striped(true)
        .num_columns(runs.len() + 1)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            ui.label("");
            for run in runs {
                let started = run.started_at.to_zoned(time_zone.clone());
                ui.label(RichText::new(started.strftime(STARTED_FORMAT).to_string()).strong());
            }
            ui.end_row();

            ui.weak("Ended");
            for run in runs {
                outcome(ui, palette, run);
            }
            ui.end_row();

            ui.weak("Total");
            for run in runs {
                ui.label(seconds(
                    u64::try_from(run.elapsed.as_millis()).unwrap_or(u64::MAX),
                ));
            }
            ui.end_row();

            let params: BTreeSet<&String> = runs.iter().flat_map(|run| run.params.keys()).collect();
            for param in params {
                ui.weak(param);
                for run in runs {
                    match run.params.get(param) {
                        Some(value) => ui.label(RichText::new(value).monospace()),
                        None => ui.weak("—"),
                    };
                }
                ui.end_row();
            }

            for key in insights::step_keys(runs) {
                ui.weak(key.to_string());
                for (run, dominant) in runs.iter().zip(&dominants) {
                    let step = run.steps.iter().find(|step| StepKey::of(step) == key);
                    match step {
                        Some(step) => {
                            step_cell(ui, palette, run, step, dominant.as_ref() == Some(&key))
                        }
                        None => {
                            ui.weak("—");
                        }
                    }
                }
                ui.end_row();
            }
        });
}

fn outcome(ui: &mut Ui, palette: &Palette, run: &ReportRun) {
    match &run.outcome {
        RunOutcome::Finished => {
            widgets::status(ui, icons::CHECK_CIRCLE, palette.success, "finished");
        }
        RunOutcome::Failed(error) => {
            ui.horizontal(|ui| {
                widgets::status(ui, icons::X_CIRCLE, palette.error, "failed");
            })
            .response
            .on_hover_text(format!("At {}: {}", error.step, error.message));
        }
        RunOutcome::Cancelled => {
            widgets::status(ui, icons::PROHIBIT, palette.text_weak, "cancelled");
        }
    }
}

/// A step's time and rows; the step that dominated its run in the accent colour.
fn step_cell(ui: &mut Ui, palette: &Palette, run: &ReportRun, step: &StepMetrics, dominant: bool) {
    let text = match (step, insights::own_time(run, step)) {
        (StepMetrics::Attached { .. }, _) | (_, None) => "attached".to_owned(),
        (StepMetrics::Source(source), Some(time)) => {
            format!("{} · {} rows", seconds(time), grouped(source.rows))
        }
        (StepMetrics::Combine(combine), Some(time)) => {
            format!("{} · {} rows", seconds(time), grouped(combine.rows))
        }
        (StepMetrics::Output(output), Some(time)) => {
            format!("{} · {}", seconds(time), ByteCount(output.size))
        }
    };
    let text = if dominant {
        RichText::new(text).strong().color(palette.primary)
    } else {
        RichText::new(text)
    };
    let response = ui.label(text);
    let details = details(step);
    let details = if dominant {
        format!("{details}\nThe step that took the longest share of this run.")
    } else {
        details
    };
    response.on_hover_text(details);
}

/// Everything a step measured, for its cell's tooltip.
fn details(step: &StepMetrics) -> String {
    match step {
        StepMetrics::Source(source) => {
            let mut lines = vec![
                format!("From {}", source.data_source),
                format!("{} rows, {}", grouped(source.rows), ByteCount(source.bytes)),
                format!("{} in all", seconds(source.elapsed_ms)),
            ];
            lines.extend(
                source
                    .first_row_ms
                    .map(|ms| format!("First row after {}", seconds(ms))),
            );
            lines.extend(source.batches.map(|batches| format!("{batches} batches")));
            lines.join("\n")
        }
        StepMetrics::Attached { data_source, .. } => {
            format!("{data_source}, attached and read in place by the combine step")
        }
        StepMetrics::Combine(combine) => {
            let unknown = || "unknown".to_owned();
            [
                format!(
                    "{} rows, {}",
                    grouped(combine.rows),
                    ByteCount(combine.bytes)
                ),
                format!("{} in all", seconds(combine.elapsed_ms)),
                format!(
                    "DuckDB's peak memory: {}",
                    combine
                        .peak_memory
                        .map_or_else(unknown, |bytes| ByteCount(bytes).to_string())
                ),
                format!(
                    "Spilled to disk at most: {}",
                    combine
                        .peak_temp_size
                        .map_or_else(unknown, |bytes| ByteCount(bytes).to_string())
                ),
            ]
            .join("\n")
        }
        StepMetrics::Output(output) => format!(
            "{}\n{} rows, {}",
            output.file,
            grouped(output.rows),
            ByteCount(output.size)
        ),
    }
}

/// `1.2 s`, as the rest of the tab writes a time.
fn seconds(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

/// **Runs**, and **Clear history** beside it once there is a run to clear, which asks before
/// it deletes. At the top, where it stays in reach however many steps the table has.
fn heading(
    ui: &mut Ui,
    palette: &Palette,
    reports: &mut Reports,
    name: &ReportName,
    count: usize,
    actions: &mut Vec<ReportAction>,
) {
    let state = reports.state_mut(name);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Runs").strong());
        if count == 0 {
            return;
        }
        if !state.confirm_clear {
            if ui.small_button("Clear history").clicked() {
                state.confirm_clear = true;
            }
            return;
        }
        let runs = if count == 1 { "run" } else { "runs" };
        ui.colored_label(
            palette.error,
            format!("Delete every run of this report kept in the history ({count} {runs} listed)?"),
        );
        if ui.button("Delete").clicked() {
            state.confirm_clear = false;
            actions.push(ReportAction::ClearRuns(name.clone()));
        }
        if ui.button("Keep").clicked() {
            state.confirm_clear = false;
        }
    });
}
