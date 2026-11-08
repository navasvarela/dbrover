//! A console's problems: syntax errors and lint warnings found while typing, and what the
//! database says when asked to prepare its statements.
//!
//! Checking stays cheap on the UI thread: it waits until typing pauses ([`DEBOUNCE`]), results
//! are cached per statement text so an edit re-parses only the statement it touched, and a
//! frame parses statements for at most [`FRAME_BUDGET`], carrying on in the next frame if a
//! large console needs longer.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::{Duration, Instant};

use dbrover_core::{CostEstimate, RejectionKind};
use dbrover_engine::StatementCheck;
use dbrover_language::QueryLanguage;
use dbrover_sql::{
    Dialect, LintRule, Problem, ProblemSource, Severity, TokenKind, check_statement, is_checkable,
    split_statements, tokenize,
};

use super::catalog_lint;
use super::chart::block::ChartProblem;
use super::completion::CatalogSource;
use crate::code_editor::highlight::error_marker;
use crate::code_editor::{self, EditorProblem};

/// How long typing must pause before the text is checked: long enough not to flag every
/// half-typed word, short enough to feel live.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// The most a frame spends parsing. Measured on an M-series Mac: checking a statement takes
/// about 13 µs in a release build (84 µs in debug), so a typical edit costs one statement's
/// check, and a console of 1,000 statements (78 KB) checks from cold in about 13 ms, spread
/// over four frames.
const FRAME_BUDGET: Duration = Duration::from_millis(4);

impl EditorProblem for Problem {
    fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    fn severity(&self) -> code_editor::Severity {
        match self.severity {
            Severity::Error => code_editor::Severity::Error,
            Severity::Warning => code_editor::Severity::Warning,
        }
    }

    fn message(&self) -> &str {
        &self.message
    }

    fn note(&self) -> Option<String> {
        match self.source {
            ProblemSource::Lint(rule) => Some(format!(
                "{} · can be switched off in View → Settings",
                rule.id()
            )),
            ProblemSource::Syntax | ProblemSource::Database => None,
        }
    }
}

/// Which checks are on, from `settings.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintOptions {
    pub disabled: Vec<LintRule>,
    pub ask_database: bool,
}

impl Default for LintOptions {
    fn default() -> Self {
        Self {
            disabled: Vec::new(),
            ask_database: true,
        }
    }
}

impl LintOptions {
    /// Whether the settings show `problem`: not a lint rule switched off.
    pub fn allows(&self, problem: &Problem) -> bool {
        match problem.source {
            ProblemSource::Lint(rule) => !self.disabled.contains(&rule),
            ProblemSource::Syntax | ProblemSource::Database => true,
        }
    }
}

/// What a check needs besides the problem state.
pub struct CheckInput<'a> {
    pub text: &'a str,
    pub dialect: Dialect,
    pub catalog: &'a dyn CatalogSource,
    pub options: &'a LintOptions,
    /// The console's connection is open and free: no run, unread rows or open transaction.
    pub can_ask_database: bool,
}

/// What the view should do after a check.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CheckOutcome {
    /// Draw again after this long, to check once typing has paused or to carry on checking.
    pub repaint_after: Option<Duration>,
    /// Statements to ask the database about.
    pub ask_database: Vec<String>,
}

#[derive(Debug)]
pub struct Problems {
    /// What the parser and lint rules found, per statement text, relative to the statement.
    local: HashMap<String, Vec<Problem>>,
    /// What the database said, per statement text.
    database: HashMap<String, StatementCheck>,
    /// Statements were sent to the database and haven't been answered.
    asking: bool,
    /// When to check next; `None` while the shown problems are up to date.
    due: Option<Instant>,
    /// The check a repaint has been asked for, so it is asked once rather than every frame.
    repaint_asked_for: Option<Instant>,
    /// The options the shown problems were found with.
    options: Option<LintOptions>,
    /// Problems in the whole text, in text order.
    shown: Vec<Problem>,
}

impl Problems {
    /// What the database estimated `sql` would scan when it last accepted it, on an engine that
    /// estimates before running (BigQuery's dry run, C4).
    pub fn estimate_of(&self, sql: &str) -> Option<&CostEstimate> {
        match self.database.get(sql)? {
            StatementCheck::Accepted { estimate } => estimate.as_ref(),
            StatementCheck::Rejected(_) | StatementCheck::Skipped => None,
        }
    }

