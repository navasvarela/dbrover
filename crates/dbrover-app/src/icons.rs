//! Which icon, in which colour, stands for each driver, catalog object and action.
//!
//! Icons are Phosphor glyphs (see `fonts`), so they scale with the text and take any colour.
//! Colours are palette roles, never fixed values, so every icon is readable in both themes.

use dbrover_core::{DriverKind, KeyType, NodeKind, SourceColor};
use egui::{Color32, Image, RichText, Vec2};
use egui_phosphor::regular as glyph;

use crate::theme::Palette;
use crate::{database_brand, fonts};

pub use egui_phosphor::regular::{
    ARROW_DOWN, ARROW_SQUARE_RIGHT, ARROW_UP, ARROWS_CLOCKWISE, BOOK_OPEN, BRACKETS_CURLY,
    CARET_DOWN, CARET_RIGHT, CHECK_CIRCLE, CLOCK_COUNTER_CLOCKWISE, CODE, COLUMNS, COPY, ERASER,
    EXPORT, EYE, EYE_SLASH, FAST_FORWARD, FILE_CSV, FILE_HTML, FILE_PLUS, FILE_SQL, FILE_TXT,
    FILE_XLS, FOLDER_OPEN, FOLDER_SIMPLE, FOLDER_SIMPLE_DASHED, FOLDER_SIMPLE_PLUS, GEAR, KEYBOARD,
    LIGHTNING, LIST_BULLETS, LIST_DASHES, LOCK_SIMPLE, MAGIC_WAND, MAGNIFYING_GLASS, MARKDOWN_LOGO,
    MICROSOFT_EXCEL_LOGO, MONITOR, MOON, PALETTE, PENCIL_SIMPLE, PENCIL_SIMPLE_LINE, PLAY, PLUGS,
    PLUGS_CONNECTED, PLUS, PROHIBIT, SPARKLE, STACK, STOP, SUN, TABLE, TERMINAL_WINDOW,
    TEXT_INDENT, TRASH, TREE_STRUCTURE, TREE_VIEW, USER, USERS, WARNING, X, X_CIRCLE,
};

/// A glyph and the palette role that colours it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icon {
    pub glyph: &'static str,
    pub role: Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Primary,
    Secondary,
    Success,
    Warning,
    Error,
    Weak,
    /// One of the data source colours, for icons that need a hue the palette's roles lack.
    Source(SourceColor),
}

impl Role {
    pub fn color(self, palette: &Palette) -> Color32 {
        match self {
            Self::Primary => palette.primary,
            Self::Secondary => palette.secondary,
            Self::Success => palette.success,
            Self::Warning => palette.warning,
            Self::Error => palette.error,
            Self::Weak => palette.text_weak,
            Self::Source(color) => palette.source(color),
        }
    }
}

impl Icon {
    const fn new(glyph: &'static str, role: Role) -> Self {
        Self { glyph, role }
    }

    pub fn rich_text(self, palette: &Palette) -> RichText {
        RichText::new(self.glyph)
            .family(fonts::icon_family())
            .color(self.role.color(palette))
    }
}

/// A database's logo, tinted for the current theme. Needs egui_extras' image loaders,
/// installed at start-up.
pub fn driver_logo(driver: DriverKind, palette: &Palette) -> Image<'static> {
    let brand = database_brand::of(driver);
    Image::from_bytes(brand.logo_uri, brand.logo_svg).tint(palette.driver(driver))
}

/// A database's logo at `size` points square, the size of the text it sits beside.
pub fn driver_logo_sized(driver: DriverKind, palette: &Palette, size: f32) -> Image<'static> {
    driver_logo(driver, palette).fit_to_exact_size(Vec2::splat(size))
}

/// A SQL keyword, in the completion list.
pub fn keyword() -> Icon {
    Icon::new(glyph::TEXT_AA, Role::Secondary)
}

/// A snippet from `snippets.toml`, in the completion list.
pub fn snippet() -> Icon {
    Icon::new(glyph::BRACKETS_CURLY, Role::Success)
}

/// A table's alias in a statement, in the completion list.
pub fn alias() -> Icon {
    Icon::new(glyph::TAG, Role::Weak)
}

/// A table the statement defines itself (a CTE or a subquery), in the completion list.
pub fn derived_table() -> Icon {
    Icon::new(glyph::BRACKETS_ROUND, Role::Primary)
}

