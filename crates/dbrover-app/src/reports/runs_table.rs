//! The report's **Runs**: its newest runs side by side, a column each, with the step that
//! dominates each run marked, the step that dominates them all named, the hints, and **Clear
//! history** (`docs/reporting.md`, "The report's tab in RP4").

use std::collections::BTreeSet;

use dbrover_core::{BlobId, ByteCount};
use dbrover_report::{HintId, grouped};
use dbrover_store::{ReportName, ReportRun, RunOrigin, RunOutcome, StepMetrics};
use egui::{Grid, RichText, Ui};
use jiff::tz::TimeZone;

use super::hints;
use super::insights::{self, StepKey, seconds};
use super::{ReportAction, Reports};
use crate::icons;
use crate::theme::Palette;
use crate::widgets;

/// How a run's start heads its column, in the system's time zone.
pub(super) const STARTED_FORMAT: &str = "%Y-%m-%d %H:%M";

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
    let quiet = quiet_hints(reports, name);
    let (silenced, shown): (Vec<_>, Vec<_>) = hints::hints(&runs)
        .into_iter()
        .partition(|hint| quiet.contains(&hint.id));
    for hint in shown {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(widgets::glyph(icons::LIGHTBULB).color(palette.warning));
            ui.label(hint.to_string()).on_hover_text(format!(
                "To stop this hint for this report, add quiet_hints = [\"{}\"] to [report] in its \
                 file.",
                hint.id
            ));
        });
    }
    match silenced.len() {
        0 => {}
        1 => {
            ui.add_space(4.0);
            ui.weak("1 hint is silenced by the file's quiet_hints.");
        }
        count => {
            ui.add_space(4.0);
            ui.weak(format!(
                "{count} hints are silenced by the file's quiet_hints."
            ));
        }
    }
}

/// The hints `name`'s file silences; none while it can't be read.
fn quiet_hints(reports: &Reports, name: &ReportName) -> Vec<HintId> {
    reports
        .find(name)
        .and_then(|listed| listed.read.as_ref().ok())
        .map(|report| report.quiet_hints.clone())
        .unwrap_or_default()
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
                // On one line, with a glyph for a run from elsewhere: two wrapped lines of date
                // and two of "from the command line" made every column four lines tall.
                ui.horizontal(|ui| {
                    let date = RichText::new(started.strftime(STARTED_FORMAT).to_string()).strong();
                    ui.add(egui::Label::new(date).wrap_mode(egui::TextWrapMode::Extend));
                    if let Some((glyph, origin)) = origin(run.origin) {
                        let response = ui
                            .label(widgets::glyph(glyph).color(palette.text_weak))
                            .on_hover_text(origin);
                        widgets::name(response, egui::WidgetType::Label, origin, true);
                    }
                });
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

            if runs.iter().any(|run| run.version.is_some()) {
                ui.weak("Version").on_hover_text(
                    "The report file's Git blob id: what Git calls the version that ran",
                );
                // Newest first, so the run to the right is the one before.
                let older = runs.iter().skip(1).map(Some).chain([None]);
                for (run, older) in runs.iter().zip(older) {
                    version(ui, palette, name, run, older);
                }
                ui.end_row();
            }

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

/// The version of the report's file that `run` ran, shortened as Git shortens it, with
/// **Copy** for the `git log` that finds its commits, and a mark when it differs from `older`'s,
/// the run before.
fn version(
    ui: &mut Ui,
    palette: &Palette,
    name: &ReportName,
    run: &ReportRun,
    older: Option<&ReportRun>,
) {
    let Some(id) = run.version else {
        ui.weak("—");
        return;
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(id.short()).monospace())
            .on_hover_text(id.to_string());
        let changed = older
            .and_then(|older| older.version)
            .is_some_and(|before| before != id);
        if changed {
            let response = ui
                .label(widgets::glyph(icons::GIT_DIFF).color(palette.warning))
                .on_hover_text("The file changed since the run before");
            widgets::name(response, egui::WidgetType::Label, "changed", true);
        }
        let copy = ui
            .small_button(widgets::glyph(icons::COPY))
            .on_hover_text("Copy the git log command that lists the commits with this version");
        let copy = widgets::name(copy, egui::WidgetType::Button, "Copy git log", true);
        if copy.clicked() {
            ui.ctx().copy_text(find_object_command(name, id));
        }
    });
}

/// The command that lists the commits that had exactly this version of the report's file,
/// from the settings folder.
fn find_object_command(name: &ReportName, id: BlobId) -> String {
    format!("git log --find-object={id} -- reports/{name}.toml")
}

/// Where a run came from, beside its start, for the runs the tab didn't start: a glyph, and
/// what it means.
fn origin(origin: RunOrigin) -> Option<(&'static str, &'static str)> {
    match origin {
        RunOrigin::App => None,
        RunOrigin::Command => Some((icons::TERMINAL_WINDOW, "from the command line")),
        RunOrigin::Mcp => Some((icons::PLUGS_CONNECTED, "from MCP")),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_the_git_log_that_finds_the_version() {
        let name = ReportName::new("trial-balance").unwrap();
        let id = BlobId::of(b"hello\n");

        assert_eq!(
            find_object_command(&name, id),
            "git log --find-object=ce013625030ba8dba906f756967f9e9ca394464a \
             -- reports/trial-balance.toml"
        );
    }
}
