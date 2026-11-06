//! A chart written above a statement as comment lines, `-- [chart]` and the keys under it
//! (`docs/charts-as-code.md`, CH1c). This module finds each block and its statement, and
//! writes a block's lines again around a changed spec; what the keys mean is `dbrover-chart`'s.

use std::ops::Range;

use dbrover_sql::TokenKind;

use crate::QueryLanguage;

/// The line that starts a block, once its comment marker is taken off.
const HEADER: &str = "[chart]";

/// A `[chart]` block of comment lines in a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartBlock {
    /// The block's lines in the script, from the start of the `[chart]` line to the end of its
    /// last line, without the line break after it.
    pub range: Range<usize>,
    /// The script's line the block starts on, counted from 1.
    pub first_line: usize,
    /// The statement directly below the block, by its index among
    /// [`QueryLanguage::split`]'s commands. `None` when a blank line, a line that isn't a
    /// comment, or the end of the script comes first: the block belongs to no statement.
    pub statement: Option<usize>,
    /// The block's text without its comment markers: TOML, a line for each of the block's.
    spec: String,
    lines: Vec<BlockLine>,
}

/// One line of a block.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockLine {
    /// The whole line in the script, without its line break.
    script: Range<usize>,
    /// Where the spec's text of it starts, in the script and in the spec.
    script_start: usize,
    spec_start: usize,
}

impl ChartBlock {
    /// The block's spec: its lines without their comment markers, and without the one space
    /// after a marker.
    pub fn spec(&self) -> &str {
        &self.spec
    }

    /// The bytes of the script that `span`, a range of [`Self::spec`], was read from: what to
    /// underline for a problem the spec has there.
    pub fn script_range(&self, span: Range<usize>) -> Range<usize> {
        let start = self.script_offset(span.start);
        let end = self.script_offset(span.end).max(start);
        start..end
    }

    fn script_offset(&self, spec_offset: usize) -> usize {
        let line = self
            .lines
            .iter()
            .rev()
            .find(|line| line.spec_start <= spec_offset)
            .or(self.lines.first());
        line.map_or(self.range.start, |line| {
            (line.script_start + spec_offset - line.spec_start.min(spec_offset))
                .min(line.script.end)
        })
    }

    /// The block's text written again for `spec`, a changed spec, to replace [`Self::range`]
    /// in `script`. A line of the spec that is still there keeps its line of the script as it
    /// was, marker and spaces included; a new or changed one is written with `language`'s
    /// marker and the `[chart]` line's indent.
    pub fn rewritten(&self, language: QueryLanguage, script: &str, spec: &str) -> String {
        let old: Vec<&str> = self.spec.split('\n').collect();
        let new: Vec<&str> = spec.lines().collect();
        let kept = common_lines(&old, &new);
        let indent = self
            .lines
            .first()
            .and_then(|line| script.get(line.script.clone()))
            .map_or("", leading_space);
        let lines: Vec<String> = new
            .iter()
            .enumerate()
            .map(|(index, text)| {
                kept.iter()
                    .find(|(_, new)| *new == index)
                    .and_then(|(old, _)| self.lines.get(*old))
                    .and_then(|line| script.get(line.script.clone()))
                    .map_or_else(|| commented(language, indent, text), str::to_owned)
            })
            .collect();
        lines.join(line_break(script))
    }
}

