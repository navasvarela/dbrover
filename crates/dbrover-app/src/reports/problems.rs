//! A report's text's problems as its editor shows them (`docs/report-editor.md`, RP8f): what
//! reading and planning the report found, the console's checks of each SQL step, and the hints
//! of the newest finished run, each with the bytes it underlines and the line they are on.

use std::ops::Range;

use dbrover_core::DataSource;
use dbrover_report::{
    HintId, Locator, Place, Plan, ReportProblem, StepLanguage, Table, embedded_code,
};
use dbrover_sql::{Dialect, ProblemSource, Severity as SqlSeverity};
use dbrover_store::ReportRun;

use super::highlight::language_of;
use super::hints::{self, HintAt};
use crate::code_editor::{EditorProblem, Severity};
use crate::console::LintOptions;

/// A problem of the text, where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextProblem {
    pub range: Range<usize>,
    /// Counted from 1.
    pub line: usize,
    pub severity: Severity,
    pub message: String,
    /// A quieter line under the message.
    pub note: Option<String>,
    /// The hint this is, which **Quiet this hint** silences.
    pub hint: Option<HintId>,
}

impl EditorProblem for TextProblem {
    fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    fn severity(&self) -> Severity {
        self.severity
    }

    fn message(&self) -> &str {
        &self.message
    }

    fn note(&self) -> Option<String> {
        self.note.clone()
    }
}

/// What the text is checked against besides itself.
pub struct ProblemInputs<'a> {
    pub data_sources: &'a [DataSource],
    pub lint: &'a LintOptions,
    /// The report's newest runs, newest first, which the hints come from.
    pub runs: &'a [ReportRun],
}

/// Every problem of `text`, in the file's order: the report's own, as `planned` found them; each
/// SQL step's; and the newest finished run's hints the file doesn't silence.
pub fn text_problems(
    text: &str,
    planned: &Result<Plan, Vec<ReportProblem>>,
    inputs: &ProblemInputs<'_>,
) -> Vec<TextProblem> {
    let locator = Locator::new(text);
    let mut found: Vec<TextProblem> = match planned {
        Ok(_) => Vec::new(),
        Err(problems) => problems
            .iter()
            .map(|problem| {
                let range = locator.locate(&problem.place);
                at(text, range, Severity::Error, problem.message.clone())
            })
            .collect(),
    };
    found.extend(sql_problems(text, inputs));
    found.extend(hint_problems(text, &locator, inputs.runs));
    found.sort_by_key(|problem| problem.range.start);
    found
}

fn at(text: &str, range: Range<usize>, severity: Severity, message: String) -> TextProblem {
    let line = text[..range.start.min(text.len())].matches('\n').count() + 1;
    TextProblem {
        range,
        line,
        severity,
        message,
        note: None,
        hint: None,
    }
}

/// The console's syntax check and lint rules over each SQL step, in its dialect.
fn sql_problems(text: &str, inputs: &ProblemInputs<'_>) -> Vec<TextProblem> {
    let mut found = Vec::new();
    for code in embedded_code(text) {
        let (dialect, combine) = match &code.language {
            StepLanguage::Source { data_source } => {
                let dialect = language_of(data_source, inputs.data_sources)
                    .and_then(|language| language.dialect());
                (dialect, false)
            }
            StepLanguage::DuckDb => (Some(Dialect::DuckDb), true),
            // prqlc compiles the PRQL when the report is planned, and says what is wrong.
            StepLanguage::Prql => (None, false),
        };
        let Some(dialect) = dialect else {
            continue;
        };
        let sql = &text[code.range.clone()];
        for statement in dbrover_sql::split_statements(sql, dialect) {
            let statement_text = &sql[statement.clone()];
            if !dbrover_sql::is_checkable(statement_text, dialect) {
                continue;
            }
            let start = code.range.start + statement.start;
            for problem in dbrover_sql::check_statement(statement_text, dialect) {
                if !inputs.lint.allows(&problem) {
                    continue;
                }
                let range = start + problem.range.start..start + problem.range.end;
                let severity = sql_severity(problem.severity, &problem.source, combine);
                let mut placed = at(text, range, severity, problem.message);
                if let ProblemSource::Lint(rule) = problem.source {
                    placed.note = Some(format!(
                        "{} · can be switched off in View → Settings",
                        rule.id()
                    ));
                }
                found.push(placed);
            }
        }
    }
    found
}

