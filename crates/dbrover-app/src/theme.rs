//! DbRover's look: the brand palette from `docs/brand.md` turned into egui visuals for the light
//! and dark themes, the dock's island style, syntax colours and data source colours.
//!
//! Every colour the UI draws comes from a [`Palette`], so a widget asks for a role ("the
//! success colour", "weak text") and gets the shade that is readable in the current theme.
//! `tests::text_colours_meet_wcag_aa` holds the palettes to that. `theme.toml` can replace
//! any of them ([`Palettes::with_overrides`]); what it makes unreadable is listed, not refused.

use std::cell::Cell;
use std::collections::BTreeMap;

use dbrover_core::{DriverKind, SourceColor};
use dbrover_store::{Rgb, ThemeOverrides, ThemeToken};
use egui::style::{Selection, WidgetVisuals, Widgets};
use egui::{Color32, CornerRadius, Margin, Shadow, Stroke, Style, Theme, Vec2, Visuals};

use crate::database_brand;

/// The gap between panels, which turns each panel into a separate rounded "island".
const ISLAND_GAP: f32 = 6.0;
const ISLAND_RADIUS: u8 = 10;
const WIDGET_RADIUS: u8 = 6;
const TAB_BAR_HEIGHT: f32 = 30.0;
/// How strongly the active tab is tinted with the primary colour.
const ACTIVE_TAB_TINT: f32 = 0.14;
/// How strongly the results grid's header is tinted with the primary colour.
pub const GRID_HEADER_TINT: f32 = 0.1;
/// How far a dark theme's cards are lifted from the panel towards the text colour: enough to
/// see the card, not so much that it competes with the fields in it.
const DARK_CARD_LIFT: f32 = 0.04;

/// The colours of one theme, by role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Selection, focus, and the one main action on a screen.
    pub primary: Color32,
    /// Secondary emphasis: SQL keywords, links.
    pub secondary: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
    /// The window behind the panels: the gaps between islands, the menu and status bars.
    pub base: Color32,
    /// The panels themselves, and windows and menus.
    pub panel: Color32,
    /// Text fields, the SQL editor and the results grid: where the user's data is.
    pub field: Color32,
    /// Buttons and other controls at rest.
    pub control: Color32,
    pub control_hover: Color32,
    pub border: Color32,
    pub text: Color32,
    /// Secondary text: details, counts, hints, the status bar.
    pub text_weak: Color32,
    /// Behind selected text and the active tab: the primary, faded into the panel.
    pub selection: Color32,
    /// Text on a solid primary fill, such as the main button on a screen.
    pub on_primary: Color32,
    /// Which theme the palette is for, which picks the shades of data source and database
    /// colours.
    pub dark: bool,
}

impl Palette {
    pub const LIGHT: Self = Self {
        primary: Color32::from_rgb(0x0A, 0x7A, 0x6C),
        secondary: Color32::from_rgb(0x5B, 0x3F, 0xE0),
        success: Color32::from_rgb(0x1C, 0x7C, 0x45),
        warning: Color32::from_rgb(0x98, 0x56, 0x00),
        error: Color32::from_rgb(0xC4, 0x2B, 0x42),
        base: Color32::from_rgb(0xE4, 0xE6, 0xEC),
        panel: Color32::from_rgb(0xFA, 0xFB, 0xFC),
        field: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        control: Color32::from_rgb(0xEC, 0xEE, 0xF2),
        control_hover: Color32::from_rgb(0xDF, 0xE3, 0xEA),
        border: Color32::from_rgb(0xD3, 0xD7, 0xDF),
        text: Color32::from_rgb(0x1D, 0x1F, 0x26),
        text_weak: Color32::from_rgb(0x5C, 0x62, 0x70),
        selection: Color32::from_rgb(0xE0, 0xF4, 0xF0),
        on_primary: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        dark: false,
    };

