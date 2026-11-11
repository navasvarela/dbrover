//! The report's **Profile** (`docs/reports-at-scale.md`, "A timeline" and RS1c): one of the
//! runs the history keeps, as a timeline of its steps, each bar split into its phases, with a
//! wait for the round before hatched, and the step and phase that held the run named under it;
//! then the data funnel and the combine step's plan (RS2c).

use dbrover_plan::PlanFormat;
use dbrover_report::grouped;
use dbrover_store::{ReportName, ReportRun, RunOutcome, StepMetrics};
use egui::{Color32, Grid, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use jiff::tz::TimeZone;

use super::funnel::{self, Funnel};
use super::insights::seconds;
use super::profile::{self, Lane, Phase, Timeline};
use super::runs_table::STARTED_FORMAT;
use super::{ProfilePlan, Reports};
use crate::console::plan::{self, AnalyseButton, PlanView};
use crate::icons;
use crate::theme::Palette;
use crate::widgets;

/// A lane's height, and its bar's within it.
const LANE_HEIGHT: f32 = 20.0;
const BAR_HEIGHT: f32 = 12.0;
/// The least a bar is drawn, so a step of a millisecond still shows.
const MIN_BAR_WIDTH: f32 = 2.0;
/// The space between the hatching's lines.
const HATCH_STEP: f32 = 5.0;
/// The room left beside the timeline for the lanes' labels: a step's name and its engine.
const LABELS_WIDTH: f32 = 280.0;
/// The narrowest the timeline is drawn, however narrow the panel.
const MIN_TIMELINE_WIDTH: f32 = 240.0;
/// The height given to the combine step's plan: room for a dozen operators and the selected
/// one's facts under them, as a console's plan has.
const PLAN_HEIGHT: f32 = 360.0;

pub fn show(ui: &mut Ui, reports: &mut Reports, name: &ReportName) {
    let palette = Palette::for_visuals(ui.visuals());
    ui.add_space(12.0);
    let runs = match reports.runs(name) {
        Some(Ok(runs)) if !runs.is_empty() => runs.clone(),
        Some(Err(error)) => {
            ui.colored_label(palette.error, format!("The runs can't be read: {error}"));
            return;
        }
        None => {
            ui.weak("Loading…");
            return;
        }
        Some(Ok(_)) => {
            ui.weak("No run yet. Each run's profile shows here: when each step ran, and why.");
            return;
        }
    };
    let chosen = reports
        .state(name)
        .map_or(0, |state| state.profile_run)
        .min(runs.len() - 1);
    let picked = pick_run(ui, &runs, chosen);
    if picked != chosen {
        reports.state_mut(name).profile_run = picked;
    }
    let run = &runs[picked];
    ui.add_space(8.0);
    let Some(timeline) = profile::timeline(run) else {
        ui.weak(
            "This run was kept before DbRover measured when each step ran. Run the report again \
             to see its profile.",
        );
        return;
    };
    if let Some(bottleneck) = profile::bottleneck(run) {
        widgets::status(
            ui,
            icons::LIGHTNING,
            palette.warning,
            &bottleneck.to_string(),
        );
        ui.add_space(6.0);
    }
    draw(ui, &palette, &timeline);
    ui.add_space(6.0);
    legend(ui, &palette, &timeline);
    if let Some(peak) = run.profile.peak_memory {
        ui.add_space(4.0);
        ui.weak(format!(
            "The worker held at most {} in memory.",
            dbrover_core::ByteCount(peak)
        ));
    }
    if let Some(funnel) = funnel::funnel(run) {
        ui.add_space(12.0);
        draw_funnel(ui, &funnel);
    }
    combine_plan(ui, reports, name, run);
}

/// Each source's rows from what it returned to what the combine used, and what was written.
fn draw_funnel(ui: &mut Ui, funnel: &Funnel) {
    ui.strong("Data funnel");
    ui.weak(
        "What each source's engine scanned, where it says, what the source returned, and the \
         rows of it the combine step's scans passed on. A wide drop says the source's query \
         could filter more.",
    );
    ui.add_space(4.0);
    Grid::new("report_profile_funnel")
        .num_columns(4)
        .spacing(vec2(24.0, 4.0))
        .show(ui, |ui| {
            ui.strong("Source");
            ui.strong("Scanned");
            ui.strong("Returned");
            ui.strong("Used by the combine");
            ui.end_row();
            for line in &funnel.lines {
                ui.label(&line.source);
                match line.scanned_bytes {
                    Some(scanned) => ui.label(dbrover_core::ByteCount(scanned).to_string()),
                    None => ui.weak("not said"),
                };
                ui.label(format!(
                    "{} rows · {}",
                    grouped(line.returned_rows),
                    dbrover_core::ByteCount(line.returned_bytes)
                ));
                match line.used_rows {
                    Some(used) => ui.label(format!(
                        "{} rows ({})",
                        grouped(used),
                        share(used, line.returned_rows)
                    )),
                    None => ui.weak("not known"),
                };
                ui.end_row();
            }
        });
    if let Some(written) = funnel.written_rows {
        ui.weak(format!("The outputs wrote {} rows.", grouped(written)));
    }
}

/// `used` as a share of `returned`: `12%`, `under 1%`.
fn share(used: u64, returned: u64) -> String {
    if returned == 0 {
        return "of none".to_owned();
    }
    let percent = u128::from(used) * 100 / u128::from(returned);
    if percent == 0 && used < returned {
        "under 1%".to_owned()
    } else {
        format!("{percent}%")
    }
}

/// The combine step's plan, drawn as W10 draws a plan, when the run kept one.
fn combine_plan(ui: &mut Ui, reports: &mut Reports, name: &ReportName, run: &ReportRun) {
    let Some(text) = run.steps.iter().find_map(|step| match step {
        StepMetrics::Combine(combine) => combine.plan.clone(),
        _ => None,
    }) else {
        return;
    };
    ui.add_space(12.0);
    ui.strong("The combine step's plan");
    ui.weak(
        "DuckDB's operators as they ran the combine query, the costliest tinted. A scan says \
         the columns it read and the filters it applied as it read.",
    );
    ui.add_space(4.0);
    let state = reports.state_mut(name);
    let kept = state
        .profile_plan
        .take()
        .filter(|kept| kept.started_at == run.started_at);
    let kept = kept.unwrap_or_else(|| {
        let mut view = PlanView::new(PlanFormat::DuckDbJson);
        view.read_text(Some(text));
        ProfilePlan {
            started_at: run.started_at,
            view,
        }
    });
    let kept = state.profile_plan.insert(kept);
    if let Some(reason) = kept.view.failure() {
        ui.weak(format!("The plan can't be read: {reason}"));
        return;
    }
    ui.allocate_ui(vec2(ui.available_width(), PLAN_HEIGHT), |ui| {
        plan::show(ui, &mut kept.view, AnalyseButton::Hidden);
    });
}

/// A menu of the runs kept, newest first; returns the one picked.
fn pick_run(ui: &mut Ui, runs: &[ReportRun], chosen: usize) -> usize {
    let time_zone = TimeZone::system();
    let label = |run: &ReportRun| {
        let started = run.started_at.to_zoned(time_zone.clone());
        let outcome = match run.outcome {
            RunOutcome::Finished => "finished",
            RunOutcome::Failed(_) => "failed",
            RunOutcome::Cancelled => "cancelled",
        };
        format!(
            "{}, {outcome} in {}",
            started.strftime(STARTED_FORMAT),
            seconds(u64::try_from(run.elapsed.as_millis()).unwrap_or(u64::MAX))
        )
    };
    let mut picked = chosen;
    ui.horizontal(|ui| {
        let caption = ui.label("Run");
        egui::ComboBox::from_id_salt("report_profile_run")
            .selected_text(label(&runs[chosen]))
            .show_ui(ui, |ui| {
                for (index, run) in runs.iter().enumerate() {
                    ui.selectable_value(&mut picked, index, label(run));
                }
            })
            .response
            .labelled_by(caption.id);
    });
    picked
}

/// The lanes, a label each beside its bar, and the time axis under them.
fn draw(ui: &mut Ui, palette: &Palette, timeline: &Timeline) {
    let width = (ui.available_width() - LABELS_WIDTH).max(MIN_TIMELINE_WIDTH);
    Grid::new("report_profile_lanes")
        .num_columns(2)
        .spacing([12.0, 0.0])
        .show(ui, |ui| {
            for (index, lane) in timeline.lanes.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.set_min_height(LANE_HEIGHT);
                    ui.horizontal(|ui| {
                        ui.label(lane.step.to_string());
                        if let Some(engine) = &lane.engine {
                            ui.weak(engine);
                        }
                    });
                });
                lane_bar(ui, palette, timeline.total_ms, lane, width, index);
                ui.end_row();
            }
            ui.label("");
            ui.horizontal(|ui| {
                ui.set_width(width);
                ui.weak("0 s");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(seconds(timeline.total_ms));
                });
            });
            ui.end_row();
        });
}