impl QueryLanguage {
    /// The marker of a comment to the end of the line, as DbRover writes one: `--` in SQL and
    /// PartiQL, `//` in MongoDB's shell syntax, `#` for Redis and Elasticsearch. A block is
    /// read after any line comment the language has, such as MySQL's `#`.
    pub fn line_comment(self) -> &'static str {
        match self {
            Self::Sql(_) | Self::PartiQl => "--",
            Self::Mongo => "//",
            Self::Redis | Self::Search => "#",
            #[cfg(any(test, feature = "test-language"))]
            Self::Test => "#",
        }
    }

    /// Every `[chart]` block of comment lines in `script`, in order, each with the statement
    /// directly below it, if one is.
    ///
    /// A block runs from a comment line holding only `[chart]` to the last comment line after
    /// it, or the line before the next `[chart]`. Comment lines are the language's line
    /// comments alone on their line, so a `-- [chart]` inside a string or a block comment is
    /// none.
    pub fn chart_blocks(self, script: &str) -> Vec<ChartBlock> {
        let comment_starts: Vec<usize> = self
            .tokens(script)
            .into_iter()
            .filter(|token| token.kind == TokenKind::LineComment)
            .map(|token| token.range.start)
            .collect();
        let lines: Vec<Line<'_>> = lines(script)
            .map(|line| Line::read(line, &comment_starts))
            .collect();
        let commands = self.split(script);
        let mut blocks = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            let Some(block_lines) = block_at(&lines[index..]) else {
                index += 1;
                continue;
            };
            let below = lines.get(index + block_lines.len());
            let statement = below.and_then(|line| {
                commands
                    .iter()
                    .position(|command| command.start == line.first_meaningful)
            });
            blocks.push(ChartBlock::new(&block_lines, index + 1, statement));
            index += block_lines.len();
        }
        blocks
    }

    /// The text of a new block of `spec`, to insert at the start of a statement's line, which
    /// begins with `indent`: each line commented, the last one ending with a line break, in
    /// `script`'s own line breaks.
    pub fn new_chart_block(self, script: &str, spec: &str, indent: &str) -> String {
        let newline = line_break(script);
        spec.lines()
            .map(|line| format!("{}{newline}", commented(self, indent, line)))
            .collect()
    }
}

impl ChartBlock {
    fn new(lines: &[&Line<'_>], first_line: usize, statement: Option<usize>) -> Self {
        let mut spec = String::new();
        let mut block_lines = Vec::new();
        for (position, line) in lines.iter().enumerate() {
            if position > 0 {
                spec.push('\n');
            }
            let Some(body) = &line.comment else {
                continue;
            };
            block_lines.push(BlockLine {
                script: line.range.clone(),
                script_start: body.start,
                spec_start: spec.len(),
            });
            spec.push_str(body.text);
        }
        let start = lines.first().map_or(0, |line| line.range.start);
        let end = lines.last().map_or(start, |line| line.range.end);
        Self {
            range: start..end,
            first_line,
            statement,
            spec,
            lines: block_lines,
        }
    }
}

/// A line of the script, as a block sees it.
struct Line<'a> {
    /// Without its line break.
    range: Range<usize>,
    /// Where its first character that isn't a space is.
    first_meaningful: usize,
    /// What follows the comment marker, when the line is a line comment alone.
    comment: Option<Body<'a>>,
}

/// A comment line's text after its marker and the one space after that.
struct Body<'a> {
    start: usize,
    text: &'a str,
}

impl<'a> Line<'a> {
    fn read((start, text): (usize, &'a str), comment_starts: &[usize]) -> Self {
        let indent = leading_space(text).len();
        let first_meaningful = start + indent;
        let comment = comment_starts
            .binary_search(&first_meaningful)
            .ok()
            .and_then(|_| body(&text[indent..]))
            .map(|(offset, body)| Body {
                start: first_meaningful + offset,
                text: body,
            });
        Self {
            range: start..start + text.len(),
            first_meaningful,
            comment,
        }
    }

    fn is_header(&self) -> bool {
        self.comment
            .as_ref()
            .is_some_and(|body| without_toml_comment(body.text).trim() == HEADER)
    }
}

/// The lines of a block starting at `lines[0]`, if a `[chart]` line is there.
fn block_at<'l, 'a>(lines: &'l [Line<'a>]) -> Option<Vec<&'l Line<'a>>> {
    let (header, rest) = lines.split_first()?;
    if !header.is_header() {
        return None;
    }
    let mut block = vec![header];
    block.extend(
        rest.iter()
            .take_while(|line| line.comment.is_some() && !line.is_header()),
    );
    Some(block)
}

/// A line comment's text after its marker: whatever the marker (`--`, `//` or `#`), and
/// without one space after it. Returns where that text starts in `comment`.
fn body(comment: &str) -> Option<(usize, &str)> {
    let marker = ["--", "//", "#"]
        .into_iter()
        .find(|marker| comment.starts_with(marker))?;
    let after = &comment[marker.len()..];
    let offset = marker.len() + usize::from(after.starts_with(' '));
    Some((offset, &comment[offset..]))
}

/// `text` without a TOML comment after it, for reading a header written `[chart]  # sales`.
fn without_toml_comment(text: &str) -> &str {
    text.split_once('#').map_or(text, |(before, _)| before)
}