    pub const DARK: Self = Self {
        primary: Color32::from_rgb(0x2D, 0xD4, 0xBF),
        secondary: Color32::from_rgb(0x9A, 0x8B, 0xFF),
        success: Color32::from_rgb(0x5B, 0xD6, 0x8A),
        warning: Color32::from_rgb(0xF2, 0xB3, 0x3D),
        error: Color32::from_rgb(0xFF, 0x7A, 0x7A),
        base: Color32::from_rgb(0x13, 0x14, 0x18),
        panel: Color32::from_rgb(0x1E, 0x1F, 0x24),
        field: Color32::from_rgb(0x19, 0x1A, 0x1E),
        control: Color32::from_rgb(0x2B, 0x2D, 0x34),
        control_hover: Color32::from_rgb(0x37, 0x3A, 0x44),
        border: Color32::from_rgb(0x30, 0x32, 0x3A),
        text: Color32::from_rgb(0xDF, 0xE2, 0xE8),
        text_weak: Color32::from_rgb(0x95, 0x9B, 0xA8),
        selection: Color32::from_rgb(0x16, 0x30, 0x2E),
        on_primary: Color32::from_rgb(0x0E, 0x1F, 0x1C),
        dark: true,
    };

    /// The brand's palette for `theme`, before `theme.toml`.
    #[cfg(test)]
    pub fn of(theme: Theme) -> &'static Self {
        match theme {
            Theme::Light => &Self::LIGHT,
            Theme::Dark => &Self::DARK,
        }
    }

    /// The palette `visuals` were built from: the one installed last on this thread.
    pub fn for_visuals(visuals: &Visuals) -> Self {
        let theme = if visuals.dark_mode {
            Theme::Dark
        } else {
            Theme::Light
        };
        INSTALLED.get().palettes.of(theme).palette
    }

    fn set(&mut self, token: ThemeToken, rgb: Rgb) {
        let color = Color32::from_rgb(rgb.r, rgb.g, rgb.b);
        let slot = match token {
            ThemeToken::Primary => &mut self.primary,
            ThemeToken::Secondary => &mut self.secondary,
            ThemeToken::Success => &mut self.success,
            ThemeToken::Warning => &mut self.warning,
            ThemeToken::Error => &mut self.error,
            ThemeToken::Base => &mut self.base,
            ThemeToken::Panel => &mut self.panel,
            ThemeToken::Field => &mut self.field,
            ThemeToken::Control => &mut self.control,
            ThemeToken::ControlHover => &mut self.control_hover,
            ThemeToken::Border => &mut self.border,
            ThemeToken::Text => &mut self.text,
            ThemeToken::TextWeak => &mut self.text_weak,
            ThemeToken::Selection => &mut self.selection,
            ThemeToken::OnPrimary => &mut self.on_primary,
        };
        *slot = color;
    }

    /// `color` faded into the panel: a tint for backgrounds that must not compete with text.
    pub fn tint(&self, color: Color32, amount: f32) -> Color32 {
        mix(self.panel, color, amount)
    }

    /// Cards that group a form's rows. A step up from the panel: the field colour in the light
    /// theme, where it is the lightest there is, and the panel lifted towards the text in the
    /// dark theme, where fields are darker than the panel.
    pub fn raised(&self) -> Color32 {
        if self.dark {
            mix(self.panel, self.text, DARK_CARD_LIFT)
        } else {
            self.field
        }
    }

    /// The shade of a data source colour for this theme.
    pub fn source(&self, color: SourceColor) -> Color32 {
        let dark = self.dark;
        let (light, dark_shade) = match color {
            SourceColor::Red => ((0xD9, 0x3B, 0x4A), (0xF0, 0x62, 0x6C)),
            SourceColor::Orange => ((0xE0, 0x6A, 0x1B), (0xFF, 0x96, 0x4F)),
            SourceColor::Yellow => ((0xC2, 0x9A, 0x00), (0xF5, 0xCD, 0x47)),
            SourceColor::Green => ((0x2E, 0x9E, 0x5B), (0x5B, 0xD6, 0x8A)),
            SourceColor::Teal => ((0x0E, 0x93, 0x84), (0x2D, 0xD4, 0xBF)),
            SourceColor::Blue => ((0x2F, 0x6F, 0xE4), (0x6C, 0xA4, 0xFF)),
            SourceColor::Violet => ((0x6A, 0x4C, 0xF0), (0x9A, 0x8B, 0xFF)),
            SourceColor::Pink => ((0xD0, 0x3C, 0x8C), (0xF5, 0x7F, 0xC0)),
        };
        let (r, g, b) = if dark { dark_shade } else { light };
        Color32::from_rgb(r, g, b)
    }
}

