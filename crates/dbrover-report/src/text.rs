//! A report's text as an editor holds it (`docs/report-editor.md`, RP8c): read and planned in
//! one step, with every problem's place, and the data sources checked even when other
//! problems stop the file from reading.

use dbrover_core::DataSource;
use dbrover_export::CustomFormat;

use crate::plan::find_data_source;
use crate::problem::{Place, ReportProblem, Table};
use crate::read::read_with_places;
use crate::{LimitDefaults, Plan};

/// Reads and plans `text` against the data sources, presets and limits' defaults.
///
/// # Errors
///
/// Every problem found, with its place. When the file doesn't read, the problems also name
/// each source whose data source doesn't exist, which planning would otherwise only say once
/// the rest is fixed.
pub fn plan_text(
    text: &str,
    data_sources: &[DataSource],
    presets: &[CustomFormat],
    defaults: &LimitDefaults,
) -> Result<Plan, Vec<ReportProblem>> {
    match read_with_places(text) {
        Ok(report) => Plan::with_places(report, data_sources, presets, defaults),
        Err(mut problems) => {
            let found = data_source_problems(text, data_sources, &problems);
            problems.extend(found);
            Err(problems)
        }
    }
}

/// A problem for each `[[source]]` naming a data source there isn't, except those whose
/// `data_source` already has one.
fn data_source_problems(
    text: &str,
    data_sources: &[DataSource],
    known: &[ReportProblem],
) -> Vec<ReportProblem> {
    let Ok(file) = toml::from_str::<toml::Table>(text) else {
        return Vec::new();
    };
    let Some(toml::Value::Array(sources)) = file.get("source") else {
        return Vec::new();
    };
    sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| {
            let place = Place::key(Table::Source(index), "data_source");
            if known.iter().any(|problem| problem.place == place) {
                return None;
            }
            let name = source.get("data_source")?.as_str()?;
            if name.trim().is_empty() {
                return None;
            }
            let problem = find_data_source(name, data_sources).err()?;
            let label = match source.get("id").and_then(toml::Value::as_str) {
                Some(id) => format!("source {id:?}"),
                None => "[[source]]".to_owned(),
            };
            Some(ReportProblem::new(format!("{label}: {problem}"), place))
        })
        .collect()
}
