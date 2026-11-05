//! **Format** for a report file (`docs/report-editor.md`, RP8g): its TOML laid out, and each
//! SQL step formatted as a console formats a statement, never changing what the file says.
//!
//! In three steps:
//!
//! 1. **Lay out the TOML**, from `toml_parser`'s tokens: `key = value` with one space each side,
//!    no indentation before keys and headers, one blank line before each table's header and
//!    the comments right above it, no more than one blank line anywhere, no trailing spaces,
//!    one line break at the end. Arrays and inline tables are left as written: the examples
//!    align their columns by hand, and a formatter that undid that would be fought.
//! 2. **Format each SQL step** in its dialect with `dbrover_sql::format_ranges`, written back
//!    as a multi-line string that starts on the line after its key. PRQL, and a step whose
//!    data source isn't known, are left as written.
//! 3. **Check the meaning**: both files parsed, each formatted step compared with its original
//!    by `dbrover_sql::same_meaning`, then everything else compared as TOML values. A format
//!    that would change anything is refused whole.

use dbrover_core::DataSource;
use dbrover_language::QueryLanguage;
use dbrover_sql::{Dialect, FormatOptions, format_ranges, same_meaning, split_statements};
use toml_parser::Source;
use toml_parser::lexer::{Token, TokenKind};

use crate::embedded::{StepLanguage, embedded_code};
use crate::plan::find_data_source;
use crate::problem::syntax_problems;

/// A file formatted, and the steps left as written, each with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedReport {
    pub text: String,
    pub left_as_written: Vec<String>,
}

/// Why a file wasn't formatted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormatReportError {
    #[error("the file isn't TOML yet; fix its syntax first")]
    NotToml,
    #[error("formatting would change what the file says, so it was left as it is")]
    ChangedMeaning,
}

/// `text` formatted, the SQL in the dialect of each source's data source among
/// `data_sources`, laid out as `options` say.
///
/// # Errors
///
/// [`FormatReportError::NotToml`] for a file with syntax errors, and
/// [`FormatReportError::ChangedMeaning`] if the result wouldn't say what `text` does.
pub fn format_report(
    text: &str,
    data_sources: &[DataSource],
    options: &FormatOptions,
) -> Result<FormattedReport, FormatReportError> {
    if !syntax_problems(text).is_empty() {
        return Err(FormatReportError::NotToml);
    }
    let laid_out = lay_out(text);
    let (formatted, left_as_written) = format_steps(&laid_out, data_sources, options);
    if !means_the_same(text, &formatted, data_sources) {
        return Err(FormatReportError::ChangedMeaning);
    }
    Ok(FormattedReport {
        text: formatted,
        left_as_written,
    })
}

/// What starts a line, for where blank lines go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Blank,
    Comment,
    Header,
    Other,
}

/// A line of the file, split only where TOML breaks lines: a multi-line string stays inside
/// the line its key is on.
struct Line {
    text: String,
    kind: LineKind,
}

fn lay_out(text: &str) -> String {
    let lines = lines(text);
    let line_break = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let starts = block_starts(&lines);
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut blank_before = false;
    for (index, line) in lines.iter().enumerate() {
        if line.kind == LineKind::Blank {
            blank_before = true;
            continue;
        }
        if !out.is_empty() && (blank_before || starts.contains(&index)) {
            out.push("");
        }
        blank_before = false;
        out.push(&line.text);
    }
    let mut laid_out = out.join(line_break);
    laid_out.push_str(line_break);
    laid_out
}

/// The first line of each table header's block: the header, or the comments right above it.
fn block_starts(lines: &[Line]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.kind == LineKind::Header)
        .map(|(header, _)| {
            let mut start = header;
            while start > 0 && lines[start - 1].kind == LineKind::Comment {
                start -= 1;
            }
            start
        })
        .collect()
}

/// The file's lines, each with its spacing laid out.
fn lines(text: &str) -> Vec<Line> {
    let tokens = Source::new(text).lex().into_vec();
    let mut writer = LineWriter::default();
    for (index, token) in tokens.iter().enumerate() {
        let raw = &text[token.span().start()..token.span().end()];
        writer.take(token, raw, tokens.get(index + 1));
    }
    writer.finish()
}

