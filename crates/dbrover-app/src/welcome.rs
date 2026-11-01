//! The Welcome tab: the logo over a soft glow, cards for the first things to do, and a pointer
//! to the manual.

use std::f32::consts::TAU;

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{
    Align, Button, Color32, CornerRadius, Frame, Layout, Margin, Pos2, Response, RichText, Sense,
    Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType, vec2,
};

use crate::brand;
use crate::explorer::{self, ExplorerAction};
use crate::fonts;
use crate::icons::{self, Icon, Role};
use crate::state::DataSources;
use crate::theme::{self, Palette};
use crate::widgets;

/// Where "Open the manual" goes: the user manual on DbRover's website, published from `book/`
/// by `.github/workflows/pages.yml`.
pub const MANUAL_URL: &str = "https://dbrover.org/manual/";

const LOGO_SIZE: f32 = 64.0;
const CARD_SIZE: Vec2 = vec2(132.0, 104.0);
const CARD_ICON_SIZE: f32 = 28.0;
const CARD_RADIUS: u8 = 10;
const CONTENT_WIDTH: f32 = 3.0 * CARD_SIZE.x + 2.0 * CARD_GAP;
const CARD_GAP: f32 = 12.0;
/// How far down the glow behind the logo reaches.
const GLOW_HEIGHT: f32 = 260.0;
const GLOW_SEGMENTS: u32 = 48;

/// The first-step cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Card {
    NewDataSource,
    NewConsole,
    Manual,
}

impl Card {
    const ALL: [Self; 3] = [Self::NewDataSource, Self::NewConsole, Self::Manual];

    fn icon(self) -> Icon {
        match self {
            Self::NewDataSource => Icon {
                glyph: icons::PLUS,
                role: Role::Primary,
            },
            Self::NewConsole => Icon {
                glyph: icons::TERMINAL_WINDOW,
                role: Role::Secondary,
            },
            Self::Manual => Icon {
                glyph: icons::BOOK_OPEN,
                role: Role::Success,
            },
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::NewDataSource => "New data source",
            Self::NewConsole => "New query console",
            Self::Manual => "Open the manual",
        }
    }

    /// The card a new user most likely wants next, drawn with an accent border.
    fn suggested(data_sources: &DataSources) -> Self {
        if data_sources.entries().is_empty() {
            Self::NewDataSource
        } else {
            Self::NewConsole
        }
    }
}

pub fn show(ui: &mut Ui, data_sources: &DataSources, actions: &mut Vec<ExplorerAction>) {
    let palette = &Palette::for_visuals(ui.visuals());
    paint_glow(ui, palette);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.add(brand::logo().fit_to_exact_size(Vec2::splat(LOGO_SIZE)));
                ui.add_space(12.0);
                ui.label(RichText::new("Welcome to DbRover").font(fonts::semibold(26.0)));
                ui.label(RichText::new(brand::MOTTO).color(palette.text_weak));
                ui.add_space(20.0);
                cards(ui, data_sources, actions, palette);
                ui.add_space(16.0);
                getting_started(ui, palette);
            });
        });
}

fn cards(
    ui: &mut Ui,
    data_sources: &DataSources,
    actions: &mut Vec<ExplorerAction>,
    palette: &Palette,
) {
    let suggested = Card::suggested(data_sources);
    let width = CONTENT_WIDTH.min(ui.available_width());
    ui.allocate_ui_with_layout(
        vec2(width, CARD_SIZE.y),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = CARD_GAP;
            for card in Card::ALL {
                let enabled = card != Card::NewConsole || !data_sources.entries().is_empty();
                let response = card_button(ui, card, card == suggested, enabled, palette);
                match card {
                    Card::NewDataSource => {
                        egui::Popup::menu(&response)
                            .style(theme::menu_style)
                            .show(|ui| explorer::new_data_source_menu(ui, actions));
                    }
                    Card::NewConsole => {
                        egui::Popup::menu(&response)
                            .style(theme::menu_style)
                            .show(|ui| console_menu(ui, data_sources, actions));
                    }
                    Card::Manual => {
                        if response.clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(MANUAL_URL));
                        }
                    }
                }
            }
        },
    );
}