impl Palette {
    /// The colour of a database's logo in this theme.
    pub fn driver(&self, driver: DriverKind) -> Color32 {
        let brand = database_brand::of(driver);
        if self.dark { brand.dark } else { brand.light }
    }
}

/// Linear blend from `from` (`amount` 0) to `to` (`amount` 1), ignoring alpha.
fn mix(from: Color32, to: Color32, amount: f32) -> Color32 {
    let channel = |a: u8, b: u8| {
        let blended = f32::from(a) + (f32::from(b) - f32::from(a)) * amount.clamp(0.0, 1.0);
        // Clamped above and rounded, so it always fits in a u8.
        blended.round() as u8
    };
    Color32::from_rgb(
        channel(from.r(), to.r()),
        channel(from.g(), to.g()),
        channel(from.b(), to.b()),
    )
}

/// WCAG 2.x relative luminance.
fn luminance(color: Color32) -> f64 {
    let linear = |channel: u8| {
        let c = f64::from(channel) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
}

/// WCAG 2.x contrast ratio, from 1 to 21.
fn contrast(a: Color32, b: Color32) -> f64 {
    let (light, dark) = {
        let (la, lb) = (luminance(a), luminance(b));
        if la > lb { (la, lb) } else { (lb, la) }
    };
    (light + 0.05) / (dark + 0.05)
}

/// WCAG AA for normal-size text.
const AA: f64 = 4.5;

/// A theme's palette, and whether `theme.toml` chose its selection or primary colour itself,
/// which the system's accent colour then leaves alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    pub palette: Palette,
    own_focus: bool,
}

/// The palettes of both themes: the brand's, with what `theme.toml` replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palettes {
    light: ThemePalette,
    dark: ThemePalette,
}

impl Default for Palettes {
    fn default() -> Self {
        Self::BRAND
    }
}

impl Palettes {
    pub const BRAND: Self = Self {
        light: ThemePalette {
            palette: Palette::LIGHT,
            own_focus: false,
        },
        dark: ThemePalette {
            palette: Palette::DARK,
            own_focus: false,
        },
    };

    pub fn with_overrides(overrides: &ThemeOverrides) -> Self {
        let themed = |base: Palette, colours: &BTreeMap<ThemeToken, Rgb>| {
            let mut palette = base;
            for (&token, &rgb) in colours {
                palette.set(token, rgb);
            }
            ThemePalette {
                palette,
                own_focus: colours.contains_key(&ThemeToken::Primary)
                    || colours.contains_key(&ThemeToken::Selection),
            }
        };
        Self {
            light: themed(Palette::LIGHT, &overrides.light),
            dark: themed(Palette::DARK, &overrides.dark),
        }
    }

    pub fn of(&self, theme: Theme) -> &ThemePalette {
        match theme {
            Theme::Light => &self.light,
            Theme::Dark => &self.dark,
        }
    }

    /// Text that falls below WCAG AA where it is drawn, by the check the brand's palettes are
    /// held to, as sentences for `theme.toml`'s problems.
    pub fn contrast_problems(&self) -> Vec<String> {
        [(Theme::Light, "light"), (Theme::Dark, "dark")]
            .into_iter()
            .flat_map(|(theme, name)| {
                text_pairs(&self.of(theme).palette).into_iter().filter_map(
                    move |(text_name, text, background_name, background)| {
                        let ratio = contrast(text, background);
                        (ratio < AA).then(|| {
                            format!(
                                "{name}: {text_name} on {background_name} is {ratio:.2}:1, \
                                 below the {AA}:1 that WCAG AA asks for"
                            )
                        })
                    },
                )
            })
            .collect()
    }
}

/// What the theme was last installed with on this thread.
#[derive(Debug, Clone, Copy)]
struct Installed {
    palettes: Palettes,
    accent: Option<Color32>,
}

