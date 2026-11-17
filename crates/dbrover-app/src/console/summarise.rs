//! **Summarise** writing a statement into the console's text (`docs/charts-as-code.md`, CH5f).
//!
//! It only ever **inserts**. The statement that produced the result on screen is the user's work
//! and the thing they will run again, so the summary goes in as a new statement *below* it with
//! its own `[chart]` block, and the original is left untouched: the console then reads as the
//! derivation it is, the result on screen still matches the text above it, and CH1's search for
//! a statement by its text keeps working.
//!
//! One replacement, so one undo takes back the block and the statement together or neither.

use std::ops::Range;

use dbrover_language::QueryLanguage;
use dbrover_sql::{Summarised, Summary};

use super::chart::block::Ran;

/// The console's text after **Summarise**, and where the summary went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inserted {
    pub text: String,
    /// Where the summary was put. Empty: nothing was replaced, which is what makes the original
    /// statement safe and the undo one step.
    pub at: Range<usize>,
    /// How many bytes were written there.
    pub written: usize,
    /// The line the summary starts on, counted from 1, for the toast that says where it went.
    pub line: usize,
}

/// `text` with `summary` of `column` written below `ran`, the statement that produced the
/// result.
///
/// The statement is found where it ran or, if the text has moved since, by its text, as
/// **Save to query** finds it.
///
/// # Errors
///
/// A sentence saying why nothing was written: the statement isn't in the text any more, or the
/// engine has no way to work the summary out.
pub fn insert(
    language: QueryLanguage,
    text: &str,
    ran: &Ran<'_>,
    summary: Summary,
    column: &str,
) -> Result<Inserted, String> {
    let dialect = language
        .dialect()
        .ok_or_else(|| "Summarise writes SQL, and this console's language isn't SQL".to_owned())?;
    let commands = language.split(text);
    let index = statement_index(&commands, text, ran).ok_or(
        "The statement has changed since it ran; run it again to summarise its column".to_owned(),
    )?;
    let command = commands
        .get(index)
        .cloned()
        .ok_or_else(|| "The statement isn't in the text any more".to_owned())?;
    let summarised = summary
        .sql(dialect, &text[command.clone()], column)
        .ok_or_else(|| summary.refusal(dialect).unwrap_or_default())?;

    let at = after_the_line(text, command.end);
    let written = written(language, text, command.start, at, &summarised);
    let mut new_text = text.to_owned();
    new_text.insert_str(at, &written);
    // The line the `[chart]` block starts on, not the blank line before it: that is where a
    // reader looks when the toast says where the summary went.
    let block = at + written.find(|c: char| c != '\n' && c != '\r').unwrap_or(0);
    Ok(Inserted {
        line: new_text[..block].matches('\n').count() + 1,
        text: new_text,
        at: at..at,
        written: written.len(),
    })
}

/// Where the summary goes: after the whole line the statement ends on, or at the end of the
/// text when that line is the last and unbroken.
///
/// After the *line*, not after the statement, so a neighbour that shares it (`SELECT 1; SELECT
/// 2`) is never split and no text the user didn't act on moves. One rule, and no refusal —
/// unlike **Save to query**, which has to refuse a shared line because it writes *above*.
fn after_the_line(text: &str, end: usize) -> usize {
    match text[end..].find('\n') {
        Some(at) => end + at + 1,
        None => text.len(),
    }
}

/// The text to insert: a blank line, the `[chart]` block, and the statement, each line in the
/// script's own line breaks and under the original's indent.
fn written(
    language: QueryLanguage,
    text: &str,
    start: usize,
    at: usize,
    summarised: &Summarised,
) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let indent = match &text[line_start..start] {
        blank if blank.trim().is_empty() => blank,
        _ => "",
    };
    let mut written = String::new();
    // A text that didn't end with a line break needs one before anything else.
    if at == text.len() && !text.is_empty() && !text.ends_with('\n') {
        written.push_str(newline);
    }
    written.push_str(newline);
    written.push_str(&language.new_chart_block(text, &summarised.chart, indent));
    for line in format!("{};", summarised.sql).lines() {
        if !line.is_empty() {
            written.push_str(indent);
            written.push_str(line);
        }
        written.push_str(newline);
    }
    written
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

#[cfg(test)]
mod tests {
    use dbrover_sql::Dialect;

    use super::*;

    const LANGUAGE: QueryLanguage = QueryLanguage::Sql(Dialect::Postgres);

    /// A statement as it ran: the range `split` gives, which stops before the terminator.
    fn ran<'a>(text: &'a str, sql: &'a str) -> Ran<'a> {
        let at = text.find(sql).expect("the statement is in the text");
        Ran {
            range: at..at + sql.len(),
            sql,
        }
    }

