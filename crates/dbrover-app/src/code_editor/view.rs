//! Draws a code editor: the text with its highlighting, a gutter of line numbers, a wavy line
//! under each problem with its message on hover, and the cursor moves and undo points asked
//! for from outside the editor.

use std::ops::Range;

use egui::text::{CCursor, CCursorRange};
use egui::text_edit::{TextEditOutput, TextEditState};
use egui::{Id, Margin, TextEdit, Ui, vec2};

use super::CodeEditor;
use super::highlight::{self, Highlighter, Marks};
use crate::theme::Palette;

/// How bad a problem is, which decides its squiggle's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    /// Advice rather than a mistake, such as a hint from a report's runs.
    Hint,
}

/// A problem the editor underlines: where it is, how bad, and what it says on hover.
pub trait EditorProblem {
    /// Byte range in the editor's text.
    fn range(&self) -> Range<usize>;
    fn severity(&self) -> Severity;
    fn message(&self) -> &str;
    /// A quieter line under the message, such as how to switch its rule off.
    fn note(&self) -> Option<String>;
}

/// What an editor draws with besides its state.
pub struct EditorView<'a, P> {
    /// The `TextEdit`'s id, which keeps its cursor and undo history in egui's memory, and which
    /// the owner checks for focus before taking shortcuts.
    pub id: Id,
    /// Shown while the text is empty.
    pub hint_text: &'a str,
    pub highlighter: &'a dyn Highlighter,
    pub marks: &'a Marks,
    pub problems: &'a [P],
}

/// Draws the editor, filling the height `ui` has left, and keeps the editor's selection up to
/// date. Meant for inside the owner's scroll area; the owner reads the output for what it does
/// on an edit or around the cursor (a completion list).
pub fn edit<P: EditorProblem>(
    ui: &mut Ui,
    editor: &mut CodeEditor,
    view: &EditorView<'_, P>,
) -> TextEditOutput {
    let reveal = move_cursor(ui, editor, view.id);
    let mut layouter = highlight::layouter(view.highlighter, view.marks);
    let gutter_width = gutter_width(ui, &editor.text);
    let output = TextEdit::multiline(&mut editor.text)
        .id(view.id)
        .code_editor()
        .hint_text(view.hint_text)
        // Fills the height given by `min_size`; one row keeps it from overflowing that height
        // when the editor is short.
        .desired_rows(1)
        .desired_width(f32::INFINITY)
        .min_size(vec2(0.0, ui.available_height()))
        .margin(Margin {
            left: gutter_width,
            ..TEXT_MARGIN
        })
        .layouter(&mut layouter)
        .show(ui);
    if let Some(cursor) = reveal {
        let rect = output.galley.pos_from_cursor(cursor.primary);
        let rect = rect.translate(output.galley_pos.to_vec2());
        ui.scroll_to_rect(rect, Some(egui::Align::Center));
    }
    line_numbers(ui, &output);
    editor.problem_rects = squiggles(ui, &editor.text, view.problems, &output);
    problem_tooltip(ui, &editor.problem_rects, view.problems, &output);
    if let Some(cursor) = output.cursor_range {
        editor.selection = byte_range(&editor.text, &cursor);
    }
    output
}

