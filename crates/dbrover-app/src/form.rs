//! The shared look of forms, from dialogs to the Settings tab: fields grouped into captioned
//! cards of rows, a few choices offered as a segmented control, options switched with a
//! toggle, a header naming what the form is for, and one primary action at the end.

use egui::{
    Align, Align2, Button, CornerRadius, Frame, Layout, Margin, Response, RichText, Sense, Stroke,
    Ui, WidgetInfo, WidgetType, vec2,
};

use crate::fonts;
use crate::theme::Palette;

const CARD_RADIUS: u8 = 8;
const CARD_MARGIN: Margin = Margin::symmetric(12, 6);
/// Space between a card's rows, either side of the line that separates them.
const ROW_GAP: f32 = 4.0;
/// Space between one section's card and the next section's caption.
const SECTION_GAP: f32 = 12.0;
const CAPTION_SIZE: f32 = 11.5;
const TITLE_SIZE: f32 = 15.0;
const HEADER_ICON_SIZE: f32 = 17.0;
/// The rounded square behind a header's icon.
const HEADER_BADGE_SIZE: f32 = 32.0;
/// How strongly the header's badge is tinted with the primary colour.
const HEADER_BADGE_TINT: f32 = 0.16;
/// Between a segmented control's rim and its segments.
const SEGMENT_MARGIN: i8 = 2;
/// A toggle's track, as a multiple of the row height: about the proportions of macOS's switch.
const TOGGLE_WIDTH: f32 = 1.75;

/// The form's title beside an icon in a tinted badge, with a line about what it acts on.
pub struct Header<'a> {
    pub glyph: &'a str,
    pub title: &'a str,
    pub subtitle: &'a str,
}

impl Header<'_> {
    /// Draws the header, with `trailing` (a choice that shapes the whole form, say) at its
    /// right.
    pub fn show(self, ui: &mut Ui, trailing: impl FnOnce(&mut Ui)) {
        let palette = Palette::for_visuals(ui.visuals());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let badge_size = vec2(HEADER_BADGE_SIZE, HEADER_BADGE_SIZE);
            let (badge, _) = ui.allocate_exact_size(badge_size, Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(
                badge,
                CARD_RADIUS,
                palette.tint(palette.primary, HEADER_BADGE_TINT),
            );
            painter.text(
                badge.center(),
                Align2::CENTER_CENTER,
                self.glyph,
                fonts::icon(HEADER_ICON_SIZE),
                palette.primary,
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(RichText::new(self.title).font(fonts::semibold(TITLE_SIZE)));
                ui.label(RichText::new(self.subtitle).color(palette.text_weak));
            });
            let rest = vec2(ui.available_width(), HEADER_BADGE_SIZE);
            ui.allocate_ui_with_layout(rest, Layout::right_to_left(Align::Center), trailing);
        });
        ui.add_space(SECTION_GAP);
    }
}

/// A captioned group of the form: the caption, then whatever `add_contents` adds, usually a
/// [`card`].
pub fn section<R>(ui: &mut Ui, caption: &str, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let palette = Palette::for_visuals(ui.visuals());
    ui.label(
        RichText::new(caption)
            .font(fonts::semibold(CAPTION_SIZE))
            .color(palette.text_weak),
    );
    let inner = add_contents(ui);
    ui.add_space(SECTION_GAP);
    inner
}

/// A card of rows, a step up from the panel behind it, with a line between rows.
pub fn card<R>(ui: &mut Ui, add_rows: impl FnOnce(&mut Rows<'_>) -> R) -> R {
    let palette = Palette::for_visuals(ui.visuals());
    Frame::new()
        .fill(palette.raised())
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(CARD_MARGIN)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Fields share the card's fill in the light theme, so at rest they get the outline
            // that hovering gives them anyway; buttons and menus look the better for it too.
            ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::new(1.0, palette.border);
            let mut rows = Rows { ui, drawn: 0 };
            let inner = add_rows(&mut rows);
            // The same space under the last row as above the first.
            if rows.drawn > 0 {
                rows.ui.add_space(ROW_GAP / 2.0);
            }
            inner
        })
        .inner
}

/// The rows of a [`card`].
pub struct Rows<'a> {
    ui: &'a mut Ui,
    drawn: usize,
}

impl Rows<'_> {
    /// A setting or option: `title` at the left, `control` at the right, and `description`,
    /// if any, below them in weak text.
    pub fn row<R>(
        &mut self,
        title: &str,
        description: Option<&str>,
        control: impl FnOnce(&mut Ui) -> R,
    ) -> R {
        self.separate();
        let ui = &mut *self.ui;
        let height = ui.spacing().interact_size.y;
        let inner = ui
            .horizontal(|ui| {
                ui.label(title);
                // As tall as a control: a right-to-left layout would otherwise take every
                // point of height the card is allowed.
                let size = vec2(ui.available_width(), height);
                ui.allocate_ui_with_layout(size, Layout::right_to_left(Align::Center), control)
                    .inner
            })
            .inner;
        if let Some(description) = description {
            description_label(ui, description);
        }
        inner
    }

    /// A row of its own making, the card's whole width: a list, a preview, a note.
    pub fn full<R>(&mut self, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
        self.separate();
        add_contents(self.ui)
    }

    fn separate(&mut self) {
        if self.drawn > 0 {
            self.ui
                .add(egui::Separator::default().spacing(ROW_GAP * 2.0));
        } else {
            self.ui.add_space(ROW_GAP / 2.0);
        }
        self.drawn += 1;
    }
}

