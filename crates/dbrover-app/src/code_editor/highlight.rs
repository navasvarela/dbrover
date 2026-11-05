//! Syntax highlighting for the code editor. A [`Highlighter`] says what each piece of the text
//! is; this module alone decides how each role is drawn, and adds the marks (an error, the
//! current statement, the find bar's matches) over it.
//!
//! A console's language highlights from the same lexer that splits statements, so what looks
//! like a string is exactly what the splitter treats as one.

use std::ops::Range;
use std::sync::Arc;

use dbrover_language::{QueryLanguage, TokenKind};
use egui::text::LayoutJob;
use egui::{FontId, Galley, Stroke, Style, TextBuffer, TextFormat, TextStyle, Ui};

use crate::theme::SyntaxColors;

const ERROR_UNDERLINE_WIDTH: f32 = 1.5;
/// How far the current statement's background moves from the editor's toward the text
/// colour: enough to see which statement ⌘↵ would run, faint enough to read through.
const CURRENT_STATEMENT_TINT: f32 = 0.07;

/// Byte ranges of the text drawn differently from their syntax colours.
#[derive(Debug, Default, Clone)]
pub struct Marks {
    /// Underlined in red: where the last failure points.
    pub error: Option<Range<usize>>,
    /// Shaded: the statement the cursor is in.
    pub current_statement: Option<Range<usize>>,
    /// Shaded as the current statement is: code inside another language's text, such as a
    /// report step's SQL inside its TOML, so where it starts and stops shows.
    pub code_blocks: Vec<Range<usize>>,
    /// Highlighted: what the find bar found, in order.
    pub matches: Vec<Range<usize>>,
    /// Highlighted more strongly: the match the find bar selected.
    pub current_match: Option<Range<usize>>,
}

/// How strongly the find bar's matches are highlighted, as a share of the selection colour:
/// enough to spot every match, while the selected one stands out at full strength.
const MATCH_STRENGTH: f32 = 0.45;

/// What a piece of code is, which decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxRole {
    Keyword,
    String,
    Number,
    Comment,
    /// A quoted name, such as a SQL identifier in double quotes.
    Identifier,
    Plain,
}

/// A byte range of the text and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub role: SyntaxRole,
}

/// Says what each piece of a text is: a query language's lexer, or a report file's.
pub trait Highlighter {
    /// Spans in order, not overlapping. Text they leave out is drawn plain.
    fn spans(&self, text: &str) -> Vec<Span>;
}

impl Highlighter for QueryLanguage {
    fn spans(&self, text: &str) -> Vec<Span> {
        self.tokens(text)
            .into_iter()
            .map(|token| {
                let keyword =
                    token.kind == TokenKind::Word && self.is_keyword(&text[token.range.clone()]);
                Span {
                    role: role_of(token.kind, keyword),
                    range: token.range,
                }
            })
            .collect()
    }
}

fn role_of(kind: TokenKind, keyword: bool) -> SyntaxRole {
    match kind {
        TokenKind::Word if keyword => SyntaxRole::Keyword,
        TokenKind::String => SyntaxRole::String,
        TokenKind::Number => SyntaxRole::Number,
        TokenKind::LineComment | TokenKind::BlockComment => SyntaxRole::Comment,
        TokenKind::QuotedIdentifier => SyntaxRole::Identifier,
        TokenKind::Word | TokenKind::Whitespace | TokenKind::Semicolon | TokenKind::Punctuation => {
            SyntaxRole::Plain
        }
    }
}

/// A `TextEdit` layouter that highlights as [`layout_job`] does, wrapping at the editor's
/// width.
pub fn layouter<'a>(
    highlighter: &'a dyn Highlighter,
    marks: &'a Marks,
) -> impl FnMut(&Ui, &dyn TextBuffer, f32) -> Arc<Galley> + 'a {
    move |ui, text, wrap_width| {
        let mut job = layout_job(text.as_str(), highlighter, marks, ui.style());
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    }
}