    /// A console's problems, to be checked as soon as it is drawn.
    pub fn new(now: Instant) -> Self {
        Self {
            local: HashMap::new(),
            database: HashMap::new(),
            asking: false,
            due: Some(now),
            repaint_asked_for: None,
            options: None,
            shown: Vec::new(),
        }
    }

    pub fn shown(&self) -> &[Problem] {
        &self.shown
    }

    pub fn count(&self, severity: Severity) -> usize {
        self.shown
            .iter()
            .filter(|problem| problem.severity == severity)
            .count()
    }

    /// The text changed: check it once typing pauses.
    pub fn edited(&mut self, now: Instant) {
        self.due = Some(now + DEBOUNCE);
    }

    /// Something the checks depend on besides the text changed (the catalog, the database's
    /// answers): check at once.
    pub fn recheck(&mut self, now: Instant) {
        self.due = Some(self.due.map_or(now, |due| due.min(now)));
    }

    /// The database's answers may be out of date (a run may have created or dropped tables,
    /// or ended a transaction): ask again.
    pub fn forget_database(&mut self, now: Instant) {
        self.database.clear();
        self.asking = false;
        self.recheck(now);
    }

    pub fn database_answered(&mut self, results: &[(String, StatementCheck)], now: Instant) {
        self.database.extend(results.iter().cloned());
        self.asking = false;
        self.recheck(now);
    }

    /// Checks the text if it is due, within this frame's budget.
    pub fn check(&mut self, input: &CheckInput<'_>, now: Instant) -> CheckOutcome {
        if self
            .options
            .as_ref()
            .is_some_and(|options| options != input.options)
        {
            self.recheck(now);
        }
        let Some(due) = self.due else {
            return CheckOutcome::default();
        };
        if now < due {
            let first_ask = self.repaint_asked_for.replace(due) != Some(due);
            return CheckOutcome {
                repaint_after: first_ask.then(|| due - now),
                ..CheckOutcome::default()
            };
        }
        let statements = split_statements(input.text, input.dialect);
        let started = Instant::now();
        for range in &statements {
            let sql = &input.text[range.clone()];
            if self.local.contains_key(sql) {
                continue;
            }
            if started.elapsed() > FRAME_BUDGET {
                return CheckOutcome {
                    repaint_after: Some(Duration::ZERO),
                    ..CheckOutcome::default()
                };
            }
            self.local
                .insert(sql.to_owned(), check_statement(sql, input.dialect));
        }
        let current: HashSet<&str> = statements
            .iter()
            .map(|range| &input.text[range.clone()])
            .collect();
        self.local.retain(|sql, _| current.contains(sql.as_str()));
        if !self.asking {
            self.database
                .retain(|sql, _| current.contains(sql.as_str()));
        }
        self.shown = self.collect(input, &statements);
        self.options = Some(input.options.clone());
        self.due = None;
        CheckOutcome {
            repaint_after: None,
            ask_database: self.questions_for_database(input, &statements),
        }
    }

    fn collect(&self, input: &CheckInput<'_>, statements: &[Range<usize>]) -> Vec<Problem> {
        let created = catalog_lint::created_names(input.text, input.dialect);
        let mut shown = Vec::new();
        for range in statements {
            let sql = &input.text[range.clone()];
            let answer = self.database.get(sql);
            // The database has the last word: a parser error it disagrees with is a gap in
            // the parser, and it knows the names better than the cached catalog.
            let answered = matches!(
                answer,
                Some(StatementCheck::Accepted { .. } | StatementCheck::Rejected(_))
            );
            let mut found: Vec<Problem> = self
                .local
                .get(sql)
                .into_iter()
                .flatten()
                .filter(|problem| !(answered && problem.source == ProblemSource::Syntax))
                .cloned()
                .collect();
            if let Some(StatementCheck::Rejected(rejection)) = answer {
                let position = rejection
                    .position
                    .or_else(|| named_in(&rejection.message, sql, input.dialect))
                    .unwrap_or(0);
                let range = error_marker(sql, QueryLanguage::Sql(input.dialect), position);
                found.push(Problem {
                    range,
                    severity: match rejection.kind {
                        RejectionKind::UnknownName => Severity::Warning,
                        RejectionKind::Syntax | RejectionKind::Other => Severity::Error,
                    },
                    message: format!("The database says: {}", rejection.message),
                    source: ProblemSource::Database,
                });
            }
            if !answered {
                found.extend(catalog_lint::problems(
                    sql,
                    input.dialect,
                    input.catalog,
                    &created,
                ));
            }
            shown.extend(
                found
                    .into_iter()
                    .filter(|problem| input.options.allows(problem))
                    .map(|problem| problem.shifted(range.start)),
            );
        }
        shown.sort_by_key(|problem| (problem.range.start, problem.severity));
        shown
    }