thread_local! {
    /// Load-bearing: [`Palette::for_visuals`] reads the palettes from here. egui's `Visuals`
    /// can't carry a palette, and widgets ask for theirs in dozens of places with only
    /// `ui.visuals()` at hand. egui runs an app on one thread, and each UI test on its own, so
    /// what was installed last on this thread is what its context shows.
    static INSTALLED: Cell<Installed> = const {
        Cell::new(Installed {
            palettes: Palettes::BRAND,
            accent: None,
        })
    };
}

/// Installs DbRover's visuals for both themes, in the brand's palettes. Which one shows follows
/// the OS unless the user picked one in the View menu; egui remembers that choice between runs.
pub fn install(ctx: &egui::Context) {
    apply(
        ctx,
        Installed {
            palettes: Palettes::BRAND,
            accent: None,
        },
    );
}

/// Installs the visuals again with selection and focus in `accent`, the system's accent
/// colour, or in the palette's primary without one. The palettes stay as they were.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only macOS gives DbRover an accent colour")
)]
pub fn install_with_accent(ctx: &egui::Context, accent: Option<Color32>) {
    let palettes = INSTALLED.get().palettes;
    apply(ctx, Installed { palettes, accent });
}

/// Installs the visuals again in `palettes`, from `theme.toml`. The accent colour stays.
pub fn install_palettes(ctx: &egui::Context, palettes: Palettes) {
    let accent = INSTALLED.get().accent;
    apply(ctx, Installed { palettes, accent });
}

fn apply(ctx: &egui::Context, installed: Installed) {
    INSTALLED.set(installed);
    for theme in [Theme::Light, Theme::Dark] {
        let ThemePalette { palette, own_focus } = *installed.palettes.of(theme);
        let focus = match installed.accent {
            Some(accent) if !own_focus => Focus::accent(&palette, theme, accent),
            _ => Focus::brand(&palette),
        };
        ctx.style_mut_of(theme, |style| {
            style.visuals = visuals(&palette, theme, focus);
            tune_spacing(style);
        });
    }
}

/// What selection and focus are drawn in. The brand colours stay for icons, tabs and buttons;
/// selection and focus are what macOS tints with the user's accent colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    /// Focus rings, the text cursor, and the outline of what is selected.
    pub accent: Color32,
    /// Behind selected text, cells and rows.
    pub selection: Color32,
}

/// The most of the accent colour a selection fill takes over the field it is drawn on, in
/// hundredths: about as strong as macOS's own selected-text colour. Less is used where text on
/// it would fall below WCAG AA.
const MAX_ACCENT_SELECTION_TINT_LIGHT: u8 = 20;
const MAX_ACCENT_SELECTION_TINT_DARK: u8 = 30;

impl Focus {
    fn brand(palette: &Palette) -> Self {
        Self {
            accent: palette.primary,
            selection: palette.selection,
        }
    }

    pub fn accent(palette: &Palette, theme: Theme, accent: Color32) -> Self {
        Self {
            accent,
            selection: accent_selection(palette, theme, accent),
        }
    }
}

/// The accent faded into the field as far as needed for every text colour drawn on a
/// selection, plain and SQL's, to keep WCAG AA. With no tint the fill is the field itself, where
/// `tests::text_colours_meet_wcag_aa` holds them to it, so a fill is always found.
fn accent_selection(palette: &Palette, theme: Theme, accent: Color32) -> Color32 {
    let syntax = SyntaxColors::of(palette);
    let texts = [
        palette.text,
        syntax.keyword,
        syntax.string,
        syntax.number,
        syntax.comment,
        syntax.identifier,
        syntax.plain,
    ];
    let max_tint = match theme {
        Theme::Light => MAX_ACCENT_SELECTION_TINT_LIGHT,
        Theme::Dark => MAX_ACCENT_SELECTION_TINT_DARK,
    };
    (0..=max_tint)
        .rev()
        .map(|hundredths| mix(palette.field, accent, f32::from(hundredths) / 100.0))
        .find(|fill| texts.iter().all(|&text| contrast(text, *fill) >= AA))
        .unwrap_or(palette.field)
}