    fn insert_into(text: &str, sql: &str) -> Inserted {
        insert(
            LANGUAGE,
            text,
            &ran(text, sql),
            Summary::Distribution { buckets: 10 },
            "price",
        )
        .expect("a distribution is written")
    }

    #[test]
    fn writes_the_summary_below_the_statement_and_leaves_it_untouched() {
        let text = "SELECT price FROM products;\n";
        let inserted = insert_into(text, "SELECT price FROM products");

        assert!(
            inserted.text.starts_with(text),
            "the original comes first, byte for byte: {:?}",
            inserted.text
        );
        assert!(inserted.text.contains("-- [chart]"));
        assert!(inserted.text.contains("-- x = \"bucket\""));
        assert!(inserted.text.contains("WITH summarised AS ("));
        assert!(inserted.text.trim_end().ends_with("ORDER BY bucket;"));
        assert_eq!(inserted.at, text.len()..text.len());
        assert_eq!(inserted.written, inserted.text.len() - text.len());
        assert_eq!(
            inserted.line, 3,
            "the block's own line, past the blank one: {:?}",
            inserted.text
        );
    }

    /// One replacement at one offset, so the editor's undo takes the block and the statement
    /// back together or not at all.
    #[test]
    fn the_whole_insertion_is_one_replacement() {
        let text = "SELECT price FROM products;\n";
        let inserted = insert_into(text, "SELECT price FROM products");

        assert!(inserted.at.is_empty(), "nothing is replaced");
        let mut undone = inserted.text.clone();
        undone.replace_range(inserted.at.start..inserted.at.start + inserted.written, "");
        assert_eq!(undone, text, "one undo gives the text back");
    }

    #[test]
    fn goes_after_the_whole_line_whatever_shares_it() {
        let cases = [
            // Its own line, with a statement after it: the summary lands between them.
            (
                "SELECT price FROM products;\nSELECT 2;\n",
                "SELECT price FROM products",
                "SELECT 2;",
            ),
            // A neighbour shares the line, so the summary lands after both and splits neither.
            (
                "SELECT price FROM products; SELECT 2;\nSELECT 3;\n",
                "SELECT price FROM products",
                "SELECT 3;",
            ),
            // Last in the file, with a line break.
            (
                "SELECT 1;\nSELECT price FROM products;\n",
                "SELECT price FROM products",
                "",
            ),
            // Last in the file, without one.
            (
                "SELECT 1;\nSELECT price FROM products;",
                "SELECT price FROM products",
                "",
            ),
        ];
        for (text, sql, follows) in cases {
            let inserted = insert_into(text, sql);
            let after = &inserted.text[inserted.at.start + inserted.written..];
            assert_eq!(after.trim(), follows, "{text:?}");
            assert!(
                inserted.text.contains("WITH summarised AS ("),
                "{text:?} summarised"
            );
            // Whatever was there is all still there, in order.
            let kept: String = inserted
                .text
                .lines()
                .filter(|line| text.lines().any(|original| original == *line))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(
                kept,
                text.trim_end_matches('\n'),
                "every line of {text:?} survives"
            );
        }
    }

    #[test]
    fn keeps_the_scripts_own_line_breaks_and_indent() {
        let text = "  SELECT price FROM products;\r\n";
        let inserted = insert_into(text, "SELECT price FROM products");

        assert!(
            !inserted.text.contains("\n\n"),
            "every break is a CRLF: {:?}",
            inserted.text
        );
        assert!(
            inserted.text.contains("  -- [chart]"),
            "the block takes the statement's indent: {:?}",
            inserted.text
        );
        assert!(inserted.text.contains("  WITH summarised AS ("));
    }

    #[test]
    fn says_why_when_there_is_nothing_to_write() {
        let text = "SELECT price FROM products;\n";
        let moved = Ran {
            range: 0..10,
            sql: "SELECT nothing LIKE THIS;",
        };
        assert_eq!(
            insert(
                LANGUAGE,
                text,
                &moved,
                Summary::Distribution { buckets: 10 },
                "price"
            ),
            Err(
                "The statement has changed since it ran; run it again to summarise its column"
                    .to_owned()
            )
        );

        // An engine with no percentile function says so rather than writing a statement that
        // would fail when it ran.
        let sqlite = QueryLanguage::Sql(Dialect::Sqlite);
        let refusal = insert(
            sqlite,
            text,
            &ran(text, "SELECT price FROM products"),
            Summary::Percentiles,
            "price",
        )
        .unwrap_err();
        assert!(
            refusal.starts_with("SQLite has no percentile function"),
            "{refusal}"
        );
    }
}