fn lane_bar(ui: &mut Ui, palette: &Palette, total_ms: u64, lane: &Lane, width: f32, index: usize) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, LANE_HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(1.0, palette.border),
    );
    let x = |ms: u64| rect.left() + width * fraction(ms, total_ms);
    let bar = |start: u64, end: u64| {
        let (left, right) = (x(start), x(end).max(x(start) + MIN_BAR_WIDTH));
        Rect::from_min_max(
            pos2(left, rect.center().y - BAR_HEIGHT / 2.0),
            pos2(right, rect.center().y + BAR_HEIGHT / 2.0),
        )
    };
    if let Some((from, to)) = lane.waited {
        let waited = bar(from, to);
        hatch(&painter, waited, palette.text_weak);
        ui.interact(
            waited,
            ui.id().with(("report_profile_wait", index)),
            Sense::hover(),
        )
        .on_hover_text(format!(
            "Waited {} for the sources of the round before",
            seconds(to - from)
        ));
    }
    for (segment_index, segment) in lane.segments.iter().enumerate() {
        let segment_rect = bar(segment.start_ms, segment.end_ms);
        painter.rect_filled(segment_rect, 2.0, color(palette, segment.phase));
        let id = ui
            .id()
            .with(("report_profile_segment", index, segment_index));
        ui.interact(segment_rect, id, Sense::hover())
            .on_hover_text(format!(
                "{}: {}, from {} to {}",
                segment.phase.name(),
                seconds(segment.end_ms - segment.start_ms),
                seconds(segment.start_ms),
                seconds(segment.end_ms)
            ));
    }
    // A partitioned step's ranges as a strip under its bar, a mark a range, so the long one
    // stands out and hovering it names it (RS4f).
    for (range_index, range) in lane.ranges.iter().enumerate() {
        let below = bar(range.start_ms, range.end_ms);
        let strip = Rect::from_min_max(
            pos2(below.left(), below.bottom() + RANGE_GAP),
            pos2(
                (below.right() - RANGE_GAP).max(below.left() + MIN_BAR_WIDTH),
                below.bottom() + RANGE_GAP + RANGE_HEIGHT,
            ),
        );
        painter.rect_filled(strip, 1.0, palette.text_weak);
        let id = ui.id().with(("report_profile_range", index, range_index));
        ui.interact(strip, id, Sense::hover())
            .on_hover_text(range.describe());
    }
}