fn visuals(palette: &Palette, theme: Theme, focus: Focus) -> Visuals {
    let mut visuals = theme.default_visuals();
    visuals.override_text_color = None;
    visuals.weak_text_color = Some(palette.text_weak);
    visuals.hyperlink_color = palette.secondary;
    visuals.faint_bg_color = mix(palette.field, palette.text, 0.035);
    visuals.extreme_bg_color = palette.field;
    visuals.text_edit_bg_color = Some(palette.field);
    visuals.code_bg_color = palette.control;
    visuals.warn_fg_color = palette.warning;
    visuals.error_fg_color = palette.error;
    visuals.panel_fill = palette.base;
    visuals.window_fill = palette.panel;
    visuals.window_stroke = Stroke::new(1.0, palette.border);
    visuals.window_corner_radius = CornerRadius::same(ISLAND_RADIUS);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.window_shadow = soft_shadow(theme, 24);
    visuals.popup_shadow = soft_shadow(theme, 12);
    visuals.selection = Selection {
        bg_fill: focus.selection,
        stroke: Stroke::new(1.0, focus.accent),
    };
    visuals.text_cursor.stroke = Stroke::new(2.0, focus.accent);
    // UI tests step egui's clock in fixed amounts, and a blinking cursor asks for a repaint
    // each time it turns, so a frame with a focused field never lets `Harness::run` settle.
    // Every install goes through here (the palettes, theme.toml, the accent colour), so a
    // reload can't turn it back on.
    visuals.text_cursor.blink = !cfg!(test);
    visuals.striped = true;
    visuals.widgets = widgets(palette, focus);
    visuals
}

fn widgets(palette: &Palette, focus: Focus) -> Widgets {
    let radius = CornerRadius::same(WIDGET_RADIUS);
    let state = |fill: Color32, outline: Stroke| WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: outline,
        corner_radius: radius,
        fg_stroke: Stroke::new(1.5, palette.text),
        expansion: 0.0,
    };
    Widgets {
        noninteractive: WidgetVisuals {
            fg_stroke: Stroke::new(1.0, palette.text),
            ..state(palette.panel, Stroke::new(1.0, palette.border))
        },
        inactive: state(palette.control, Stroke::NONE),
        hovered: state(
            palette.control_hover,
            Stroke::new(1.0, mix(palette.border, focus.accent, 0.5)),
        ),
        active: state(focus.selection, Stroke::new(1.0, focus.accent)),
        open: state(palette.control_hover, Stroke::new(1.0, palette.border)),
    }
}

fn soft_shadow(theme: Theme, blur: u8) -> Shadow {
    let alpha = match theme {
        Theme::Light => 40,
        Theme::Dark => 110,
    };
    Shadow {
        offset: [0, 4],
        blur,
        spread: 0,
        color: Color32::from_black_alpha(alpha),
    }
}

/// The height of buttons, fields and other one-line controls, and so of a tree's rows: room
/// for the text and a few points either side, still dense enough for an IDE.
const CONTROL_HEIGHT: f32 = 24.0;
/// Inside a button, around its text: wide enough that a button reads as a shape rather than
/// as text with a tint behind it.
const BUTTON_PADDING: Vec2 = Vec2::new(10.0, 4.0);
const ITEM_SPACING: Vec2 = Vec2::new(8.0, 6.0);
/// Inside dialogs and windows, between their edge and what they show.
const WINDOW_MARGIN: i8 = 16;
const MENU_MARGIN: i8 = 5;
/// Load-bearing: egui opens a window it has no position for 16 points from the top of the
/// screen, over the menu bar, and the window's resize edge reaches a few points above that.
/// At 22 points the titles' middle stays clear of it; at 24, clicking **Help** while the
/// Keymap window is open grabbed the window's edge instead (`menus::items_run_their_commands`).
const MENU_BAR_HEIGHT: f32 = 22.0;

/// A little roomier than egui's defaults, but still dense like an IDE: more rows of a tree or
/// grid fit on screen than in an office app.
fn tune_spacing(style: &mut Style) {
    let spacing = &mut style.spacing;
    spacing.item_spacing = ITEM_SPACING;
    spacing.button_padding = BUTTON_PADDING;
    spacing.interact_size.y = CONTROL_HEIGHT;
    spacing.menu_margin = Margin::same(MENU_MARGIN);
    spacing.window_margin = Margin::same(WINDOW_MARGIN);
    spacing.indent = 16.0;
}

