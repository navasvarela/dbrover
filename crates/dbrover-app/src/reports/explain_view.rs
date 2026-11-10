//! The report's **Explain** panel (`docs/reports-at-scale.md`, RS2d): each source's statement
//! with its engine's plan and its cost guard's estimate, then the combine step's plan over
//! empty tables. Plans are shown as their text plan, as W10's **Text** view writes them.

use dbrover_report::{CombineExplanation, ExplainedPlan, SourceExplanation};
use dbrover_store::ReportName;
use egui::{RichText, Ui};

use super::explanation;
use super::hints::Hint;
use super::{ExplainState, Reports};
use crate::icons;
use crate::theme::Palette;
use crate::widgets;

pub fn show(ui: &mut Ui, reports: &Reports, name: &ReportName) {
    let palette = Palette::for_visuals(ui.visuals());
    ui.add_space(12.0);
    let Some(state) = reports
        .state(name)
        .and_then(|state| state.explanation.as_ref())
    else {
        ui.weak(
            "Explain plans the report without running it: each source's plan from its \
             database, what its cost guard estimates, and the combine step's plan.",
        );
        return;
    };
    let explanation = match state {
        ExplainState::Explaining => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Explaining…");
            });
            return;
        }
        ExplainState::Failed(message) => {
            ui.colored_label(palette.error, message);
            return;
        }
        ExplainState::Explained(explanation) => explanation,
    };
    for source in &explanation.sources {
        source_section(ui, &palette, source);
        ui.add_space(10.0);
    }
    match &explanation.combine {
        None => {}
        Some(CombineExplanation::Planned { .. }) => {
            ui.strong("The combine step");
            ui.weak(
                "Planned by DuckDB over empty tables of each source's columns: what it reads \
                 of each, not what it costs.",
            );
            match explanation::combine_plan(explanation) {
                Some(Ok(plan)) => plan_text(ui, &plan.text()),
                Some(Err(reason)) => {
                    ui.colored_label(palette.error, format!("The plan can't be read: {reason}"));
                }
                None => {}
            }
        }
        Some(CombineExplanation::Unknown { reason }) => {
            ui.strong("The combine step");
            ui.weak(reason);
        }
        Some(CombineExplanation::Failed { message }) => {
            ui.strong("The combine step");
            ui.colored_label(palette.error, format!("DuckDB couldn't plan it: {message}"));
        }
    }
    let runs = match reports.runs(name) {
        Some(Ok(runs)) => runs.as_slice(),
        _ => &[],
    };
    let quiet = reports
        .find(name)
        .and_then(|listed| listed.read.as_ref().ok())
        .map(|report| report.quiet_hints.clone())
        .unwrap_or_default();
    let hints: Vec<Hint> = explanation::hints(explanation, runs)
        .into_iter()
        .filter(|hint| !quiet.contains(&hint.id))
        .collect();
    if !hints.is_empty() {
        ui.add_space(10.0);
        ui.strong("Hints");
        for hint in &hints {
            widgets::status(ui, icons::LIGHTBULB, palette.primary, &hint.text);
        }
    }
}

fn source_section(ui: &mut Ui, palette: &Palette, source: &SourceExplanation) {
    ui.strong(format!("Source {} on {}", source.id, source.data_source));
    egui::CollapsingHeader::new("Statement")
        .id_salt(("report_explain_statement", source.id.as_str()))
        .show(ui, |ui| plan_text(ui, &source.explained.statement));
    for note in &source.notes {
        ui.weak(note);
    }
    if let Some(estimate) = &source.explained.estimate {
        let (glyph, color) = if estimate.refused {
            (icons::WARNING, palette.warning)
        } else {
            (icons::LIGHTBULB, palette.primary)
        };
        let mut text = estimate.says.clone();
        if let Some(limit) = estimate.limit_bytes {
            text.push_str(&format!(
                " Its data source asks above {}.",
                dbrover_core::ByteCount(limit)
            ));
        }
        if estimate.refused {
            text.push_str(" A run would refuse this source: nobody can be asked.");
        }
        widgets::status(ui, glyph, color, &text);
    }
    match &source.explained.plan {
        plan @ ExplainedPlan::Drawn { .. } => match explanation::drawn(plan) {
            Some(Ok(plan)) => plan_text(ui, &plan.text()),
            Some(Err(reason)) => {
                ui.colored_label(palette.error, format!("The plan can't be read: {reason}"));
            }
            None => {}
        },
        ExplainedPlan::Text { text } => plan_text(ui, text),
        ExplainedPlan::Unexplained { reason } => {
            ui.weak(reason);
        }
        ExplainedPlan::Failed { message } => {
            ui.colored_label(palette.error, format!("Couldn't be explained: {message}"));
        }
    }
}

fn plan_text(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).monospace());
}