    /// The statements the database hasn't been asked about, if it may be asked now.
    fn questions_for_database(
        &mut self,
        input: &CheckInput<'_>,
        statements: &[Range<usize>],
    ) -> Vec<String> {
        if self.asking || !input.can_ask_database || !input.options.ask_database {
            return Vec::new();
        }
        let mut ask: Vec<String> = Vec::new();
        for range in statements {
            let sql = &input.text[range.clone()];
            if is_checkable(sql, input.dialect)
                && !self.database.contains_key(sql)
                && !ask.iter().any(|asked| asked == sql)
            {
                ask.push(sql.to_owned());
            }
        }
        self.asking = !ask.is_empty();
        ask
    }
}

/// Where to jump from byte `cursor` among problems starting at `starts`, sorted: the first one
/// starting after it, or before it going backwards, wrapping around.
pub fn next_problem(starts: &[usize], cursor: usize, forward: bool) -> Option<usize> {
    if forward {
        starts
            .iter()
            .find(|&&start| start > cursor)
            .or(starts.first())
            .copied()
    } else {
        starts
            .iter()
            .rev()
            .find(|&&start| start < cursor)
            .or(starts.last())
            .copied()
    }
}

/// A problem the editor underlines: a statement's, or a chart block's.
pub enum ShownProblem<'a> {
    Statement(&'a Problem),
    Chart(&'a ChartProblem),
}

impl ShownProblem<'_> {
    /// `statements`' and `charts`' problems together, in the text's order.
    pub fn all<'a>(statements: &'a [Problem], charts: &'a [ChartProblem]) -> Vec<ShownProblem<'a>> {
        let mut all: Vec<ShownProblem<'a>> = statements
            .iter()
            .map(ShownProblem::Statement)
            .chain(charts.iter().map(ShownProblem::Chart))
            .collect();
        all.sort_by_key(|problem| problem.range().start);
        all
    }

    fn problem(&self) -> &dyn EditorProblem {
        match self {
            Self::Statement(problem) => *problem,
            Self::Chart(problem) => *problem,
        }
    }
}

impl EditorProblem for ShownProblem<'_> {
    fn range(&self) -> Range<usize> {
        self.problem().range()
    }

    fn severity(&self) -> code_editor::Severity {
        self.problem().severity()
    }

    fn message(&self) -> &str {
        self.problem().message()
    }

    fn note(&self) -> Option<String> {
        self.problem().note()
    }
}

/// Where the name an error message ends with appears in the statement, for databases that
/// say what is unknown but not where: SQLite's `no such table: shipments`, `no such column:
/// o.nope`.
fn named_in(message: &str, sql: &str, dialect: Dialect) -> Option<usize> {
    let (_, name) = message.rsplit_once(": ")?;
    let name = name.rsplit('.').next()?.trim();
    tokenize(sql, dialect)
        .into_iter()
        .find(|token| {
            token.kind == TokenKind::Word && sql[token.range.clone()].eq_ignore_ascii_case(name)
        })
        .map(|token| token.range.start)
}

#[cfg(test)]
mod tests {
    use dbrover_core::Rejection;

    use super::super::completion::NoCatalog;
    use super::*;
    use dbrover_core::{ByteCount, ScanEstimate};

    fn input<'a>(text: &'a str, options: &'a LintOptions, can_ask: bool) -> CheckInput<'a> {
        CheckInput {
            text,
            dialect: Dialect::Postgres,
            catalog: &NoCatalog,
            options,
            can_ask_database: can_ask,
        }
    }

    /// Checks until the check is done, as the view does over several frames when a frame's
    /// budget runs out.
    fn check(problems: &mut Problems, input: &CheckInput<'_>, now: Instant) -> CheckOutcome {
        loop {
            let outcome = problems.check(input, now);
            if outcome.repaint_after != Some(Duration::ZERO) {
                return outcome;
            }
        }
    }

    fn messages(problems: &Problems, text: &str) -> Vec<(Severity, String)> {
        problems
            .shown()
            .iter()
            .map(|problem| (problem.severity, text[problem.range.clone()].to_owned()))
            .collect()
    }