/// The menu bar: no taller than before the other controls grew.
pub fn menu_bar_style(style: &mut Style) {
    style.spacing.interact_size.y = MENU_BAR_HEIGHT;
}

/// egui_dock's look: each node of the dock is a rounded island on the darker base, and the
/// active tab is a subtle highlight rather than a frame.
pub fn dock_style(style: &Style) -> egui_dock::Style {
    let palette = &Palette::for_visuals(&style.visuals);
    let mut dock = egui_dock::Style::from_egui(style);
    let top = CornerRadius {
        nw: ISLAND_RADIUS,
        ne: ISLAND_RADIUS,
        sw: 0,
        se: 0,
    };
    let bottom = CornerRadius {
        nw: 0,
        ne: 0,
        sw: ISLAND_RADIUS,
        se: ISLAND_RADIUS,
    };

    dock.dock_area_padding = Some(Margin::same(ISLAND_GAP as i8));
    dock.main_surface_border_stroke = Stroke::NONE;
    dock.separator.width = ISLAND_GAP;
    dock.separator.color_idle = palette.base;
    dock.separator.color_hovered = palette.tint(palette.primary, 0.5);
    dock.separator.color_dragged = palette.primary;

    dock.tab_bar.bg_fill = palette.panel;
    dock.tab_bar.height = TAB_BAR_HEIGHT;
    dock.tab_bar.corner_radius = top;
    dock.tab_bar.hline_color = palette.border;
    dock.tab_bar.inner_margin = Margin::symmetric(6, 3);

    dock.tab.tab_body.bg_fill = palette.panel;
    dock.tab.tab_body.corner_radius = bottom;
    dock.tab.tab_body.stroke = Stroke::NONE;
    dock.tab.tab_body.inner_margin = Margin::symmetric(8, 6);
    dock.tab.spacing = 2.0;
    dock.tab.hline_below_active_tab_name = false;

    let tab = |fill: Color32, text: Color32| egui_dock::TabInteractionStyle {
        outline_color: Color32::TRANSPARENT,
        corner_radius: CornerRadius::same(WIDGET_RADIUS),
        bg_fill: fill,
        text_color: text,
    };
    let active = tab(palette.tint(palette.primary, ACTIVE_TAB_TINT), palette.text);
    dock.tab.active = active.clone();
    dock.tab.focused = active.clone();
    dock.tab.active_with_kb_focus = active.clone();
    dock.tab.focused_with_kb_focus = active;
    dock.tab.inactive = tab(palette.panel, palette.text_weak);
    dock.tab.inactive_with_kb_focus = tab(palette.control, palette.text);
    dock.tab.hovered = tab(palette.control, palette.text);

    dock.buttons.close_tab_color = palette.text_weak;
    dock.buttons.close_tab_active_color = palette.text;
    dock.buttons.close_tab_bg_fill = palette.control_hover;
    dock.buttons.collapse_tabs_color = palette.text_weak;
    dock.buttons.collapse_tabs_active_color = palette.text;
    dock.buttons.collapse_tabs_bg_fill = palette.control_hover;
    dock.buttons.close_all_tabs_color = palette.text_weak;
    dock.buttons.close_all_tabs_active_color = palette.text;
    dock.buttons.close_all_tabs_bg_fill = palette.control_hover;

    dock.overlay.selection_color = palette.primary.gamma_multiply(0.35);
    dock.overlay.button_color = palette.text_weak;
    dock
}

/// The active-tab style for a console on a data source with a colour: the tab takes the
/// data source's colour instead of the primary, so production consoles stand out.
pub fn source_tab_style(
    base: &egui_dock::TabStyle,
    palette: &Palette,
    color: SourceColor,
) -> egui_dock::TabStyle {
    let shade = palette.source(color);
    let mut style = base.clone();
    for state in [
        &mut style.active,
        &mut style.focused,
        &mut style.active_with_kb_focus,
        &mut style.focused_with_kb_focus,
    ] {
        state.bg_fill = palette.tint(shade, 0.22);
        state.outline_color = palette.tint(shade, 0.6);
    }
    style.inactive.outline_color = palette.tint(shade, 0.45);
    style
}

