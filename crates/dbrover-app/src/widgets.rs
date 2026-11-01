//! Small building blocks the panels share: icon buttons, menus and menu items with icons,
//! toolbars and count badges.

use egui::containers::menu::{self, MenuButton, MenuConfig};
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Button, Color32, Frame, Id, InnerResponse, IntoAtoms, Modal, Popup, Response, RichText,
    TextStyle, Ui, Vec2, WidgetInfo, WidgetType,
};

use crate::{fonts, theme};

/// Icon size in toolbars, a little larger than text so thin glyphs stay legible.
const TOOLBAR_ICON_SIZE: f32 = 15.0;
/// Between a toolbar's buttons: their hover highlights nearly touch, but don't merge.
const TOOLBAR_SPACING: f32 = 3.0;
/// The narrowest a menu gets, so a menu of two short entries doesn't look like a tooltip.
const MENU_MIN_WIDTH: f32 = 160.0;

/// A frameless button that shows only an icon. Its label is the tooltip and the accessible
/// name, so screen readers (and UI tests) find it by what it does rather than by its glyph.
pub struct IconButton<'a> {
    glyph: &'a str,
    label: &'a str,
    tooltip: Option<String>,
    color: Option<Color32>,
    enabled: bool,
}

impl<'a> IconButton<'a> {
    pub fn new(glyph: &'a str, label: &'a str) -> Self {
        Self {
            glyph,
            label,
            tooltip: None,
            color: None,
            enabled: true,
        }
    }

    /// Colours the icon; monochrome (the text colour) without it.
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// A longer tooltip than the label, e.g. one that mentions the shortcut.
    pub fn tooltip(mut self, tooltip: String) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let mut text = glyph(self.glyph).size(TOOLBAR_ICON_SIZE);
        if let Some(color) = self.color {
            text = text.color(color);
        }
        let tooltip = self.tooltip.as_deref().unwrap_or(self.label);
        // Square, as tall as a control, so its hover highlight is a tile around the icon.
        let size = Vec2::splat(ui.spacing().interact_size.y);
        let button = Button::new(text).frame_when_inactive(false).min_size(size);
        let response = ui
            .add_enabled(self.enabled, button)
            .on_hover_text(tooltip)
            .on_disabled_hover_text(tooltip);
        name(response, WidgetType::Button, self.label, self.enabled)
    }
}

/// An icon on its own, in the icon font.
pub fn glyph(glyph: &str) -> RichText {
    RichText::new(glyph).family(fonts::icon_family())
}

/// A modal dialog with a window's margin rather than egui's default, a popup's, which is sized
/// for menus and leaves a dialog's text and buttons against its edge.
pub fn dialog(ctx: &egui::Context, id: Id) -> Modal {
    let style = ctx.global_style();
    Modal::new(id).frame(Frame::popup(&style).inner_margin(style.spacing.window_margin))
}

/// A menu opened by right-clicking `response`, in DbRover's menu style. Returns what
/// `add_contents` returned, on the frames the menu is open.
pub fn context_menu<R>(response: &Response, add_contents: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    Popup::context_menu(response)
        .style(theme::menu_style)
        .show(|ui| {
            ui.set_min_width(MENU_MIN_WIDTH);
            add_contents(ui)
        })
        .map(|shown| shown.inner)
}

/// A button that opens a menu, in DbRover's menu style; in a menu, an entry that opens a
/// submenu, in the style of the menu it is in.
pub fn menu_button<'a, R>(
    ui: &mut Ui,
    atoms: impl IntoAtoms<'a>,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<Option<R>> {
    let add_contents = |ui: &mut Ui| {
        ui.set_min_width(MENU_MIN_WIDTH);
        add_contents(ui)
    };
    if menu::is_in_menu(ui) {
        return ui.menu_button(atoms, add_contents);
    }
    let (response, inner) = MenuButton::new(atoms)
        .config(menu_config())
        .ui(ui, add_contents);
    InnerResponse::new(inner.map(|shown| shown.inner), response)
}

/// What egui's menus need to draw in DbRover's menu style, submenus included.
pub fn menu_config() -> MenuConfig {
    MenuConfig::new().style(theme::menu_style)
}

/// A menu entry with an icon in front of its text, named by the text alone.
pub fn menu_item(ui: &mut Ui, glyph: &str, text: &str) -> Response {
    let job = icon_text(ui, glyph, Color32::PLACEHOLDER, text);
    let response = ui.button(job);
    name(response, WidgetType::Button, text, true)
}

/// An icon and a message, both in `color`, for status lines.
pub fn status(ui: &mut Ui, glyph_text: &str, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.label(glyph(glyph_text).color(color));
        ui.colored_label(color, text);
    });
}

/// An icon in `icon_color` followed by `text` in the widget's own text colour, for buttons
/// and menu entries. `Color32::PLACEHOLDER` gives the icon the text colour too.
pub fn icon_text(ui: &Ui, glyph: &str, icon_color: Color32, text: &str) -> LayoutJob {
    let font = TextStyle::Button.resolve(ui.style());
    let mut job = LayoutJob::default();
    job.append(
        glyph,
        0.0,
        TextFormat::simple(fonts::icon(font.size), icon_color),
    );
    // PLACEHOLDER takes the widget's text colour, so hover and disabled states still apply.
    job.append(text, 6.0, TextFormat::simple(font, Color32::PLACEHOLDER));
    job
}

/// Overrides the accessible name a widget got from its visible text.
pub fn name(response: Response, kind: WidgetType, label: &str, enabled: bool) -> Response {
    response.widget_info(|| WidgetInfo::labeled(kind, enabled, label));
    response
}

/// A row of tightly spaced icon buttons and controls along the top of a panel.
pub fn toolbar<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = TOOLBAR_SPACING;
        add_contents(ui)
    })
    .inner
}

/// The status bar's link to a config file's problems: `keymap.toml: 2 problems`.
pub fn problems_link(ui: &mut Ui, file_name: &str, count: usize, color: Color32) -> Response {
    let text = match count {
        1 => format!("{file_name}: 1 problem"),
        n => format!("{file_name}: {n} problems"),
    };
    ui.link(RichText::new(text).small().color(color))
}

/// The muted number after a tree label, like "tables 4".
pub fn count_badge(ui: &mut Ui, count: usize) {
    ui.label(
        RichText::new(count.to_string())
            .small()
            .color(ui.visuals().weak_text_color()),
    );
}