    #[test]
    fn waits_for_typing_to_pause_then_finds_errors_and_warnings() {
        let start = Instant::now();
        let options = LintOptions::default();
        let text = "select * from t where a = null;\nselect from t where;";
        let mut problems = Problems::new(start);
        problems.edited(start);

        let early = check(&mut problems, &input(text, &options, false), start);
        assert_eq!(early.repaint_after, Some(DEBOUNCE));
        assert!(problems.shown().is_empty());

        let done = check(
            &mut problems,
            &input(text, &options, false),
            start + DEBOUNCE,
        );
        assert_eq!(done, CheckOutcome::default());
        assert_eq!(
            messages(&problems, text),
            [
                (Severity::Warning, "= null".to_owned()),
                (Severity::Error, "where".to_owned()),
            ]
        );

        let quiet = LintOptions {
            disabled: vec![LintRule::NullComparison],
            ..LintOptions::default()
        };
        check(&mut problems, &input(text, &quiet, false), start + DEBOUNCE);
        assert_eq!(
            messages(&problems, text),
            [(Severity::Error, "where".to_owned())]
        );
    }

    #[test]
    fn asks_the_database_once_and_lets_it_overrule_the_parser() {
        let start = Instant::now();
        let options = LintOptions::default();
        let text = "select nope from t; select a from t where;";
        let mut problems = Problems::new(start);

        let first = check(&mut problems, &input(text, &options, true), start);
        assert_eq!(
            first.ask_database,
            ["select nope from t", "select a from t where"]
        );
        problems.recheck(start);
        let again = check(&mut problems, &input(text, &options, true), start);
        assert!(again.ask_database.is_empty(), "already asked");

        problems.database_answered(
            &[
                (
                    "select nope from t".to_owned(),
                    StatementCheck::Rejected(Rejection {
                        message: "column \"nope\" does not exist".to_owned(),
                        position: Some(7),
                        kind: RejectionKind::UnknownName,
                    }),
                ),
                (
                    "select a from t where".to_owned(),
                    StatementCheck::Accepted { estimate: None },
                ),
            ],
            start,
        );
        check(&mut problems, &input(text, &options, true), start);
        assert_eq!(
            messages(&problems, text),
            [(Severity::Warning, "nope".to_owned())]
        );
        assert_eq!(
            problems.shown()[0].message,
            "The database says: column \"nope\" does not exist"
        );
    }

    #[test]
    fn keeps_what_the_database_estimates_an_accepted_statement_scans() {
        let start = Instant::now();
        let options = LintOptions::default();
        let text = "select * from big.events; select 1";
        let mut problems = Problems::new(start);
        check(&mut problems, &input(text, &options, true), start);
        let scan = CostEstimate::Bytes(ScanEstimate::exactly(ByteCount(1_400_000_000)));
        problems.database_answered(
            &[
                (
                    "select * from big.events".to_owned(),
                    StatementCheck::Accepted {
                        estimate: Some(scan.clone()),
                    },
                ),
                (
                    "select 1".to_owned(),
                    StatementCheck::Accepted { estimate: None },
                ),
            ],
            start,
        );
        assert_eq!(
            problems.estimate_of("select * from big.events"),
            Some(&scan)
        );
        assert_eq!(problems.estimate_of("select 1"), None);
        assert_eq!(problems.estimate_of("select 2"), None);
    }

    #[test]
    fn finds_the_name_a_positionless_message_mentions() {
        let sql = "select o.nope from orders o join Items i on true";
        let cases = [
            ("no such column: o.nope", Some(sql.find("nope").unwrap())),
            ("no such table: items", Some(sql.find("Items").unwrap())),
            ("ambiguous column name: id", None),
            ("near \"x\": syntax error", None),
        ];
        for (message, expected) in cases {
            assert_eq!(
                named_in(message, sql, Dialect::Sqlite),
                expected,
                "{message}"
            );
        }
    }

    #[test]
    fn jumps_to_the_next_or_previous_problem_wrapping_around() {
        let start = Instant::now();
        let options = LintOptions::default();
        let text = "delete from a; delete from b;";
        let mut problems = Problems::new(start);
        check(&mut problems, &input(text, &options, false), start);

        let second = text.rfind("delete").unwrap();
        let starts: Vec<usize> = problems
            .shown()
            .iter()
            .map(|problem| problem.range.start)
            .collect();
        let next = |cursor, forward| next_problem(&starts, cursor, forward);
        assert_eq!(next(0, true), Some(second));
        assert_eq!(next(second, true), Some(0));
        assert_eq!(next(second, false), Some(0));
        assert_eq!(next(0, false), Some(second));
    }
}
