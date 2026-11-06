//! The chart block above a statement in the console's text (CH1d): read when the statement
//! runs, written by **Save to query**, and its problems underlined in the editor.

use std::ops::Range;

use dbrover_chart::{ChartSpec, SavedChoice, write_back};
use dbrover_language::{ChartBlock, QueryLanguage};

use crate::code_editor::{EditorProblem, Severity};

/// A statement's block, as it was read from the console's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySpec {
    /// The spec's text without its comment markers, to tell an edited block from the same one.
    text: String,
    /// The spec, or every problem its keys have, each saying the console's line.
    read: Result<ChartSpec, Vec<String>>,
}

impl QuerySpec {
    fn of(block: &ChartBlock) -> Self {
        let read = ChartSpec::read(block.spec()).map_err(|problems| {
            problems
                .into_iter()
                .map(|problem| problem.moved_to(block.first_line).to_string())
                .collect()
        });
        Self {
            text: block.spec().to_owned(),
            read,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn read(&self) -> Result<&ChartSpec, &[String]> {
        self.read.as_ref().map_err(Vec::as_slice)
    }
}

/// The chart blocks of a console's text, found once for every statement of a run.
pub struct Blocks {
    commands: Vec<Range<usize>>,
    blocks: Vec<ChartBlock>,
}

impl Blocks {
    pub fn of(language: QueryLanguage, text: &str) -> Self {
        Self {
            commands: language.split(text),
            blocks: language.chart_blocks(text),
        }
    }

    /// The block above the statement at `range` of the text, if it has one.
    pub fn spec_for(&self, range: &Range<usize>) -> Option<QuerySpec> {
        let index = self.commands.iter().position(|command| command == range)?;
        self.block_of(index).map(QuerySpec::of)
    }

    fn block_of(&self, statement: usize) -> Option<&ChartBlock> {
        self.blocks
            .iter()
            .find(|block| block.statement == Some(statement))
    }
}

/// A statement that ran, as the chart tab knows it: where it was, and its text.
pub struct Ran<'a> {
    pub range: Range<usize>,
    pub sql: &'a str,
}

/// The console's text after **Save to query**.
#[derive(Debug, PartialEq, Eq)]
pub struct Saved {
    pub text: String,
    /// The bytes of the old text that were replaced.
    pub replaced: Range<usize>,
    /// How many bytes replaced them.
    pub written: usize,
    /// The statement's block, as now written.
    pub spec: QuerySpec,
}

/// Writes `saved` into the block above `ran`, or a new block above it, in `text`. The
/// statement is found where it ran or, if the text moved since, by its text.
///
/// # Errors
///
/// A sentence saying why nothing was written: the statement isn't in the text any more, it
/// shares its first line with another one, or its block isn't TOML.
pub fn save(
    language: QueryLanguage,
    text: &str,
    ran: &Ran<'_>,
    saved: &SavedChoice,
) -> Result<Saved, String> {
    let blocks = Blocks::of(language, text);
    let index = statement_index(&blocks.commands, text, ran).ok_or(
        "The statement has changed since it ran; run it again to save its chart".to_owned(),
    )?;
    let (replaced, written) = match blocks.block_of(index) {
        Some(block) => {
            let spec = write_back(Some(block.spec()), saved).map_err(|error| error.to_string())?;
            (block.range.clone(), block.rewritten(language, text, &spec))
        }
        None => {
            let start = blocks
                .commands
                .get(index)
                .map_or(0, |command| command.start);
            let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
            let indent = &text[line_start..start];
            if !indent.trim().is_empty() {
                return Err(
                    "Put the statement on a line of its own to save its chart above it".to_owned(),
                );
            }
            let spec = write_back(None, saved).map_err(|error| error.to_string())?;
            (
                line_start..line_start,
                language.new_chart_block(text, &spec, indent),
            )
        }
    };
    let mut new_text = text.to_owned();
    new_text.replace_range(replaced.clone(), &written);
    let rewritten = Blocks::of(language, &new_text);
    let spec = rewritten
        .block_of(index)
        .map(QuerySpec::of)
        .ok_or("The chart's block couldn't be read back after writing it".to_owned())?;
    Ok(Saved {
        text: new_text,
        replaced,
        written: written.len(),
        spec,
    })
}

/// The index of `ran` among `commands`: at the range it ran from if its text is still there,
/// else the command of the same text nearest to it.
fn statement_index(commands: &[Range<usize>], text: &str, ran: &Ran<'_>) -> Option<usize> {
    let same = |command: &Range<usize>| text.get(command.clone()) == Some(ran.sql);
    commands
        .iter()
        .position(|command| *command == ran.range && same(command))
        .or_else(|| {
            commands
                .iter()
                .enumerate()
                .filter(|(_, command)| same(command))
                .min_by_key(|(_, command)| command.start.abs_diff(ran.range.start))
                .map(|(index, _)| index)
        })
}

/// A problem with a chart block, underlined in the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartProblem {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
}