/// Writes tokens into lines, laying out what Format lays out.
#[derive(Default)]
struct LineWriter {
    lines: Vec<Line>,
    text: String,
    kind: Option<LineKind>,
    /// The brackets of arrays and inline tables open around the token.
    open: Vec<TokenKind>,
    in_header: bool,
    /// Just wrote ` = ` between a key and its value, so the spaces after `=` are dropped.
    after_equals: bool,
}

impl LineWriter {
    fn take(&mut self, token: &Token, raw: &str, next: Option<&Token>) {
        let kind = token.kind();
        let top_level = self.open.is_empty() && !self.in_header;
        match kind {
            TokenKind::Newline => self.end_line(),
            TokenKind::Eof => {}
            TokenKind::Whitespace => {
                let starts_line = self.kind.is_none() && self.open.is_empty();
                let ends_line = next
                    .is_none_or(|next| matches!(next.kind(), TokenKind::Newline | TokenKind::Eof));
                let around_equals = top_level
                    && (self.after_equals
                        || next.is_some_and(|next| next.kind() == TokenKind::Equals));
                if !(starts_line || ends_line || around_equals) {
                    self.text.push_str(raw);
                }
            }
            TokenKind::Equals if top_level => {
                self.text.push_str(" = ");
                self.after_equals = true;
                return;
            }
            TokenKind::LeftSquareBracket if self.kind.is_none() && self.open.is_empty() => {
                self.in_header = true;
                self.write(raw, LineKind::Header);
            }
            TokenKind::LeftSquareBracket | TokenKind::LeftCurlyBracket if !self.in_header => {
                self.open.push(kind);
                self.write(raw, LineKind::Other);
            }
            TokenKind::RightSquareBracket | TokenKind::RightCurlyBracket if !self.in_header => {
                self.open.pop();
                self.write(raw, LineKind::Other);
            }
            TokenKind::Comment => self.write(raw, LineKind::Comment),
            _ => self.write(raw, LineKind::Other),
        }
        if kind != TokenKind::Whitespace {
            self.after_equals = false;
        }
    }

    /// Writes a token, the line taking `kind` if this is its first token.
    fn write(&mut self, raw: &str, kind: LineKind) {
        self.kind.get_or_insert(kind);
        self.text.push_str(raw);
    }

    fn end_line(&mut self) {
        let text = std::mem::take(&mut self.text);
        let kind = self.kind.take().unwrap_or(LineKind::Blank);
        self.in_header = false;
        self.lines.push(Line {
            text: text.trim_end_matches([' ', '\t']).to_owned(),
            kind,
        });
    }

    fn finish(mut self) -> Vec<Line> {
        if self.kind.is_some() {
            self.end_line();
        }
        self.lines
    }
}

/// Each SQL step formatted, from the last to the first so the earlier ones' places hold; and
/// the steps left as written, with why.
fn format_steps(
    text: &str,
    data_sources: &[DataSource],
    options: &FormatOptions,
) -> (String, Vec<String>) {
    let mut formatted = text.to_owned();
    let mut left = Vec::new();
    for code in embedded_code(text).into_iter().rev() {
        let Some(dialect) = dialect_of(&code.language, data_sources) else {
            continue;
        };
        let sql = &text[code.range.clone()];
        let statements = split_statements(sql, dialect);
        let sql_formatted = match format_ranges(sql, &statements, dialect, options) {
            Ok(done) => done.into_text(),
            Err(error) => {
                let line = text[..code.range.start].matches('\n').count() + 1;
                left.push(format!("line {line}: the SQL is left as written: {error}"));
                continue;
            }
        };
        let original = &text[code.string.clone()];
        if let Some(string) = multi_line_string(original, sql_formatted.trim()) {
            formatted.replace_range(code.string.clone(), &with_line_breaks(&string, text));
        }
    }
    left.reverse();
    (formatted, left)
}

/// `string` with the line breaks `file` uses: CRLF in a file that has them, as Git on Windows
/// checks files out, so formatting never mixes the two.
fn with_line_breaks(string: &str, file: &str) -> String {
    let lines = string.replace("\r\n", "\n");
    if file.contains("\r\n") {
        lines.replace('\n', "\r\n")
    } else {
        lines
    }
}

