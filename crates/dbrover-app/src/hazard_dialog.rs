use dbrover_language::{QueryLanguage, StatementKind};
use dbrover_sql::commits_implicitly;
use dbrover_store::ConsoleKey;
use egui::{Align, Id, Layout, Modal};

use dbrover_core::FullScan;
use dbrover_engine::{RunPurpose, ScanApproval};

use crate::console::{ChangeCount, SubmittedRun, SubmittedStatement};
use crate::form;

pub enum HazardChoice {
    RunAnyway,
    Cancel,
}

/// Shown for a statement that commits on its own (DDL on MySQL) when the user may be counting
/// on Rollback.
const COMMITS_AT_ONCE: &str = "This commits at once, together with anything the open \
     transaction holds, so Rollback can't undo it.";

/// Asks before running commands that may touch everything (an `UPDATE` without `WHERE`, each
/// language lists its own), or that commit by themselves while the user may be counting on
/// Rollback; and, on a careful data source, before any change at all.
pub struct HazardConfirmation {
    pub console: ConsoleKey,
    pub pending: Pending,
    heading: String,
    warnings: Vec<String>,
}

/// What runs once the user confirms.
pub enum Pending {
    Run(SubmittedRun),
    /// The grid's pending edits.
    Edits,
    /// The rest of a run, from a statement its data source's cost guard held back because it
    /// would read a whole table; it runs allowed to.
    Scan(SubmittedRun),
}

/// The outcome of checking statements before running them.
pub enum Review {
    Safe(SubmittedRun),
    NeedsConfirmation(HazardConfirmation),
}

/// What the console the statements run in adds to the review.
#[derive(Debug, Clone, Copy)]
pub struct ReviewContext<'a> {
    pub language: QueryLanguage,
    /// The console commits by hand, or a transaction is open: a statement that commits by
    /// itself would undo what the user expects Rollback to do.
    pub relies_on_rollback: bool,
    /// The data source's name, when it is careful: then every change asks first.
    pub careful: Option<&'a str>,
}

/// Checks each command for one its language says may touch everything, such as an `UPDATE`
/// without `WHERE`, and, when the user relies on Rollback, for one that commits by itself.
///
/// Explain analyse runs its statement, so the statement is reviewed as running it would be,
/// without its `EXPLAIN` prefix, and a careful data source always asks: that it runs is easy to
/// forget, even of a `SELECT`.
pub fn review(console: ConsoleKey, run: SubmittedRun, context: ReviewContext<'_>) -> Review {
    let analysed = run.purpose == RunPurpose::ExplainAnalyse;
    let warnings: Vec<String> = run
        .statements
        .iter()
        .flat_map(|statement| match analysed {
            true => analysed_warnings(statement, context),
            false => statement_warnings(&statement.sql, context),
        })
        .collect();
    if warnings.is_empty() {
        return Review::Safe(run);
    }
    let heading = match (context.careful, analysed) {
        (Some(data_source), true) => {
            format!("Explain analyse on \u{201c}{data_source}\u{201d}?")
        }
        (Some(data_source), false) => format!("Run on \u{201c}{data_source}\u{201d}?"),
        (None, _) => "Run anyway?".to_owned(),
    };
    Review::NeedsConfirmation(HazardConfirmation {
        console,
        pending: Pending::Run(run),
        heading,
        warnings,
    })
}

/// Asks before applying the grid's pending changes to a careful data source.
pub fn confirm_edits(
    console: ConsoleKey,
    data_source: &str,
    changes: ChangeCount,
) -> HazardConfirmation {
    HazardConfirmation {
        console,
        pending: Pending::Edits,
        heading: format!("Apply edits to \u{201c}{data_source}\u{201d}?"),
        warnings: vec![format!(
            "\u{201c}{data_source}\u{201d} is careful: this {}.",
            changes.describe()
        )],
    }
}