/// The console's severity, except a syntax error in `[combine]`, which is a warning: the
/// parser doesn't read all of DuckDB's SQL (`ASOF JOIN … ON`, RP2), and the run's DuckDB is
/// what decides.
fn sql_severity(severity: SqlSeverity, source: &ProblemSource, combine: bool) -> Severity {
    match (severity, source) {
        (SqlSeverity::Error, ProblemSource::Syntax) if combine => Severity::Warning,
        (SqlSeverity::Error, _) => Severity::Error,
        (SqlSeverity::Warning, _) => Severity::Warning,
    }
}

/// The newest finished run's hints, under the table each names, except those `[report]
/// quiet_hints` silences.
fn hint_problems(text: &str, locator: &Locator<'_>, runs: &[ReportRun]) -> Vec<TextProblem> {
    let file = toml::from_str::<toml::Table>(text).ok();
    let quiet = file.as_ref().map(quiet_hints).unwrap_or_default();
    hints::hints(runs)
        .into_iter()
        .filter(|hint| !quiet.contains(&hint.id))
        .filter_map(|hint| {
            let table = match &hint.at {
                HintAt::Source(source) => Table::Source(source_position(file.as_ref()?, source)?),
                HintAt::Combine => Table::Combine,
                HintAt::Output(number) => Table::Output(number.checked_sub(1)?),
                HintAt::Report => Table::Report,
            };
            let range = locator.locate(&Place::table(table));
            let mut placed = at(text, range, Severity::Hint, hint.to_string());
            placed.note = Some(format!(
                "From the newest run. Quiet this hint in the side panel, or add \"{}\" to \
                 [report] quiet_hints.",
                hint.id
            ));
            placed.hint = Some(hint.id);
            Some(placed)
        })
        .collect()
}