/// The SQL dialect of a step, if it is SQL in a dialect DbRover knows.
fn dialect_of(language: &StepLanguage, data_sources: &[DataSource]) -> Option<Dialect> {
    match language {
        StepLanguage::Source { data_source } => {
            let data_source = find_data_source(data_source, data_sources).ok()?;
            QueryLanguage::from(data_source.config.driver()).dialect()
        }
        StepLanguage::DuckDb => Some(Dialect::DuckDb),
        StepLanguage::Prql => None,
    }
}

/// `sql` as a multi-line string that starts on the line after its key, in the quotes
/// `original` used: `"""` for a basic string, `'''` for a literal one. A basic string whose
/// SQL holds a backslash, where the string would read an escape, becomes literal; a literal
/// one whose SQL holds its own closing quotes is left as written (`None`).
fn multi_line_string(original: &str, sql: &str) -> Option<String> {
    let literal = original.starts_with('\'') || sql.contains('\\') || sql.contains("\"\"\"");
    let quotes = if literal { "'''" } else { "\"\"\"" };
    if sql.contains(quotes) {
        return None;
    }
    Some(format!("{quotes}\n{sql}\n{quotes}"))
}

/// Where a step's SQL is in the file as a TOML value.
#[derive(Debug, Clone, Copy)]
enum Step {
    Source(usize),
    Combine,
}

/// Whether `after` says what `before` does: the same TOML values, but for SQL steps that
/// differ only in their layout and keywords' case.
fn means_the_same(before: &str, after: &str, data_sources: &[DataSource]) -> bool {
    let (Ok(before), Ok(mut after)) = (
        toml::from_str::<toml::Table>(before),
        toml::from_str::<toml::Table>(after),
    ) else {
        return false;
    };
    for (step, dialect) in sql_steps(&before, data_sources) {
        let original = query(&before, step).cloned();
        let Some(written) = query_mut(&mut after, step) else {
            continue;
        };
        if let (Some(toml::Value::String(original)), toml::Value::String(formatted)) =
            (original, &*written)
            && same_meaning(&original, formatted, dialect)
        {
            *written = toml::Value::String(original);
        }
    }
    before == after
}

/// Each SQL step of a file, and its dialect.
fn sql_steps(file: &toml::Table, data_sources: &[DataSource]) -> Vec<(Step, Dialect)> {
    let sources = file
        .get("source")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, source)| {
            let name = source.get("data_source")?.as_str()?;
            let language = StepLanguage::Source {
                data_source: name.to_owned(),
            };
            Some((Step::Source(index), dialect_of(&language, data_sources)?))
        });
    sources
        .chain(std::iter::once((Step::Combine, Dialect::DuckDb)))
        .collect()
}

fn query(file: &toml::Table, step: Step) -> Option<&toml::Value> {
    match step {
        Step::Source(index) => file.get("source")?.as_array()?.get(index)?.get("query"),
        Step::Combine => file.get("combine")?.get("query"),
    }
}