/// The strip of a partitioned step's ranges under its bar: its height, and the gap above it
/// and between ranges, in points; small enough to fit within the lane's height.
const RANGE_HEIGHT: f32 = 3.0;
const RANGE_GAP: f32 = 1.0;

/// Where `ms` falls in the run, from 0 to 1.
fn fraction(ms: u64, total_ms: u64) -> f32 {
    if total_ms == 0 {
        return 0.0;
    }
    (ms as f32 / total_ms as f32).clamp(0.0, 1.0)
}

/// Diagonal lines across `rect`, for time spent waiting.
fn hatch(painter: &egui::Painter, rect: Rect, color: Color32) {
    let painter = painter.with_clip_rect(rect);
    let stroke = Stroke::new(1.0, color);
    let mut x = rect.left() - rect.height();
    while x < rect.right() {
        painter.line_segment(
            [
                Pos2::new(x, rect.bottom()),
                Pos2::new(x + rect.height(), rect.top()),
            ],
            stroke,
        );
        x += HATCH_STEP;
    }
    painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
}

/// Each phase's colour, from the palette's roles so both themes read it.
fn color(palette: &Palette, phase: Phase) -> Color32 {
    match phase {
        Phase::Connect | Phase::Unsplit => palette.text_weak,
        Phase::Server | Phase::Query => palette.warning,
        Phase::Transfer | Phase::Rows => palette.secondary,
        Phase::Load => palette.primary,
        Phase::Type => palette.primary.gamma_multiply(0.55),
        Phase::Write => palette.success,
        Phase::Idle => palette.border,
    }
}

/// The phases the timeline shows, each with its colour.
fn legend(ui: &mut Ui, palette: &Palette, timeline: &Timeline) {
    let mut shown: Vec<Phase> = Vec::new();
    for segment in timeline.lanes.iter().flat_map(|lane| &lane.segments) {
        if !shown.contains(&segment.phase) {
            shown.push(segment.phase);
        }
    }
    let waited = timeline.lanes.iter().any(|lane| lane.waited.is_some());
    ui.horizontal_wrapped(|ui| {
        // Each swatch beside its name, so a wrapped line never parts them.
        for phase in shown {
            ui.horizontal(|ui| {
                swatch(ui, color(palette, phase), false);
                ui.weak(phase.name());
            });
            ui.add_space(8.0);
        }
        if waited {
            ui.horizontal(|ui| {
                swatch(ui, palette.text_weak, true);
                ui.weak("Waiting for the round before");
            });
        }
    });
}

fn swatch(ui: &mut Ui, color: Color32, hatched: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 10.0), Sense::hover());
    if hatched {
        hatch(ui.painter(), rect, color);
    } else {
        ui.painter().rect_filled(rect, 2.0, color);
    }
}