fn quiet_hints(file: &toml::Table) -> Vec<HintId> {
    file.get("report")
        .and_then(|report| report.get("quiet_hints"))
        .and_then(toml::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(toml::Value::as_str)
                .filter_map(|id| id.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Where the source `id` is among the file's `[[source]]` tables.
fn source_position(file: &toml::Table, id: &str) -> Option<usize> {
    file.get("source")?
        .as_array()?
        .iter()
        .position(|source| source.get("id").and_then(toml::Value::as_str) == Some(id))
}

/// `text` with `hint` added to `[report] quiet_hints`, its comments and order kept; `None` if
/// the file isn't TOML or has no `[report]`.
pub fn quiet_hint(text: &str, hint: HintId) -> Option<String> {
    let mut file: toml_edit::DocumentMut = text.parse().ok()?;
    let report = file.get_mut("report")?.as_table_like_mut()?;
    let id = hint.as_str();
    match report.get_mut("quiet_hints") {
        Some(item) => {
            let ids = item.as_array_mut()?;
            if !ids.iter().any(|value| value.as_str() == Some(id)) {
                ids.push(id);
            }
        }
        None => {
            let mut ids = toml_edit::Array::new();
            ids.push(id);
            report.insert("quiet_hints", toml_edit::value(ids));
        }
    }
    Some(file.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use dbrover_core::{ConnectionConfig, DataSourceId, SafetyLevel, SqliteConfig};
    use dbrover_report::{LimitDefaults, plan_text};
    use dbrover_store::{
        CombineMetrics, OutputMetrics, ReportName, RunOrigin, RunOutcome, SourceMetrics,
        StepMetrics,
    };

    use super::*;

    const REPORT: &str = r#"[report]
name = "R"
# Hints we don't want.
quiet_hints = []

[[source]]
id = "lines"
data_source = "shop"
query = """
SELECT * FROM orders WHERE
"""

[combine]
query = "SELECT * FROM lines ASOF JOIN prices ON lines.t >= prices.t"

[[output]]
format = "csv"
file = "r.csv"
"#;

    fn sqlite(name: &str) -> DataSource {
        DataSource {
            id: DataSourceId::generate(),
            name: name.to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Sqlite(SqliteConfig {
                path: PathBuf::from("/tmp/shop.db"),
                read_only: true,
            }),
        }
    }

    /// A finished run whose source read 100,000 rows for 10 written, and whose combine spilled.
    fn hinted_run() -> ReportRun {
        ReportRun {
            report: ReportName::new("r").unwrap(),
            started_at: jiff::Timestamp::UNIX_EPOCH,
            elapsed: Duration::from_secs(2),
            outcome: RunOutcome::Finished,
            params: std::collections::BTreeMap::new(),
            steps: vec![
                StepMetrics::Source(SourceMetrics {
                    id: "lines".to_owned(),
                    data_source: "shop".to_owned(),
                    rows: 100_000,
                    bytes: 1_000_000,
                    first_row_ms: Some(10),
                    elapsed_ms: 1_000,
                    batches: None,
                    started_ms: None,
                    engine: None,
                    phases: None,
                    limits: None,
                    waits_for: None,
                }),
                StepMetrics::Combine(CombineMetrics {
                    rows: 10,
                    bytes: 100,
                    first_row_ms: Some(10),
                    elapsed_ms: 500,
                    peak_memory: Some(1_000_000),
                    peak_temp_size: Some(3_000_000),
                    started_ms: None,
                    query_ms: None,
                    limits: None,
                }),
                StepMetrics::Output(OutputMetrics {
                    number: 1,
                    file: "r.csv".to_owned(),
                    rows: 10,
                    size: 100,
                    elapsed_ms: 1,
                    started_ms: None,
                    ended_ms: None,
                }),
            ],
            origin: RunOrigin::App,
            version: None,
            profile: Default::default(),
        }
    }

    fn problems(text: &str, runs: &[ReportRun]) -> Vec<(String, Severity, String)> {
        let shop = [sqlite("shop")];
        let planned = plan_text(text, &shop, &[], &LimitDefaults::default());
        let inputs = ProblemInputs {
            data_sources: &shop,
            lint: &LintOptions::default(),
            runs,
        };
        text_problems(text, &planned, &inputs)
            .into_iter()
            .map(|problem| {
                (
                    text[problem.range].to_owned(),
                    problem.severity,
                    problem.message,
                )
            })
            .collect()
    }

    #[test]
    fn checks_each_sql_step_and_warns_rather_than_errs_on_duckdbs_own_syntax() {
        let found = problems(REPORT, &[]);
        let severities: Vec<Severity> = found.iter().map(|(_, severity, _)| *severity).collect();
        assert!(
            severities.contains(&Severity::Error),
            "the source's unfinished WHERE: {found:#?}"
        );
        let combine = REPORT.find("ASOF").unwrap();
        let in_combine = found
            .iter()
            .filter(|(fragment, ..)| REPORT[combine - 40..].contains(fragment.as_str()));
        assert!(
            in_combine
                .into_iter()
                .all(|(_, severity, _)| *severity != Severity::Error),
            "{found:#?}"
        );
    }

    #[test]
    fn places_the_runs_hints_under_their_tables_unless_quieted() {
        let text = REPORT.replace("WHERE\n", "\n");
        let runs = [hinted_run()];
        let hints: Vec<(String, String)> = problems(&text, &runs)
            .into_iter()
            .filter(|(_, severity, _)| *severity == Severity::Hint)
            .map(|(fragment, _, message)| (fragment, message))
            .collect();
        assert_eq!(hints.len(), 2, "{hints:#?}");
        assert_eq!(hints[0].0, "[[source]]");
        assert_eq!(hints[1].0, "[combine]");

        let quiet = quiet_hint(&text, HintId::CombineSpilled).unwrap();
        assert!(
            quiet.contains("# Hints we don't want."),
            "comments kept:\n{quiet}"
        );
        let left: Vec<String> = problems(&quiet, &runs)
            .into_iter()
            .filter(|(_, severity, _)| *severity == Severity::Hint)
            .map(|(fragment, ..)| fragment)
            .collect();
        assert_eq!(left, ["[[source]]"]);
    }

    #[test]
    fn adds_quiet_hints_to_a_report_without_them_once() {
        let text = "[report]\nname = \"R\"\n\n[[source]]\nid = \"a\"\n";
        let quiet = quiet_hint(text, HintId::SourceReadsMore).unwrap();
        let again = quiet_hint(&quiet, HintId::SourceReadsMore).unwrap();
        assert_eq!(quiet, again);
        let file: toml::Table = toml::from_str(&quiet).unwrap();
        assert_eq!(
            file["report"]["quiet_hints"],
            toml::Value::Array(vec![toml::Value::String("source-reads-more".to_owned())])
        );
        assert!(quiet_hint("name = 1", HintId::SourceReadsMore).is_none());
    }
}