impl EditorProblem for ChartProblem {
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
        None
    }
}

/// The chart blocks' problems, found again whenever the console's text changes.
#[derive(Debug, Default)]
pub struct ChartProblems {
    /// The text they were found in.
    checked: Option<String>,
    found: Vec<ChartProblem>,
}

impl ChartProblems {
    /// Finds the problems again if `text` isn't what they were found in.
    pub fn refresh(&mut self, language: QueryLanguage, text: &str) {
        if self.checked.as_deref() != Some(text) {
            self.found = problems(language, text);
            self.checked = Some(text.to_owned());
        }
    }

    pub fn shown(&self) -> &[ChartProblem] {
        &self.found
    }

    pub fn count(&self, severity: Severity) -> usize {
        self.found
            .iter()
            .filter(|problem| problem.severity == severity)
            .count()
    }
}

/// The problems of every chart block in `text`: a block that belongs to no statement, and the
/// keys' problems of one that does. The columns' problems wait for a result.
pub fn problems(language: QueryLanguage, text: &str) -> Vec<ChartProblem> {
    let mut problems = Vec::new();
    for block in language.chart_blocks(text) {
        if block.statement.is_none() {
            problems.push(ChartProblem {
                range: header(text, &block),
                severity: Severity::Warning,
                message: "This chart belongs to no statement: a blank line or other text comes \
                          between it and the statement below it"
                    .to_owned(),
            });
            continue;
        }
        let Err(found) = ChartSpec::read(block.spec()) else {
            continue;
        };
        problems.extend(found.into_iter().map(|problem| ChartProblem {
            range: visible(text, block.script_range(problem.span.clone())),
            severity: Severity::Error,
            message: problem.moved_to(block.first_line).to_string(),
        }));
    }
    problems
}

/// The block's `[chart]` line.
fn header(text: &str, block: &ChartBlock) -> Range<usize> {
    let start = block.range.start;
    let end = text[start..block.range.end]
        .find(['\r', '\n'])
        .map_or(block.range.end, |at| start + at);
    start..end
}

/// `range`, or the rest of its line when it is empty, so a problem at the end of a value is
/// still underlined.
fn visible(text: &str, range: Range<usize>) -> Range<usize> {
    if !range.is_empty() {
        return range;
    }
    let line_end = text[range.start..]
        .find(['\r', '\n'])
        .map_or(text.len(), |at| range.start + at);
    let line_start = text[..range.start].rfind('\n').map_or(0, |at| at + 1);
    if line_end > range.start {
        range.start..line_end
    } else {
        line_start..line_end
    }
}

#[cfg(test)]
mod tests {
    use dbrover_chart::ChartKind;
    use dbrover_sql::Dialect;

    use super::*;

    const SQL: QueryLanguage = QueryLanguage::Sql(Dialect::Sqlite);

    const BY_MONTH: &str = "-- Net sales by month.\n\
                            -- [chart]\n\
                            -- # bars read best\n\
                            -- kind = \"bars\"\n\
                            -- x = \"month\"\n\
                            -- y = \"net\"\n\
                            SELECT month, net FROM sales;\n";

    fn lines() -> SavedChoice {
        SavedChoice {
            kind: ChartKind::Lines,
            x: "month".to_owned(),
            y: vec!["net".to_owned()],
            series: None,
            stack: false,
        }
    }