/// Asks before running a statement the data source's cost guard held back, and the
/// statements after it, which `rest` holds.
pub fn confirm_scan(
    console: ConsoleKey,
    data_source: &str,
    scan: &FullScan,
    mut rest: SubmittedRun,
) -> HazardConfirmation {
    rest.scans = ScanApproval::AllowFirst;
    HazardConfirmation {
        console,
        pending: Pending::Scan(rest),
        heading: format!("Read all of {}?", scan.table),
        warnings: vec![
            scan.describe(),
            format!("\u{201c}{data_source}\u{201d} asks before reading a whole table."),
        ],
    }
}

impl HazardConfirmation {
    pub fn show(&self, ctx: &egui::Context) -> Option<HazardChoice> {
        let confirm = match self.pending {
            Pending::Run(_) => "Run anyway",
            Pending::Edits => "Apply",
            Pending::Scan(_) => "Scan anyway",
        };
        let modal = Modal::new(Id::new("hazard_confirmation")).show(ctx, |ui| {
            ui.heading(&self.heading);
            for warning in &self.warnings {
                ui.label(warning);
            }
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if form::danger_button(ui, confirm, true).clicked() {
                    Some(HazardChoice::RunAnyway)
                } else if ui.button("Cancel").clicked() {
                    Some(HazardChoice::Cancel)
                } else {
                    None
                }
            })
            .inner
        });
        if modal.should_close() {
            return Some(HazardChoice::Cancel);
        }
        modal.inner
    }
}

/// What running an explained statement would warn of, and on a careful data source that
/// Explain analyse runs it.
fn analysed_warnings(statement: &SubmittedStatement, context: ReviewContext<'_>) -> Vec<String> {
    let typed = statement
        .sql
        .get(statement.prefix_len..)
        .unwrap_or(&statement.sql);
    let runs = context.careful.map(|data_source| {
        warning(
            &format!(
                "\u{201c}{data_source}\u{201d} is careful, and Explain analyse runs this \
                 statement."
            ),
            typed,
        )
    });
    runs.into_iter()
        .chain(statement_warnings(typed, context))
        .collect()
}

fn statement_warnings(sql: &str, context: ReviewContext<'_>) -> Vec<String> {
    let careful = context
        .careful
        .and_then(|data_source| careful_warning(sql, context.language, data_source));
    let everything = context.language.hazard(sql);
    let commits = context
        .language
        .dialect()
        .is_some_and(|dialect| commits_implicitly(sql, dialect));
    let commit = (context.relies_on_rollback && commits).then_some(COMMITS_AT_ONCE);
    careful
        .into_iter()
        .chain(everything.into_iter().chain(commit).map(str::to_owned))
        .map(|description| warning(&description, sql))
        .collect()
}

/// On a careful data source, what a statement that may change something does. Reading and
/// ending a transaction ask nothing: whatever the transaction changed asked already.
fn careful_warning(sql: &str, language: QueryLanguage, data_source: &str) -> Option<String> {
    let what = match language.classify(sql) {
        Ok(StatementKind::Read | StatementKind::Transaction) => return None,
        Ok(kind) => format!("this statement {}", kind.description()),
        Err(_) => "what this statement does can't be told".to_owned(),
    };
    Some(format!(
        "\u{201c}{data_source}\u{201d} is careful, and {what}."
    ))
}

fn warning(description: &str, sql: &str) -> String {
    let first_line = sql.lines().next().unwrap_or_default();
    format!("{description}\n    {first_line}")
}

#[cfg(test)]
mod tests {
    use dbrover_sql::Dialect;

    use super::*;

    #[test]
    fn warns_of_an_implicit_commit_only_where_rollback_was_counted_on() {
        let context = |dialect, relies_on_rollback| ReviewContext {
            language: QueryLanguage::Sql(dialect),
            relies_on_rollback,
            careful: None,
        };
        let cases = [
            ("CREATE TABLE t (n int)", context(Dialect::Mysql, true), 1),
            ("CREATE TABLE t (n int)", context(Dialect::Mysql, false), 0),
            (
                "CREATE TEMPORARY TABLE t (n int)",
                context(Dialect::Mysql, true),
                0,
            ),
            (
                "CREATE TABLE t (n int)",
                context(Dialect::Postgres, true),
                0,
            ),
            ("DELETE FROM t", context(Dialect::Mysql, true), 1),
        ];
        for (sql, context, expected) in cases {
            let warnings = statement_warnings(sql, context);
            assert_eq!(warnings.len(), expected, "{sql} {context:?}: {warnings:?}");
        }
        assert!(
            statement_warnings("DROP TABLE t", context(Dialect::Mysql, true))[0]
                .starts_with(COMMITS_AT_ONCE)
        );
    }

