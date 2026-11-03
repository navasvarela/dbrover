//! The frame every popup shares: a modal dialog or a floating window of one of four widths,
//! opened in the middle of the app's window. What goes inside is `form`'s: a
//! [`Header`](crate::form::Header), cards, and a [`footer`](crate::form::footer) of actions.

use egui::{Align2, Frame, Id, Modal, ModalResponse, Panel, Ui, Vec2, Window};

use crate::form;

/// A list or preview inside a dialog: data sources to import, tables to draw, a file's rows.
pub const LIST_HEIGHT: f32 = 280.0;
/// A list that is the dialog's whole point, such as the data sources a file holds.
pub const TALL_LIST_HEIGHT: f32 = 380.0;

/// From the app window's top right corner to a floating window's: below the title bar or menu
/// bar (28 points at most) and the dock's tab bar, with the window margin's room at the right.
const WINDOW_INSET: Vec2 = Vec2::new(-16.0, 64.0);

/// How wide a dialog or a floating window is, by what it holds, so popups of a kind are the
/// same width and none picks its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogSize {
    /// A question and a few lines about it: delete, roll back, run anyway, a notice.
    Confirm,
    /// A few fields: a password, a name, a statement's parameters.
    Form,
    /// A list or a preview, or a long form in cards: the data source dialog, export.
    Wide,
    /// A big form with a preview beside its options: CSV import.
    Large,
}

impl DialogSize {
    /// Today's widths rounded into four steps. `Form` is wide enough for a 280-point field and
    /// its label in a card.
    fn width(self) -> f32 {
        match self {
            Self::Confirm => 420.0,
            Self::Form => 480.0,
            Self::Wide => 560.0,
            Self::Large => 680.0,
        }
    }
}

/// A modal dialog, shown with [`Dialog::show`].
pub struct Dialog {
    modal: Modal,
    size: DialogSize,
}

/// A modal dialog `size` wide, with a window's margin rather than egui's default, a popup's,
/// which is sized for menus and leaves a dialog's text and buttons against its edge.
pub fn modal(ctx: &egui::Context, id: Id, size: DialogSize) -> Dialog {
    let style = ctx.global_style();
    let frame = Frame::popup(&style).inner_margin(style.spacing.window_margin);
    Dialog {
        modal: Modal::new(id).frame(frame),
        size,
    }
}

impl Dialog {
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        add_contents: impl FnOnce(&mut Ui) -> R,
    ) -> ModalResponse<R> {
        let width = self.size.width();
        self.modal.show(ctx, |ui| {
            ui.set_width(width);
            add_contents(ui)
        })
    }
}

/// A floating, non-modal window titled `title`, `size` wide at first, opened at the right of
/// the app's window: it is read beside the work, so it covers the end of the editor's lines
/// rather than the middle of the grid. egui would open it at (16, 16), over the menu bar on
/// Linux and Windows.
pub fn window<'open>(
    ctx: &egui::Context,
    title: impl Into<egui::WidgetText>,
    id: Id,
    size: DialogSize,
) -> Window<'open> {
    Window::new(title)
        .id(id)
        .default_width(size.width())
        .pivot(Align2::RIGHT_TOP)
        .default_pos(ctx.content_rect().right_top() + WINDOW_INSET)
}

/// A floating window's row of actions, pinned to its bottom edge so what is above it can
/// scroll and fill the rest. `id` tells it from other windows' footers.
pub fn window_footer<R>(ui: &mut Ui, id: Id, add_actions: impl FnOnce(&mut Ui) -> R) -> R {
    Panel::bottom(id)
        .frame(Frame::NONE)
        .show_separator_line(false)
        .resizable(false)
        .show(ui, |ui| form::footer(ui, add_actions))
        .inner
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn a_dialog_is_as_wide_as_its_size_whatever_it_holds() {
        for size in [
            DialogSize::Confirm,
            DialogSize::Form,
            DialogSize::Wide,
            DialogSize::Large,
        ] {
            let id = Id::new("sized");
            let mut harness = Harness::builder()
                .with_size(egui::vec2(1000.0, 600.0))
                .build_ui(|ui| {
                    modal(ui.ctx(), id, size).show(ui.ctx(), |ui| ui.label("Short"));
                });
            harness.run();

            let frame = harness.ctx.memory(|memory| memory.area_rect(id));
            let style = harness.ctx.global_style();
            let edges =
                style.spacing.window_margin.sum().x + 2.0 * style.visuals.window_stroke.width;
            let width = frame.map(|frame| frame.width() - edges);
            assert_eq!(width, Some(size.width()), "{size:?}");
        }
    }

    #[test]
    fn the_footer_puts_the_primary_action_in_the_corner_and_the_leading_one_apart() {
        let mut harness = Harness::new_ui(|ui| {
            form::footer_with_leading(
                ui,
                |ui| form::discard_button(ui, "Don't save"),
                |ui| {
                    form::primary_button(ui, "Save", true);
                    let _ = ui.button("Cancel");
                },
            );
        });
        harness.run();

        let width = harness.ctx.content_rect().width();
        let save = harness.get_by_label("Save").rect();
        let cancel = harness.get_by_label("Cancel").rect();
        let discard = harness.get_by_label("Don't save").rect();
        assert!(save.right() > width * 0.9, "Save is in the corner");
        assert!(cancel.right() < save.left(), "Cancel is beside Save");
        assert!(discard.left() < width * 0.1, "Don't save is at the left");
    }
}