    fn ran(text: &str) -> Ran<'_> {
        let range = SQL.split(text).remove(0);
        Ran {
            sql: &text[range.clone()],
            range,
        }
    }

    #[test]
    fn finds_the_block_of_a_statement_that_ran() {
        let blocks = Blocks::of(SQL, BY_MONTH);
        let spec = blocks.spec_for(&ran(BY_MONTH).range).unwrap();
        let read = spec.read().unwrap();
        assert_eq!(read.kind, Some(ChartKind::Bars));
        assert_eq!(read.y, vec!["net".to_owned()]);
        assert_eq!(blocks.spec_for(&(0..3)), None);
    }

    #[test]
    fn a_blocks_problems_say_the_consoles_line() {
        let text = "SELECT 1;\n-- [chart]\n-- kind = \"area\"\nSELECT 2";
        let blocks = Blocks::of(SQL, text);
        let range = SQL.split(text).remove(1);
        let spec = blocks.spec_for(&range).unwrap();
        assert_eq!(
            spec.read(),
            Err(&["chart, line 3: `kind` is bars, lines or scatter, not \"area\"".to_owned()][..])
        );
    }

    #[test]
    fn saving_rewrites_only_what_changed() {
        let saved = save(SQL, BY_MONTH, &ran(BY_MONTH), &lines()).unwrap();
        assert_eq!(
            saved.text,
            BY_MONTH.replace("kind = \"bars\"", "kind = \"lines\"")
        );
        assert_eq!(saved.spec.read().unwrap().kind, Some(ChartKind::Lines));
        assert_eq!(
            &BY_MONTH[saved.replaced.clone()],
            BY_MONTH
                .lines()
                .skip(1)
                .take(5)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn saving_a_statement_without_a_block_writes_one_above_it() {
        let text = "SELECT 1;\n\n  SELECT month, net FROM sales";
        let range = SQL.split(text).remove(1);
        let ran = Ran {
            sql: &text[range.clone()],
            range,
        };
        let saved = save(SQL, text, &ran, &lines()).unwrap();
        assert_eq!(
            saved.text,
            "SELECT 1;\n\n  -- [chart]\n  -- kind = \"lines\"\n  -- x = \"month\"\n  -- y = \"net\"\n  SELECT month, net FROM sales"
        );
        assert_eq!(saved.replaced, 11..11);
        assert_eq!(saved.written, saved.text.len() - text.len());
    }

    #[test]
    fn finds_a_statement_that_moved_since_it_ran() {
        let ran_text = "SELECT month, net FROM sales";
        let ran = Ran {
            range: 0..ran_text.len(),
            sql: ran_text,
        };
        let moved = format!("SELECT 0;\n{ran_text}");
        let saved = save(SQL, &moved, &ran, &lines()).unwrap();
        assert!(
            saved.text.starts_with("SELECT 0;\n-- [chart]\n"),
            "{}",
            saved.text
        );

        let gone = save(SQL, "SELECT 0", &ran, &lines());
        assert_eq!(
            gone,
            Err(
                "The statement has changed since it ran; run it again to save its chart".to_owned()
            )
        );
        let shared = save(SQL, &format!("SELECT 0; {ran_text}"), &ran, &lines());
        assert_eq!(
            shared,
            Err("Put the statement on a line of its own to save its chart above it".to_owned())
        );
    }

    #[test]
    fn underlines_each_blocks_problems() {
        let text = "-- [chart]\n-- ys = \"net\"\nSELECT 1;\n\n-- [chart]\n\nSELECT 2";
        let found: Vec<(&str, Severity, String)> = problems(SQL, text)
            .into_iter()
            .map(|problem| (&text[problem.range], problem.severity, problem.message))
            .collect();
        assert_eq!(
            found,
            vec![
                (
                    "ys",
                    Severity::Error,
                    "chart, line 2: no key `ys`; did you mean `y`?".to_owned()
                ),
                (
                    "-- [chart]",
                    Severity::Warning,
                    "This chart belongs to no statement: a blank line or other text comes \
                     between it and the statement below it"
                        .to_owned()
                ),
            ]
        );
    }

    #[test]
    fn an_empty_problem_is_underlined_to_the_end_of_its_line() {
        assert_eq!(visible("ab\ncd", 3..3), 3..5);
        assert_eq!(visible("ab\n", 3..3), 3..3);
        assert_eq!(visible("ab", 2..2), 0..2);
    }
}