    #[test]
    fn a_careful_data_source_asks_before_every_change_naming_itself() {
        let context = ReviewContext {
            language: QueryLanguage::Sql(Dialect::Postgres),
            relies_on_rollback: false,
            careful: Some("production"),
        };
        let cases = [
            ("SELECT * FROM orders", None),
            ("COMMIT", None),
            (
                "INSERT INTO orders VALUES (1)",
                Some("\u{201c}production\u{201d} is careful, and this statement changes data."),
            ),
            (
                "DROP TABLE orders",
                Some(
                    "\u{201c}production\u{201d} is careful, and this statement changes the schema.",
                ),
            ),
        ];
        for (sql, expected) in cases {
            let warnings = statement_warnings(sql, context);
            assert_eq!(
                warnings
                    .first()
                    .map(|w| w.lines().next().unwrap_or_default()),
                expected,
                "{sql}"
            );
        }
        assert_eq!(statement_warnings("DELETE FROM orders", context).len(), 2);
    }

    #[test]
    fn explain_analyse_asks_on_a_careful_data_source_and_reviews_the_statement_it_runs() {
        use crate::console::RunDestination;
        let run = |sql: &str| {
            let prefix = "EXPLAIN (ANALYZE, FORMAT JSON) ";
            SubmittedRun {
                statements: vec![SubmittedStatement {
                    sql: format!("{prefix}{sql}"),
                    range: 0..sql.len(),
                    prefix_len: prefix.len(),
                    in_database: None,
                    plan: None,
                }],
                purpose: RunPurpose::ExplainAnalyse,
                parameters: dbrover_sql::ParameterValues::default(),
                origin: dbrover_engine::RunOrigin::Console,
                destination: RunDestination::Grid,
                scans: ScanApproval::Ask,
            }
        };
        let context = |careful| ReviewContext {
            language: QueryLanguage::Sql(Dialect::Postgres),
            relies_on_rollback: false,
            careful,
        };
        let key = || ConsoleKey {
            data_source: dbrover_core::DataSourceId::generate(),
            name: dbrover_store::ConsoleName::numbered(1),
        };
        let warnings = |sql: &str, careful| match review(key(), run(sql), context(careful)) {
            Review::Safe(_) => Vec::new(),
            Review::NeedsConfirmation(confirmation) => confirmation
                .warnings
                .iter()
                .map(|warning| warning.lines().next().unwrap_or_default().to_owned())
                .collect(),
        };

        assert!(warnings("SELECT * FROM orders", None).is_empty());
        assert_eq!(
            warnings("SELECT * FROM orders", Some("production")),
            ["\u{201c}production\u{201d} is careful, and Explain analyse runs this statement."]
        );
        assert_eq!(
            warnings("DELETE FROM orders", None).len(),
            1,
            "a DELETE without WHERE asks, as running it would"
        );
        assert_eq!(warnings("DELETE FROM orders", Some("production")).len(), 3);
    }

    #[test]
    fn warns_of_what_each_language_lists_as_touching_everything() {
        let context = ReviewContext {
            language: QueryLanguage::Test,
            relies_on_rollback: true,
            careful: None,
        };
        let warned: Vec<usize> = ["get a", "keys *", "flush"]
            .into_iter()
            .map(|command| statement_warnings(command, context).len())
            .collect();
        assert_eq!(warned, [0, 1, 1]);

        let careful = ReviewContext {
            careful: Some("cache"),
            ..context
        };
        assert!(statement_warnings("get a", careful).is_empty());
        assert_eq!(
            statement_warnings("set a 1", careful),
            ["\u{201c}cache\u{201d} is careful, and this statement changes data.\n    set a 1"]
        );
    }
}
