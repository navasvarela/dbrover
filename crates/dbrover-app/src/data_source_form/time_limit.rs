//! A time limit typed in minutes, as **Stop statements after** takes it on the engines that
//! bill the time a warehouse runs (Snowflake, C5g; Databricks, C6g).

use dbrover_core::TimeLimit;

/// What a new data source stops statements after: ten minutes, a sixth of an hour of its
/// warehouse, so that a forgotten statement can't run on for the engine's default of two days
/// (`docs/cloud-data.md`, C5, "Warehouse time and the time limit").
pub const NEW_LIMIT_SECONDS: u32 = 600;

const SECONDS_PER_MINUTE: f64 = 60.0;

/// The longest limit an engine takes, in minutes, and as its error says it.
#[derive(Debug, Clone, Copy)]
pub struct Longest {
    pub minutes: f64,
    pub said: &'static str,
}

/// `limit` in minutes, as the field shows it: `10`, or `0.5` for thirty seconds.
pub fn minutes(limit: TimeLimit) -> String {
    let seconds = limit.seconds();
    if seconds.is_multiple_of(60) {
        (seconds / 60).to_string()
    } else {
        format!("{}", f64::from(seconds) / SECONDS_PER_MINUTE)
    }
}

/// Minutes as typed (`10`, `0.5`) as a limit of whole seconds; `None` for text that isn't a
/// positive number of minutes up to `longest`.
pub fn parse_minutes(typed: &str, longest: Longest) -> Option<TimeLimit> {
    let minutes: f64 = typed.trim().parse().ok()?;
    if !(minutes > 0.0 && minutes <= longest.minutes) {
        return None;
    }
    // Within u32 by the bound above (a week of seconds at most); rounded to the second the
    // limit counts in.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let seconds = (minutes * SECONDS_PER_MINUTE).round() as u32;
    TimeLimit::from_seconds(seconds)
}