/// Weak text that wraps at the width it is given, for what a setting does.
pub fn description_label(ui: &mut Ui, text: &str) {
    let palette = Palette::for_visuals(ui.visuals());
    ui.add(egui::Label::new(RichText::new(text).small().color(palette.text_weak)).wrap());
}

/// One choice of a [`segmented`] control.
pub struct Segment<'a, T> {
    value: T,
    label: &'a str,
    hint: Option<&'a str>,
}

impl<'a, T> Segment<'a, T> {
    pub fn new(value: T, label: &'a str) -> Self {
        Self {
            value,
            label,
            hint: None,
        }
    }

    /// A tooltip for a choice whose label can't say it all.
    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = Some(hint);
        self
    }
}

/// A few mutually exclusive choices side by side in one rounded control, the chosen one
/// highlighted. Screen readers (and UI tests) see radio buttons, by label. Returns whether the
/// choice changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    current: &mut T,
    segments: &[Segment<'_, T>],
) -> bool {
    let palette = Palette::for_visuals(ui.visuals());
    let mut changed = false;
    Frame::new()
        .fill(palette.control)
        .corner_radius(CornerRadius::same(CARD_RADIUS - 2))
        .inner_margin(Margin::same(SEGMENT_MARGIN))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let spacing = ui.spacing_mut();
                spacing.item_spacing.x = 2.0;
                spacing.button_padding.y = 1.0;
                // As tall as any other control, margin included, so it lines up with the
                // title beside it.
                spacing.interact_size.y -= f32::from(SEGMENT_MARGIN) * 2.0;
                // A row laid out right to left (a card row's control) would list the
                // segments backwards.
                let in_order: Box<dyn Iterator<Item = &Segment<'_, T>>> =
                    if ui.layout().prefer_right_to_left() {
                        Box::new(segments.iter().rev())
                    } else {
                        Box::new(segments.iter())
                    };
                for segment in in_order {
                    let selected = *current == segment.value;
                    let mut response = ui.add(Button::selectable(selected, segment.label));
                    if let Some(hint) = segment.hint {
                        response = response.on_hover_text(hint);
                    }
                    response.widget_info(|| {
                        WidgetInfo::selected(
                            WidgetType::RadioButton,
                            ui.is_enabled(),
                            selected,
                            segment.label,
                        )
                    });
                    if response.clicked() && !selected {
                        *current = segment.value;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// An on/off switch, named `label` for screen readers and UI tests, which see a checkbox.
/// Unlike egui's own demo switch it doesn't animate: an animation keeps asking for repaints,
/// which UI tests stepping the clock would have to wait out.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let palette = Palette::for_visuals(ui.visuals());
    let height = ui.spacing().interact_size.y * 0.8;
    let size = vec2(height * TOGGLE_WIDTH, height);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *on, label));
    if ui.is_rect_visible(rect) {
        let accent = ui.visuals().selection.stroke.color;
        let (track, outline) = if *on {
            (accent, Stroke::NONE)
        } else {
            (palette.control_hover, Stroke::new(1.0, palette.border))
        };
        let track = if enabled {
            track
        } else {
            track.gamma_multiply(0.5)
        };
        let knob = if palette.dark {
            palette.text
        } else {
            palette.field
        };
        let radius = rect.height() / 2.0;
        let painter = ui.painter();
        painter.rect(rect, radius, track, outline, egui::StrokeKind::Inside);
        let center_x = if *on {
            rect.right() - radius
        } else {
            rect.left() + radius
        };
        painter.circle_filled(egui::pos2(center_x, rect.center().y), radius - 2.5, knob);
    }
    response
}

/// The one main action of a form, filled with the primary colour.
pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let palette = Palette::for_visuals(ui.visuals());
    let text_color = if enabled {
        palette.on_primary
    } else {
        palette.text_weak
    };
    let fill = if enabled {
        palette.primary
    } else {
        palette.control
    };
    ui.add_enabled(
        enabled,
        Button::new(RichText::new(text).color(text_color)).fill(fill),
    )
}

/// The row of actions along the bottom of a dialog, laid out right to left so the primary
/// action added first sits in the corner, under a line across the dialog.
pub fn footer<R>(ui: &mut Ui, add_actions: impl FnOnce(&mut Ui) -> R) -> R {
    ui.separator();
    ui.add_space(2.0);
    // As tall as a button: a right-to-left layout would otherwise take every point of height
    // a modal allows, and push the dialog off the screen.
    let row = vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(row, Layout::right_to_left(Align::Center), add_actions)
        .inner
}
