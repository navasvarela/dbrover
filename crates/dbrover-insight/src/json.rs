//! A kept run as JSON, written once for every surface that answers in it
//! (`docs/reports-at-scale.md`, "The profile" among the decisions: the app, the command line
//! and MCP read the same thing).
//!
//! [`profile`] is what `dbrover report run --profile <file>` writes, what
//! `dbrover report runs --json` prints a line of, and what `dbrover mcp`'s `report_profile`
//! answers. [`comparison`] is what `dbrover report compare --json` prints and what
//! `compare_report_runs` answers. They are here, rather than in each caller, so a reader of
//! one has read all three.

use dbrover_report::{Explanation, HintId};
use dbrover_store::{ReportRun, RunOutcome};
use serde_json::{Value, json};

use crate::compare::{Change, Comparison, Differs};
use crate::hints::{Hint, HintAt};
use crate::insights::{millis, origin, outcome};
use crate::{hints, profile as run_profile};

/// The format of the document [`profile`] writes. A reader checks it before trusting the
/// fields: a later DbRover that changes their meaning raises it.
pub const PROFILE_FORMAT: u32 = 1;

/// The table of a report's file a hint is about, as a field of its own beside the sentence.
pub fn hint_at(at: &HintAt) -> String {
    match at {
        HintAt::Report => "report".to_owned(),
        HintAt::Source(id) => format!("source {id}"),
        HintAt::Combine => "combine".to_owned(),
        HintAt::Output(number) => format!("output {number}"),
    }
}

/// One hint, with whether the report's `quiet_hints` silences it. A silenced hint is still
/// answered, marked: the report says not to show it, and a reader asking for the profile is
/// asking what the run measured.
pub fn hint(hint: &Hint, quiet: &[HintId]) -> Value {
    json!({
        "id": hint.id.as_str(),
        "at": hint_at(&hint.at),
        "text": hint.text,
        "quiet": quiet.contains(&hint.id),
    })
}

/// A run's whole profile: how it ended, what its steps measured, the step that held it
/// longest, and the hints its numbers give.
pub fn profile(run: &ReportRun, quiet: &[HintId]) -> Value {
    let hints: Vec<Value> = hints::hints(std::slice::from_ref(run))
        .iter()
        .map(|found| hint(found, quiet))
        .collect();
    let (failed_at, failure) = match &run.outcome {
        RunOutcome::Failed(error) => (Some(error.step.clone()), Some(error.message.clone())),
        RunOutcome::Finished | RunOutcome::Cancelled => (None, None),
    };
    json!({
        "format": PROFILE_FORMAT,
        "id": run.id,
        "report": run.report.as_str(),
        "started_at": run.started_at.to_string(),
        "elapsed_ms": millis(run),
        "outcome": outcome(run),
        "failed_at": failed_at,
        "failure": failure,
        "origin": origin(run),
        "params": run.params,
        "version": run.version.map(|version| version.to_string()),
        // Every report it used as a source, each with the version of its file that ran
        // (RS9d): a run says which version of every report it read, not only its own.
        "reports": run.profile.reports,
        "machine": run.profile,
        "steps": run.steps,
        "bottleneck": run_profile::bottleneck(run).map(|bottleneck| bottleneck.to_string()),
        "hints": hints,
    })
}

fn change(change: &Change) -> Value {
    json!({
        "what": change.what,
        "before": change.before,
        "after": change.after,
        "change": change.ratio(),
        "marked": change.marked(),
    })
}

fn differs(differs: &[Differs]) -> Vec<Value> {
    differs
        .iter()
        .map(|differ| json!({"name": differ.name, "before": differ.before, "after": differ.after}))
        .collect()
}

/// An explanation as `dbrover report explain --json` writes it and `explain_report` answers
/// it: the sources and the combine as `dbrover-report` serialises them, with the hints the
/// plans give beside them.
///
/// `quiet` marks a hint the report silences, as it does in [`profile`]. The command line
/// filters those out before calling and passes none, so its document is unchanged; a tool
/// answers them all, marked, because a client asking what to change is asking what the plans
/// show.
pub fn explanation(explanation: &Explanation, hints: &[Hint], quiet: &[HintId]) -> Value {
    let mut value = serde_json::to_value(explanation).unwrap_or(Value::Null);
    let hints: Vec<Value> = hints.iter().map(|found| hint(found, quiet)).collect();
    if let Value::Object(fields) = &mut value {
        fields.insert("hints".to_owned(), Value::Array(hints));
    }
    value
}

/// Two runs side by side: the run's own changes, each step's, the parameters and limits that
/// differ, and whether the report's file changed between them.
pub fn comparison(before: &ReportRun, after: &ReportRun, comparison: &Comparison) -> Value {
    json!({
        "before": before.id,
        "after": after.id,
        "file_changed": comparison.file_changed,
        "run": comparison.run.iter().map(change).collect::<Vec<_>>(),
        "steps": comparison.steps.iter().map(|step| json!({
            "step": step.step.to_string(),
            "changes": step.changes.iter().map(change).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "params": differs(&comparison.params),
        "limits": differs(&comparison.limits),
    })
}