/// Lays out `text` with the highlighter's colours and the `marks`.
pub fn layout_job(
    text: &str,
    highlighter: &dyn Highlighter,
    marks: &Marks,
    style: &Style,
) -> LayoutJob {
    let font = TextStyle::Monospace.resolve(style);
    let colors = SyntaxColors::for_visuals(&style.visuals);
    let error_stroke = Stroke::new(ERROR_UNDERLINE_WIDTH, style.visuals.error_fg_color);
    let current_background = style
        .visuals
        .extreme_bg_color
        .lerp_to_gamma(style.visuals.text_color(), CURRENT_STATEMENT_TINT);

    let match_background = style
        .visuals
        .selection
        .bg_fill
        .gamma_multiply(MATCH_STRENGTH);
    let current_match_background = style.visuals.selection.bg_fill;

    let mut job = LayoutJob::default();
    for span in covering(highlighter.spans(text), text.len()) {
        let span_format = format_for(span.role, &font, &colors);
        // A match can start or end inside a span (part of a word), so the span is split where
        // matches begin and end.
        for piece in pieces(&span.range, &marks.matches) {
            let mut format = span_format.clone();
            if marks
                .error
                .as_ref()
                .is_some_and(|error| overlaps(error, &piece))
            {
                format.underline = error_stroke;
            }
            let in_block = marks
                .code_blocks
                .iter()
                .any(|block| overlaps(block, &piece));
            if in_block
                || marks
                    .current_statement
                    .as_ref()
                    .is_some_and(|current| overlaps(current, &piece))
            {
                format.background = current_background;
            }
            if marks.matches.iter().any(|found| overlaps(found, &piece)) {
                format.background = match_background;
            }
            if marks
                .current_match
                .as_ref()
                .is_some_and(|current| overlaps(current, &piece))
            {
                format.background = current_match_background;
            }
            job.append(&text[piece], 0.0, format);
        }
    }
    job
}

/// `spans` with the text they leave out (and any that overlap or run past the end) filled in
/// as plain, so the layout holds every byte of the text exactly once.
fn covering(spans: Vec<Span>, len: usize) -> Vec<Span> {
    let plain = |range| Span {
        range,
        role: SyntaxRole::Plain,
    };
    let mut covered = Vec::with_capacity(spans.len());
    let mut at = 0;
    for span in spans {
        if span.range.start < at || span.range.end > len || span.range.is_empty() {
            continue;
        }
        if span.range.start > at {
            covered.push(plain(at..span.range.start));
        }
        at = span.range.end;
        covered.push(span);
    }
    if at < len {
        covered.push(plain(at..len));
    }
    covered
}

/// `range` cut where any of `matches` starts or ends inside it.
fn pieces(range: &Range<usize>, matches: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut cuts: Vec<usize> = matches
        .iter()
        .flat_map(|found| [found.start, found.end])
        .filter(|cut| range.start < *cut && *cut < range.end)
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut pieces = Vec::with_capacity(cuts.len() + 1);
    let mut start = range.start;
    for cut in cuts {
        pieces.push(start..cut);
        start = cut;
    }
    pieces.push(start..range.end);
    pieces
}