/// A square card with a large icon over its label.
fn card_button(
    ui: &mut Ui,
    card: Card,
    suggested: bool,
    enabled: bool,
    palette: &Palette,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(CARD_SIZE, sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, card.label()));
    let border = if enabled && (suggested || response.hovered()) {
        Stroke::new(1.5, palette.primary)
    } else {
        Stroke::new(1.0, palette.border)
    };
    let fill = if enabled && response.hovered() {
        palette.control
    } else {
        palette.field
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(CARD_RADIUS),
        fill,
        border,
        StrokeKind::Inside,
    );
    let alpha = if enabled { 1.0 } else { 0.4 };
    let icon = card.icon();
    painter.text(
        rect.center() - vec2(0.0, 12.0),
        egui::Align2::CENTER_CENTER,
        icon.glyph,
        fonts::icon(CARD_ICON_SIZE),
        icon.role.color(palette).gamma_multiply(alpha),
    );
    painter.text(
        rect.center() + vec2(0.0, 26.0),
        egui::Align2::CENTER_CENTER,
        card.label(),
        egui::FontId::proportional(12.5),
        palette.text.gamma_multiply(alpha),
    );
    if enabled {
        response
    } else {
        response.on_hover_text("Add a data source first")
    }
}

fn console_menu(ui: &mut Ui, data_sources: &DataSources, actions: &mut Vec<ExplorerAction>) {
    for entry in data_sources.entries() {
        let data_source = &entry.data_source;
        let palette = &Palette::for_visuals(ui.visuals());
        let logo = icons::driver_logo_sized(
            data_source.config.driver(),
            palette,
            ui.text_style_height(&egui::TextStyle::Button),
        );
        let response = widgets::name(
            ui.add(Button::image_and_text(logo, data_source.name.as_str())),
            WidgetType::Button,
            &data_source.name,
            true,
        );
        if response.clicked() {
            actions.push(ExplorerAction::NewConsole(data_source.id));
        }
    }
}

/// A wide card with a short pitch for the manual and a solid primary button.
fn getting_started(ui: &mut Ui, palette: &Palette) {
    let width = CONTENT_WIDTH.min(ui.available_width());
    Frame::new()
        .fill(palette.field)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(width - 32.0);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.label(RichText::new("New to DbRover?").font(fonts::semibold(15.0)));
                ui.label(
                    RichText::new(
                        "The manual walks through adding a data source, browsing its tables \
                         and running your first query.",
                    )
                    .color(palette.text_weak),
                );
                ui.add_space(6.0);
                let mut label =
                    widgets::icon_text(ui, icons::BOOK_OPEN, palette.on_primary, "Read the manual");
                for section in &mut label.sections {
                    section.format.color = palette.on_primary;
                }
                let button = Button::new(label)
                    .fill(palette.primary)
                    .corner_radius(CornerRadius::same(6))
                    .min_size(vec2(0.0, 28.0));
                let response =
                    widgets::name(ui.add(button), WidgetType::Button, "Read the manual", true);
                if response.clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(MANUAL_URL));
                }
            });
        });
}

/// Two soft radial glows along the top of the tab, the secondary on the left and the primary
/// on the right, fading into the panel.
fn paint_glow(ui: &Ui, palette: &Palette) {
    let rect = ui.max_rect();
    // Light backgrounds show a tint more readily than dark ones, so the light glow is fainter.
    let strength = if ui.visuals().dark_mode { 0.22 } else { 0.16 };
    let radius = vec2(rect.width() * 0.45, GLOW_HEIGHT);
    let painter = ui.painter();
    for (x, color) in [(0.3, palette.secondary), (0.7, palette.primary)] {
        let center = Pos2::new(rect.left() + rect.width() * x, rect.top());
        painter.add(radial_glow(center, radius, color.gamma_multiply(strength)));
    }
}

/// A fan of triangles from a coloured centre to a transparent ellipse: a cheap radial
/// gradient, since egui has no gradient fills.
fn radial_glow(center: Pos2, radius: Vec2, color: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    mesh.vertices.push(Vertex {
        pos: center,
        uv: WHITE_UV,
        color,
    });
    for step in 0..=GLOW_SEGMENTS {
        let angle = TAU * step as f32 / GLOW_SEGMENTS as f32;
        mesh.vertices.push(Vertex {
            pos: center + vec2(angle.cos() * radius.x, angle.sin() * radius.y),
            uv: WHITE_UV,
            color: Color32::TRANSPARENT,
        });
    }
    // Vertex 0 is the centre; the rim starts at 1.
    for rim in 1..=GLOW_SEGMENTS {
        mesh.add_triangle(0, rim, rim + 1);
    }
    mesh
}