pub fn node(kind: NodeKind) -> Icon {
    match kind {
        NodeKind::Database => Icon::new(glyph::DATABASE, Role::Secondary),
        NodeKind::Schema => Icon::new(glyph::TREE_STRUCTURE, Role::Secondary),
        NodeKind::Folder(_) => Icon::new(glyph::FOLDER_SIMPLE, Role::Warning),
        NodeKind::Table => Icon::new(glyph::TABLE, Role::Primary),
        NodeKind::View => Icon::new(glyph::EYE, Role::Primary),
        NodeKind::MaterializedView => Icon::new(glyph::STACK, Role::Primary),
        NodeKind::Function => Icon::new(glyph::FUNCTION, Role::Success),
        NodeKind::Procedure => Icon::new(glyph::GEAR_SIX, Role::Success),
        NodeKind::Event => Icon::new(glyph::CLOCK, Role::Success),
        NodeKind::Synonym => Icon::new(glyph::ARROW_BEND_UP_RIGHT, Role::Success),
        NodeKind::Dictionary => Icon::new(glyph::BOOK_OPEN, Role::Primary),
        NodeKind::Sequence => Icon::new(glyph::LIST_NUMBERS, Role::Success),
        NodeKind::Column => Icon::new(glyph::TEXT_COLUMNS, Role::Weak),
        NodeKind::Index => Icon::new(glyph::LIST_MAGNIFYING_GLASS, Role::Warning),
        NodeKind::Constraint => Icon::new(glyph::KEY, Role::Warning),
        NodeKind::Trigger => Icon::new(glyph::LIGHTNING, Role::Error),
        NodeKind::Role => Icon::new(glyph::USER, Role::Secondary),
        NodeKind::KeyPrefix => Icon::new(glyph::FOLDER_SIMPLE, Role::Warning),
        NodeKind::Key(key_type) => key(key_type),
        NodeKind::Collection => Icon::new(glyph::FILES, Role::Primary),
        NodeKind::SearchIndex => Icon::new(glyph::MAGNIFYING_GLASS, Role::Primary),
    }
}

/// A Redis key's icon, by the type of its value.
fn key(key_type: KeyType) -> Icon {
    match key_type {
        KeyType::String => Icon::new(glyph::TEXT_T, Role::Primary),
        KeyType::Hash => Icon::new(glyph::HASH, Role::Primary),
        KeyType::List => Icon::new(glyph::LIST_BULLETS, Role::Primary),
        KeyType::Set => Icon::new(glyph::CIRCLES_THREE, Role::Primary),
        KeyType::SortedSet => Icon::new(glyph::RANKING, Role::Primary),
        KeyType::Stream => Icon::new(glyph::WAVE_SINE, Role::Primary),
        KeyType::Json => Icon::new(glyph::BRACKETS_CURLY, Role::Primary),
        KeyType::Other => Icon::new(glyph::KEY, Role::Weak),
    }
}

#[cfg(test)]
mod tests {
    use egui::load::{ImagePoll, SizeHint};

    use super::*;

    #[test]
    fn every_driver_logo_renders_to_a_visible_image() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        for driver in DriverKind::ALL {
            let image = driver_logo(driver, &Palette::LIGHT);
            let source = image.source(&ctx);
            let uri = source.uri().expect("logos are loaded by URI");
            if let egui::ImageSource::Bytes { uri, bytes } = &source {
                ctx.include_bytes(uri.clone(), bytes.clone());
            }
            let poll = ctx
                .try_load_image(
                    uri,
                    SizeHint::Size {
                        width: 32,
                        height: 32,
                        maintain_aspect_ratio: true,
                    },
                )
                .unwrap_or_else(|error| panic!("{driver:?}: {error}"));
            let ImagePoll::Ready { image } = poll else {
                panic!("{driver:?}: the SVG loader answers synchronously");
            };
            // Catches a blank or unreadable SVG, not a sparse one: the MySQL dolphin is drawn
            // in thin strokes and has the fewest opaque pixels of the logos, 45 at this size.
            let opaque = image.pixels.iter().filter(|pixel| pixel.a() > 200).count();
            assert!(opaque > 30, "{driver:?}: only {opaque} opaque pixels");
        }
    }
}