fn format_for(role: SyntaxRole, font: &FontId, colors: &SyntaxColors) -> TextFormat {
    let color = match role {
        SyntaxRole::Keyword => colors.keyword,
        SyntaxRole::String => colors.string,
        SyntaxRole::Number => colors.number,
        SyntaxRole::Comment => colors.comment,
        SyntaxRole::Identifier => colors.identifier,
        SyntaxRole::Plain => colors.plain,
    };
    let mut format = TextFormat::simple(font.clone(), color);
    format.italics = role == SyntaxRole::Comment;
    format
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// The text to underline for an error reported at byte `offset`: the token there, or one
/// character if the offset falls on whitespace or past the end.
pub fn error_marker(text: &str, language: QueryLanguage, offset: usize) -> Range<usize> {
    language
        .tokens(text)
        .into_iter()
        .find(|token| token.range.contains(&offset) && token.kind != TokenKind::Whitespace)
        .map(|token| token.range)
        .unwrap_or_else(|| {
            let start = offset.min(text.len());
            let end = text[start..]
                .chars()
                .next()
                .map_or(start, |c| start + c.len_utf8());
            start..end
        })
}

#[cfg(test)]
mod tests {
    use dbrover_sql::Dialect;

    use super::*;

    #[test]
    fn colours_each_languages_own_words() {
        let colors_of = |text: &str, language: QueryLanguage| -> Vec<(String, egui::Color32)> {
            layout_job(text, &language, &Marks::default(), &Style::default())
                .sections
                .iter()
                .map(|section| {
                    let range = section.byte_range.start.0..section.byte_range.end.0;
                    (text[range].to_owned(), section.format.color)
                })
                .filter(|(fragment, _)| !fragment.trim().is_empty())
                .collect()
        };
        let colors = SyntaxColors::for_visuals(&Style::default().visuals);

        let test = colors_of("get \"k\" 42 # why", QueryLanguage::Test);
        assert_eq!(
            test,
            [
                ("get".to_owned(), colors.keyword),
                ("\"k\"".to_owned(), colors.string),
                ("42".to_owned(), colors.number),
                ("# why".to_owned(), colors.comment),
            ]
        );
        // `select` is a SQL keyword, not one of the test language's.
        let sql_word = colors_of("select", QueryLanguage::Test);
        assert_eq!(sql_word, [("select".to_owned(), colors.plain)]);
    }

    #[test]
    fn shades_the_current_statement_and_underlines_the_error() {
        let text = "select 1; select bad";
        let marks = Marks {
            error: Some(17..20),
            current_statement: Some(10..20),
            ..Marks::default()
        };

        let job = layout_job(
            text,
            &QueryLanguage::Sql(Dialect::Postgres),
            &marks,
            &Style::default(),
        );

        let shaded: String = job
            .sections
            .iter()
            .filter(|section| section.format.background != egui::Color32::TRANSPARENT)
            .map(|section| &text[section.byte_range.start.0..section.byte_range.end.0])
            .collect();
        let underlined: String = job
            .sections
            .iter()
            .filter(|section| section.format.underline != Stroke::NONE)
            .map(|section| &text[section.byte_range.start.0..section.byte_range.end.0])
            .collect();
        assert_eq!(shaded, "select bad");
        assert_eq!(underlined, "bad");
    }

    #[test]
    fn highlights_matches_inside_tokens_and_the_current_one_more() {
        let text = "select order_id from orders";
        let marks = Marks {
            matches: vec![7..12, 21..26],
            current_match: Some(21..26),
            ..Marks::default()
        };
        let style = Style::default();

        let job = layout_job(text, &QueryLanguage::Sql(Dialect::Postgres), &marks, &style);

        let section_text = |section: &egui::text::LayoutSection| {
            &text[section.byte_range.start.0..section.byte_range.end.0]
        };
        let with_background = |wanted: egui::Color32| -> Vec<&str> {
            job.sections
                .iter()
                .filter(|section| section.format.background == wanted)
                .map(section_text)
                .collect()
        };
        let selection = style.visuals.selection.bg_fill;
        assert_eq!(
            with_background(selection.gamma_multiply(MATCH_STRENGTH)),
            ["order"]
        );
        assert_eq!(with_background(selection), ["order"]);
        let all: String = job.sections.iter().map(section_text).collect();
        assert_eq!(all, text);
    }

    #[test]
    fn fills_what_the_spans_leave_out_as_plain() {
        let span = |range: Range<usize>, role| Span { range, role };
        let keyword = SyntaxRole::Keyword;
        let plain = SyntaxRole::Plain;
        let cases = [
            (vec![], vec![span(0..6, plain)]),
            (
                vec![span(1..3, keyword)],
                vec![span(0..1, plain), span(1..3, keyword), span(3..6, plain)],
            ),
            (
                vec![
                    span(0..3, keyword),
                    span(2..4, keyword),
                    span(4..9, keyword),
                ],
                vec![span(0..3, keyword), span(3..6, plain)],
            ),
            (vec![span(2..2, keyword)], vec![span(0..6, plain)]),
        ];
        for (spans, expected) in cases {
            assert_eq!(covering(spans.clone(), 6), expected, "{spans:?}");
        }
    }

    #[test]
    fn marks_the_token_at_the_error() {
        let text = "SELECT * FROM missing_table";

        assert_eq!(
            &text[error_marker(text, QueryLanguage::Sql(Dialect::Postgres), 14)],
            "missing_table"
        );
        assert_eq!(
            error_marker(text, QueryLanguage::Sql(Dialect::Postgres), 99),
            text.len()..text.len()
        );
    }
}