/// Applies a cursor move or selection requested from outside the editor, and returns it so
/// the editor scrolls to it. A cursor move also focuses the editor; a selection (a match the
/// find bar went to) leaves the focus where it is. After the text was replaced (a format,
/// a reload from its file, a replacement), also records the text before and after as undo
/// points, so one undo restores it; egui would otherwise only record a point once the text
/// had stayed the same for a second.
fn move_cursor(ui: &Ui, editor: &mut CodeEditor, editor_id: Id) -> Option<CCursorRange> {
    let focus = editor.pending_cursor.is_some();
    let range = match (
        editor.pending_cursor.take(),
        editor.pending_selection.take(),
    ) {
        (_, Some(range)) => range,
        (Some(offset), None) => offset..offset,
        (None, None) => return None,
    };
    let char_at = |offset: usize| CCursor::new(char_index(&editor.text, offset));
    let cursor = CCursorRange::two(char_at(range.start), char_at(range.end));
    let mut state = TextEditState::load(ui.ctx(), editor_id).unwrap_or_default();
    if let Some(before) = editor.replaced_text.take() {
        let mut undoer = state.undoer();
        let cursor_before = state.cursor.char_range().unwrap_or(cursor);
        undoer.add_undo(&(cursor_before, before));
        undoer.add_undo(&(cursor, editor.text.clone()));
        state.set_undoer(undoer);
    }
    state.cursor.set_char_range(Some(cursor));
    state.store(ui.ctx(), editor_id);
    if focus {
        ui.memory_mut(|memory| memory.request_focus(editor_id));
    }
    Some(cursor)
}

/// How far a squiggle's wave rises and falls, and the length of one rise or fall.
const SQUIGGLE_AMPLITUDE: f32 = 1.5;
const SQUIGGLE_STEP: f32 = 2.5;

/// Underlines each problem with a wave, red for errors and amber for warnings, and each hint
/// with dots in the accent colour. Returns where, with each problem's index, so hovering can
/// show the message.
fn squiggles<P: EditorProblem>(
    ui: &Ui,
    text: &str,
    problems: &[P],
    output: &TextEditOutput,
) -> Vec<(egui::Rect, usize)> {
    let palette = &Palette::for_visuals(ui.visuals());
    let origin = output.galley_pos;
    let mut rects = Vec::new();
    for (index, problem) in problems.iter().enumerate() {
        let severity = problem.severity();
        let color = match severity {
            Severity::Error => palette.error,
            Severity::Warning => palette.warning,
            Severity::Hint => palette.primary,
        };
        let range = problem.range();
        let start = char_index(text, range.start);
        let end = char_index(text, range.end).max(start + 1);
        let mut row_start = 0;
        for row in &output.galley.rows {
            let row_end = row_start + row.char_count_including_newline().0;
            let (from, to) = (start.max(row_start), end.min(row_end));
            if from < to {
                let column =
                    |char: usize| egui::text::CharIndex((char - row_start).min(row.glyphs.len()));
                let left = origin.x + row.pos.x + row.x_offset(column(from));
                let right = origin.x + row.pos.x + row.x_offset(column(to));
                let rect = egui::Rect::from_min_max(
                    egui::pos2(left, origin.y + row.min_y()),
                    egui::pos2(
                        right.max(left + 2.0 * SQUIGGLE_STEP),
                        origin.y + row.max_y(),
                    ),
                );
                ui.painter().add(match severity {
                    Severity::Hint => dots(rect, color),
                    Severity::Error | Severity::Warning => wave(rect, color),
                });
                rects.push((rect, index));
            }
            row_start = row_end;
        }
    }
    rects
}

fn wave(rect: egui::Rect, color: egui::Color32) -> egui::Shape {
    let baseline = rect.bottom() - SQUIGGLE_AMPLITUDE;
    let mut points = Vec::new();
    let mut x = rect.left();
    let mut up = true;
    while x <= rect.right() {
        let offset = if up {
            -SQUIGGLE_AMPLITUDE
        } else {
            SQUIGGLE_AMPLITUDE
        };
        points.push(egui::pos2(x, baseline + offset));
        x += SQUIGGLE_STEP;
        up = !up;
    }
    egui::Shape::line(points, egui::Stroke::new(1.0, color))
}

/// A dotted line along the bottom of `rect`, quieter than a wave.
fn dots(rect: egui::Rect, color: egui::Color32) -> egui::Shape {
    let y = rect.bottom() - SQUIGGLE_AMPLITUDE;
    let mut dots = Vec::new();
    let mut x = rect.left();
    while x <= rect.right() {
        dots.push(egui::Shape::circle_filled(egui::pos2(x, y), 1.0, color));
        x += 2.0 * SQUIGGLE_STEP;
    }
    egui::Shape::Vec(dots)
}