/// Each line of `script` with where it starts, without its line break (`\n` or `\r\n`).
fn lines(script: &str) -> impl Iterator<Item = (usize, &str)> {
    script.split('\n').scan(0, |start, line| {
        let at = *start;
        *start += line.len() + 1;
        Some((at, line.strip_suffix('\r').unwrap_or(line)))
    })
}

/// The line break `script` uses: `\r\n` if its first line ends with one.
fn line_break(script: &str) -> &'static str {
    match script.find('\n') {
        Some(at) if script[..at].ends_with('\r') => "\r\n",
        _ => "\n",
    }
}

fn leading_space(text: &str) -> &str {
    &text[..text.len() - text.trim_start().len()]
}

/// `text` as a comment line of `language`, after `indent`.
fn commented(language: QueryLanguage, indent: &str, text: &str) -> String {
    let marker = language.line_comment();
    if text.is_empty() {
        format!("{indent}{marker}")
    } else {
        format!("{indent}{marker} {text}")
    }
}

/// The lines `old` and `new` have in common, in order, as pairs of their indexes: the longest
/// such sequence, so a spec with one value changed keeps every other line.
fn common_lines(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    // lengths[i][j]: the longest common sequence of old[i..] and new[j..].
    let mut lengths = vec![vec![0_usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lengths[i][j] = if old[i] == new[j] {
                lengths[i + 1][j + 1] + 1
            } else {
                lengths[i + 1][j].max(lengths[i][j + 1])
            };
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if lengths[i + 1][j] >= lengths[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use dbrover_sql::Dialect;

    use super::*;

    const SQL: QueryLanguage = QueryLanguage::Sql(Dialect::Sqlite);

    /// A block as its spec and the start of its statement's text, if it has one.
    type Found = (String, Option<String>);

    fn found(language: QueryLanguage, script: &str) -> Vec<Found> {
        let commands = language.split(script);
        language
            .chart_blocks(script)
            .into_iter()
            .map(|block| {
                let statement = block
                    .statement
                    .and_then(|index| commands.get(index))
                    .map(|range| script[range.clone()].chars().take(8).collect());
                (block.spec().to_owned(), statement)
            })
            .collect()
    }

    fn block(spec: &str, statement: Option<&str>) -> Found {
        (spec.to_owned(), statement.map(str::to_owned))
    }

    #[test]
    fn finds_the_block_above_each_statement() {
        let cases: [(&str, QueryLanguage, &str, Vec<Found>); 12] = [
            (
                "a block above a statement",
                SQL,
                "-- [chart]\n-- kind = \"bars\"\nSELECT 1",
                vec![block("[chart]\nkind = \"bars\"", Some("SELECT 1"))],
            ),
            (
                "prose above the block, a TOML comment inside it",
                SQL,
                "-- Net sales.\n-- [chart]\n-- # by month\n-- x = \"month\"\nSELECT 1;",
                vec![block(
                    "[chart]\n# by month\nx = \"month\"",
                    Some("SELECT 1"),
                )],
            ),
            (
                "an empty chart",
                SQL,
                "--[chart]\nSELECT 1",
                vec![block("[chart]", Some("SELECT 1"))],
            ),
            (
                "a blank line ends the block before its statement",
                SQL,
                "-- [chart]\n-- kind = \"bars\"\n\nSELECT 1",
                vec![block("[chart]\nkind = \"bars\"", None)],
            ),
            (
                "a block at the end of the script",
                SQL,
                "SELECT 1;\n-- [chart]",
                vec![block("[chart]", None)],
            ),
            (
                "a block comment between it and the statement",
                SQL,
                "-- [chart]\n/* note */ SELECT 1",
                vec![block("[chart]", None)],
            ),
            (
                "a block inside a statement",
                SQL,
                "SELECT 1\n-- [chart]\nFROM t",
                vec![block("[chart]", None)],
            ),
            (
                "a block for each statement",
                SQL,
                "-- [chart]\nSELECT 1;\n\n  -- [chart]\n  -- y = \"n\"\n  SELECT 2",
                vec![
                    block("[chart]", Some("SELECT 1")),
                    block("[chart]\ny = \"n\"", Some("SELECT 2")),
                ],
            ),
            (
                "a second header starts a second block",
                SQL,
                "-- [chart]\n-- [chart]  # again\nSELECT 1",
                vec![
                    block("[chart]", None),
                    block("[chart]  # again", Some("SELECT 1")),
                ],
            ),
            ("not in a string", SQL, "SELECT '\n-- [chart]\n'", vec![]),
            (
                "MongoDB's comments",
                QueryLanguage::Mongo,
                "// [chart]\n// kind = \"lines\"\ndb.sales.find()",
                vec![block("[chart]\nkind = \"lines\"", Some("db.sales"))],
            ),
            (
                "Redis's comments",
                QueryLanguage::Redis,
                "# [chart]\nGET a",
                vec![block("[chart]", Some("GET a"))],
            ),
        ];
        for (what, language, script, expected) in cases {
            assert_eq!(found(language, script), expected, "{what}");
        }
    }

    #[test]
    fn reads_mysqls_hash_comments_and_windows_line_breaks() {
        let mysql = QueryLanguage::Sql(Dialect::Mysql);
        let script = "# [chart]\r\n# x = \"month\"\r\nSELECT 1";
        let blocks = mysql.chart_blocks(script);
        assert_eq!(blocks[0].spec(), "[chart]\nx = \"month\"");
        assert_eq!(blocks[0].statement, Some(0));
        assert_eq!(
            &script[blocks[0].range.clone()],
            "# [chart]\r\n# x = \"month\""
        );
    }

    #[test]
    fn keeps_where_each_byte_of_the_spec_came_from() {
        let script = "SELECT 0;\n\n  -- [chart]\n  --kind = \"area\"\n  SELECT 1";
        let block = &SQL.chart_blocks(script)[0];
        assert_eq!(block.first_line, 3);
        assert_eq!(block.statement, Some(1));
        let spec = block.spec();
        let area = spec.find("\"area\"").unwrap();
        let in_script = block.script_range(area..area + "\"area\"".len());
        assert_eq!(&script[in_script], "\"area\"");
        assert_eq!(&script[block.script_range(0..0)], "");
        assert_eq!(
            block.script_range(0..0).start,
            script.find("[chart]").unwrap()
        );
    }

    #[test]
    fn rewrites_only_the_lines_that_changed() {
        let script =
            "--  [chart]\n--   # bars read best\n-- kind = \"bars\"\n--x = \"month\"\nSELECT 1";
        let block = &SQL.chart_blocks(script)[0];
        let spec = block.spec().replace("bars\"", "lines\"");
        assert_eq!(
            block.rewritten(SQL, script, &spec),
            "--  [chart]\n--   # bars read best\n-- kind = \"lines\"\n--x = \"month\""
        );
        let longer = format!("{}\ny = \"net\"\n", block.spec());
        assert_eq!(
            block.rewritten(SQL, script, &longer),
            "--  [chart]\n--   # bars read best\n-- kind = \"bars\"\n--x = \"month\"\n-- y = \"net\""
        );
    }

    #[test]
    fn writes_a_new_block_in_the_scripts_line_breaks() {
        let spec = "[chart]\nkind = \"bars\"\n\nx = \"month\"\n";
        assert_eq!(
            SQL.new_chart_block("SELECT 1", spec, "  "),
            "  -- [chart]\n  -- kind = \"bars\"\n  --\n  -- x = \"month\"\n"
        );
        assert_eq!(
            QueryLanguage::Mongo.new_chart_block("a\r\nb", "[chart]\n", ""),
            "// [chart]\r\n"
        );
    }

    #[test]
    fn a_new_block_reads_back_as_written() {
        let spec = "[chart]\nkind = \"bars\"";
        for language in [
            SQL,
            QueryLanguage::Mongo,
            QueryLanguage::Redis,
            QueryLanguage::Search,
        ] {
            let script = format!("{}x", language.new_chart_block("", spec, ""));
            let blocks = language.chart_blocks(&script);
            assert_eq!(blocks.len(), 1, "{script}");
            assert_eq!(blocks[0].spec(), spec, "{script}");
        }
    }

    #[test]
    fn lines_in_common_keep_their_order() {
        assert_eq!(
            common_lines(&["a", "b", "c"], &["a", "x", "c", "d"]),
            vec![(0, 0), (2, 2)]
        );
        assert_eq!(common_lines(&[], &["a"]), vec![]);
    }
}
