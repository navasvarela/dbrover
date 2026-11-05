//! The code editor a console types its statements in, and a report its file
//! (`docs/report-editor.md`, RP8b): the text and its selection, moving the cursor from outside
//! the editor, replacing the text as one undo step, the find bar, the line numbers and the
//! squiggles under problems. What the text means, and which problems it has, is the owner's.

mod find;
pub mod find_bar;
pub mod highlight;
mod view;

use std::ops::Range;

pub use find::{FindBar, Found};
pub use view::{EditorProblem, EditorView, Severity, edit};

/// An editor's text, and what the editor needs between frames besides egui's own state.
#[derive(Debug, Default)]
pub struct CodeEditor {
    pub text: String,
    /// The cursor or selection as byte offsets into `text`, in either order.
    pub selection: Range<usize>,
    /// A byte offset to move the cursor to on the next frame, after the text was changed from
    /// outside the editor. Moving it also focuses the editor.
    pending_cursor: Option<usize>,
    /// A range to select and scroll to on the next frame, leaving the keyboard focus where it
    /// is: a match the find bar went to.
    pending_selection: Option<Range<usize>>,
    /// The text before it was replaced from outside the editor (formatted, reloaded from its
    /// file, a replacement), for the editor to record as an undo point next to the new text,
    /// so one undo takes the whole change back.
    replaced_text: Option<String>,
    find: FindBar,
    /// Where each problem's squiggle was drawn last frame, with the problem's index, for
    /// hovering.
    problem_rects: Vec<(egui::Rect, usize)>,
}

impl CodeEditor {
    pub fn new(text: String) -> Self {
        Self {
            text,
            ..Self::default()
        }
    }

    /// The selection's start and end, smaller first.
    pub fn ordered_selection(&self) -> (usize, usize) {
        ordered(&self.selection)
    }

    /// Puts the cursor at byte `offset` and focuses the editor.
    pub fn put_cursor(&mut self, offset: usize) {
        self.selection = offset..offset;
        self.pending_cursor = Some(offset);
    }

    /// Selects `range` and focuses the editor.
    pub fn select(&mut self, range: Range<usize>) {
        self.selection = range.clone();
        self.pending_cursor = Some(range.end);
        self.pending_selection = Some(range);
    }

    /// Selects `range` and scrolls to it, leaving the keyboard focus where it is.
    pub fn select_without_focus(&mut self, range: Range<usize>) {
        self.selection = range.clone();
        self.pending_selection = Some(range);
    }

    /// Replaces the whole text from outside the editor, as one undo step, with the cursor at
    /// byte `cursor` of the new text.
    pub fn replace_text(&mut self, text: String, cursor: usize) {
        self.replaced_text = Some(std::mem::replace(&mut self.text, text));
        self.put_cursor(cursor);
    }

    /// Replaces the whole text from outside the editor, as one undo step, keeping the cursor
    /// where it was if the new text is long enough.
    pub fn replace_keeping_cursor(&mut self, text: String) {
        let (_, end) = self.ordered_selection();
        let mut cursor = end.min(text.len());
        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        self.replace_text(text, cursor);
    }

    /// Where each problem's squiggle was drawn last frame, for tests to hover over.
    #[cfg(test)]
    pub fn squiggle_rects(&self) -> impl Iterator<Item = egui::Rect> + '_ {
        self.problem_rects.iter().map(|(rect, _)| *rect)
    }

    #[cfg(test)]
    pub fn pending_cursor(&self) -> Option<usize> {
        self.pending_cursor
    }

    #[cfg(test)]
    pub fn replaced_text(&self) -> Option<&str> {
        self.replaced_text.as_deref()
    }
}

fn ordered(range: &Range<usize>) -> (usize, usize) {
    (range.start.min(range.end), range.start.max(range.end))
}