/// Every text colour against every background it is drawn on.
fn text_pairs(palette: &Palette) -> Vec<(&'static str, Color32, &'static str, Color32)> {
    let syntax = SyntaxColors::of(palette);
    let mut pairs = Vec::new();
    let mut on = |backgrounds: &[(&'static str, Color32)], texts: &[(&'static str, Color32)]| {
        for &(background_name, background) in backgrounds {
            for &(text_name, text) in texts {
                pairs.push((text_name, text, background_name, background));
            }
        }
    };
    on(
        &[
            ("panel", palette.panel),
            ("field", palette.field),
            ("base", palette.base),
            ("control", palette.control),
            ("hovered control", palette.control_hover),
            ("card", palette.raised()),
            ("selection", palette.selection),
            (
                "grid header",
                palette.tint(palette.primary, GRID_HEADER_TINT),
            ),
            ("active tab", palette.tint(palette.primary, ACTIVE_TAB_TINT)),
        ],
        &[("text", palette.text)],
    );
    on(
        &[
            ("panel", palette.panel),
            ("field", palette.field),
            ("base", palette.base),
            ("control", palette.control),
            ("card", palette.raised()),
        ],
        &[("weak text", palette.text_weak)],
    );
    on(
        &[("panel", palette.panel), ("field", palette.field)],
        &[
            ("primary", palette.primary),
            ("secondary", palette.secondary),
            ("success", palette.success),
            ("warning", palette.warning),
            ("error", palette.error),
        ],
    );
    on(
        &[("primary fill", palette.primary)],
        &[("text on primary", palette.on_primary)],
    );
    on(
        &[
            ("editor", palette.field),
            ("selected text", palette.selection),
        ],
        &[
            ("keyword", syntax.keyword),
            ("string", syntax.string),
            ("number", syntax.number),
            ("comment", syntax.comment),
            ("identifier", syntax.identifier),
            ("plain", syntax.plain),
        ],
    );
    pairs
}

/// Colours for SQL syntax highlighting, from the palette.
pub struct SyntaxColors {
    pub keyword: Color32,
    pub string: Color32,
    pub number: Color32,
    pub comment: Color32,
    pub identifier: Color32,
    pub plain: Color32,
}

impl SyntaxColors {
    pub fn for_visuals(visuals: &Visuals) -> Self {
        Self::of(&Palette::for_visuals(visuals))
    }