fn query_mut(file: &mut toml::Table, step: Step) -> Option<&mut toml::Value> {
    let table = match step {
        Step::Source(index) => file
            .get_mut("source")?
            .as_array_mut()?
            .get_mut(index)?
            .as_table_mut()?,
        Step::Combine => file.get_mut("combine")?.as_table_mut()?,
    };
    table.get_mut("query")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use dbrover_core::{ConnectionConfig, DataSourceId, SafetyLevel, SqliteConfig};

    use super::*;

    fn shop() -> Vec<DataSource> {
        vec![DataSource {
            id: DataSourceId::generate(),
            name: "shop".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Sqlite(SqliteConfig {
                path: PathBuf::from("/tmp/shop.db"),
                read_only: true,
            }),
        }]
    }

    fn format(text: &str) -> String {
        format_report(text, &shop(), &FormatOptions::default())
            .expect(text)
            .text
    }

    #[test]
    fn lays_out_the_toml_and_keeps_comments_and_hand_aligned_arrays() {
        let text = "# A report.\n[report]\n  name   =\"R\"   \n\n\n\nparams = { since = \"date\" }\n\
                    # The source.\n[[source]]\nid=\"lines\"\ndata_source = \"other\"\n\
                    query = 'select 1'\ncolumns = [\n  { name = \"a\",   type = \"text\" },\n]\n";
        // The comment right above [report] is its block's, so no blank line comes between.
        let expected = "# A report.\n[report]\nname = \"R\"\n\nparams = { since = \"date\" }\n\n\
                        # The source.\n[[source]]\nid = \"lines\"\ndata_source = \"other\"\n\
                        query = 'select 1'\ncolumns = [\n  { name = \"a\",   type = \"text\" },\n]\n";
        assert_eq!(format(text), expected);
    }

    #[test]
    fn formats_each_sql_step_in_a_multi_line_string_after_its_key() {
        let text = "[report]\nname = \"R\"\n\n[[source]]\nid = \"lines\"\ndata_source = \"shop\"\n\
                    query = \"select a, b from t where a > 1\"\n\n[combine]\n\
                    query = '''select a from lines'''\n";
        let formatted = format(text);
        assert!(
            formatted.contains("query = \"\"\"\nSELECT\n"),
            "the source's query, upper-cased and on lines of its own:\n{formatted}"
        );
        assert!(
            formatted.contains("query = '''\nSELECT"),
            "the combine's, in literal quotes as written:\n{formatted}"
        );
        assert_eq!(
            format(&formatted),
            formatted,
            "formatting twice changes nothing"
        );
    }

    #[test]
    fn leaves_prql_and_unknown_data_sources_as_written() {
        let text = "[report]\nname = \"R\"\n\n[[source]]\nid = \"a\"\ndata_source = \"gone\"\n\
                    query = \"select 1\"\n\n[combine]\nprql = \"from a | select {x}\"\n";
        assert_eq!(format(text), text);
    }

    #[test]
    fn tells_a_change_of_layout_from_a_change_of_meaning() {
        let before = "[[source]]\ndata_source = \"shop\"\nquery = \"select a from t\"\n";
        let laid_out =
            "[[source]]\ndata_source = \"shop\"\nquery = \"\"\"\nSELECT a\nFROM t\n\"\"\"\n";
        let renamed = "[[source]]\ndata_source = \"shop\"\nquery = \"select b from t\"\n";
        let elsewhere = "[[source]]\ndata_source = \"shops\"\nquery = \"select a from t\"\n";
        assert!(means_the_same(before, laid_out, &shop()));
        assert!(!means_the_same(before, renamed, &shop()));
        assert!(!means_the_same(before, elsewhere, &shop()));
    }

    #[test]
    fn keeps_crlf_line_breaks() {
        let example = include_str!("../../../examples/reports/sales-by-category.toml");
        let crlf = example.replace('\n', "\r\n");
        assert_eq!(lay_out(&crlf), crlf);
        let formatted = format(&crlf);
        assert_eq!(format(&formatted), formatted);
        assert!(
            !formatted.replace("\r\n", "").contains('\n'),
            "every line break is CRLF"
        );
    }

    #[test]
    fn refuses_a_file_that_isnt_toml() {
        let refused = format_report("[report\nname = 1\n", &[], &FormatOptions::default());
        assert_eq!(refused, Err(FormatReportError::NotToml));
    }

    #[test]
    fn formats_the_examples_once_and_for_all() {
        let examples = [
            include_str!("../../../examples/reports/battery-cycles.toml"),
            include_str!("../../../examples/reports/battery-flows.toml"),
            include_str!("../../../examples/reports/order-history.toml"),
            include_str!("../../../examples/reports/sales-by-category.toml"),
            include_str!("../../../examples/reports/sales-by-week.toml"),
        ];
        for example in examples {
            // Git on Windows checks the examples out with CRLF, which Format keeps.
            let line_break = if example.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            let once = format(example);
            assert_eq!(format(&once), once, "formatting twice changes nothing");
            // An example indented by hand comes back as the example formatted.
            let indented: String = example
                .lines()
                .map(|line| {
                    let key = line.split_once(" = ").is_some_and(|(key, _)| {
                        key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    });
                    // Spaces after a multi-line string's opening quotes are its value's.
                    let opens_string = line.ends_with("\"\"\"") || line.ends_with("'''");
                    if (key || line.starts_with('[')) && !opens_string {
                        format!("    {line}   {line_break}")
                    } else if key {
                        format!("    {line}{line_break}")
                    } else {
                        format!("{line}{line_break}")
                    }
                })
                .collect();
            assert_eq!(format(&indented), once);
            // Laying out alone leaves the examples, which are laid out already, as they are.
            assert_eq!(lay_out(example), example);
        }
    }
}