/// Shows the message of the problem under the pointer, and its note.
fn problem_tooltip<P: EditorProblem>(
    ui: &Ui,
    rects: &[(egui::Rect, usize)],
    problems: &[P],
    output: &TextEditOutput,
) {
    if !output.response.hovered() {
        return;
    }
    let Some(pointer) = ui.ctx().pointer_hover_pos() else {
        return;
    };
    let Some(problem) = rects
        .iter()
        .find(|(rect, _)| rect.contains(pointer))
        .and_then(|(_, index)| problems.get(*index))
    else {
        return;
    };
    output
        .response
        .response
        .clone()
        .on_hover_ui_at_pointer(|ui| {
            ui.label(problem.message());
            if let Some(note) = problem.note() {
                ui.weak(note);
            }
        });
}

/// The character index of byte `offset`, as the editor's galley counts.
fn char_index(text: &str, offset: usize) -> usize {
    text.get(..offset).unwrap_or(text).chars().count()
}

/// The editor's own margin around the text; the line numbers widen its left side.
const TEXT_MARGIN: Margin = Margin::symmetric(4, 2);
/// Space between the line numbers and the text.
const GUTTER_GAP: f32 = 12.0;

/// The editor's left margin: room for the largest line number, so the text doesn't shift
/// until a line count needs another digit.
fn gutter_width(ui: &Ui, text: &str) -> i8 {
    let lines = text.lines().count().max(1) + usize::from(text.ends_with('\n'));
    let digits = lines.to_string().len();
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let digit_width = ui.fonts_mut(|fonts| fonts.glyph_width(&font, '0'));
    let width = f32::from(TEXT_MARGIN.left) + digits as f32 * digit_width + GUTTER_GAP;
    // Margins are whole points in an `i8`; 127 points hold over a dozen digits.
    width.ceil().min(f32::from(i8::MAX)) as i8
}

/// Numbers each line of the editor's text in the gutter. A line that wraps is numbered on
/// its first row only.
fn line_numbers(ui: &Ui, output: &TextEditOutput) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let color = ui.visuals().weak_text_color();
    let right_edge = output.galley_pos.x - GUTTER_GAP;
    let rows = &output.galley.rows;
    let ends: Vec<bool> = rows.iter().map(|row| row.ends_with_newline).collect();
    for (line, row_index) in line_starts(&ends).into_iter().enumerate() {
        let y = output.galley_pos.y + rows[row_index].rect().center().y;
        ui.painter().text(
            egui::pos2(right_edge, y),
            egui::Align2::RIGHT_CENTER,
            (line + 1).to_string(),
            font.clone(),
            color,
        );
    }
}

/// The indices of the rows that start a line, given whether each row ends with a line break
/// (rather than wrapping onto the next).
fn line_starts(ends_with_newline: &[bool]) -> Vec<usize> {
    (0..ends_with_newline.len())
        .filter(|&row| row == 0 || ends_with_newline[row - 1])
        .collect()
}

/// Converts the editor's character-index cursor into byte offsets into `text`.
fn byte_range(text: &str, cursor: &CCursorRange) -> Range<usize> {
    let byte = |char_index: usize| {
        text.char_indices()
            .nth(char_index)
            .map_or(text.len(), |(byte, _)| byte)
    };
    byte(cursor.secondary.index.0)..byte(cursor.primary.index.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_only_the_first_row_of_a_wrapped_line() {
        // Line 1 wraps onto a second row; lines 2 and 3 fit on one row each.
        assert_eq!(line_starts(&[false, true, true, false]), [0, 2, 3]);
        assert_eq!(line_starts(&[false]), [0]);
        assert!(line_starts(&[]).is_empty());
    }
}