    fn of(palette: &Palette) -> Self {
        Self {
            keyword: palette.secondary,
            string: palette.success,
            number: palette.warning,
            comment: palette.text_weak,
            identifier: palette.primary,
            plain: palette.text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS's accent colours in sRGB (Blue, Purple, Pink, Red, Orange, Yellow, Green,
    /// Graphite), as `NSColor.controlAccentColor` reports them in the light appearance, and Blue
    /// as the dark appearance lightens it.
    const MACOS_ACCENTS: [(u8, u8, u8); 9] = [
        (0x00, 0x7A, 0xFF),
        (0x95, 0x3D, 0x96),
        (0xF7, 0x4F, 0x9E),
        (0xE0, 0x38, 0x3E),
        (0xF7, 0x82, 0x1B),
        (0xFF, 0xC6, 0x00),
        (0x62, 0xBA, 0x46),
        (0x8C, 0x8C, 0x8C),
        (0x0A, 0x84, 0xFF),
    ];

    /// Enough for a selected row to stand out from the rest of the grid.
    const MIN_SELECTION_CONTRAST: f64 = 1.1;

    #[test]
    fn accent_selection_meets_wcag_aa() {
        let mut failures = Vec::new();
        for theme in [Theme::Light, Theme::Dark] {
            let palette = Palette::of(theme);
            let syntax = SyntaxColors::of(palette);
            for (r, g, b) in MACOS_ACCENTS {
                let selection = Focus::accent(palette, theme, Color32::from_rgb(r, g, b)).selection;
                // Faded only so far that a selection still shows.
                if contrast(selection, palette.field) < MIN_SELECTION_CONTRAST {
                    failures.push(format!(
                        "{theme:?}: #{r:02X}{g:02X}{b:02X}'s selection barely shows"
                    ));
                }
                for (name, text) in [
                    ("text", palette.text),
                    ("keyword", syntax.keyword),
                    ("string", syntax.string),
                    ("number", syntax.number),
                    ("comment", syntax.comment),
                    ("identifier", syntax.identifier),
                ] {
                    let ratio = contrast(text, selection);
                    if ratio < AA {
                        failures.push(format!(
                            "{theme:?}: {name} on #{r:02X}{g:02X}{b:02X}'s selection is {ratio:.2}:1"
                        ));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn text_colours_meet_wcag_aa() {
        let failures = Palettes::BRAND.contrast_problems();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn theme_toml_replaces_colours_and_lists_what_it_makes_unreadable() {
        let overrides = dbrover_store::parse_theme(
            "[dark]\nprimary = \"#9a8bff\"\n[light]\ntext_weak = \"#bbbbbb\"\n",
        )
        .value;
        let palettes = Palettes::with_overrides(&overrides);

        assert_eq!(
            palettes.of(Theme::Dark).palette.primary,
            Color32::from_rgb(0x9a, 0x8b, 0xff)
        );
        assert_eq!(palettes.of(Theme::Dark).palette.base, Palette::DARK.base);
        assert!(palettes.of(Theme::Dark).own_focus);
        assert!(!palettes.of(Theme::Light).own_focus);
        assert!(palettes.of(Theme::Dark).palette.dark);
        let problems = palettes.contrast_problems();
        assert!(
            problems.contains(
                &"light: weak text on panel is 1.85:1, below the 4.5:1 that WCAG AA asks for"
                    .to_owned()
            ),
            "{problems:#?}"
        );
        assert!(
            problems
                .iter()
                .all(|problem| problem.starts_with("light: ")),
            "{problems:#?}"
        );
    }

    #[test]
    fn the_accent_tints_focus_unless_the_file_chose_it() {
        let ctx = egui::Context::default();
        let accent = Color32::from_rgb(0xF7, 0x4F, 0x9E);
        let overrides = dbrover_store::parse_theme("[dark]\nselection = \"#333344\"\n").value;
        install_palettes(&ctx, Palettes::with_overrides(&overrides));
        install_with_accent(&ctx, Some(accent));

        let stroke = |theme| ctx.style_of(theme).visuals.selection.stroke.color;
        assert_eq!(stroke(Theme::Light), accent);
        assert_eq!(stroke(Theme::Dark), Palette::DARK.primary);
        assert_eq!(
            ctx.style_of(Theme::Dark).visuals.selection.bg_fill,
            Color32::from_rgb(0x33, 0x33, 0x44)
        );
        install(&ctx);
        assert_eq!(stroke(Theme::Light), Palette::LIGHT.primary);
    }

    /// WCAG 2.1's minimum for graphics that identify something (success criterion 1.4.11).
    const GRAPHICS: f64 = 3.0;

    #[test]
    fn database_logos_stand_out_from_what_they_are_drawn_on() {
        for theme in [Theme::Light, Theme::Dark] {
            let palette = Palette::of(theme);
            // The explorer and the dialog, a text field, and a hovered menu entry.
            for background in [palette.panel, palette.field, palette.control_hover] {
                for driver in DriverKind::ALL {
                    let ratio = contrast(palette.driver(driver), background);
                    assert!(
                        ratio >= GRAPHICS,
                        "{theme:?}: the {driver:?} logo is {ratio:.2}:1 on {background:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_cursor_stays_still_in_ui_tests_after_every_install() {
        let ctx = egui::Context::default();
        install(&ctx);
        install_palettes(&ctx, Palettes::BRAND);
        install_with_accent(&ctx, Some(Color32::from_rgb(0x00, 0x7A, 0xFF)));
        for theme in [Theme::Light, Theme::Dark] {
            assert!(!ctx.style_of(theme).visuals.text_cursor.blink, "{theme:?}");
        }
    }

    #[test]
    fn contrast_matches_known_values() {
        assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.01);
    }
}
