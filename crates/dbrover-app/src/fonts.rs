//! The typefaces: Inter for the interface, JetBrains Mono for SQL and results, and Phosphor for
//! icons, all embedded so the app looks the same on every machine. Licences are next to the font
//! files in `assets/fonts/`. On macOS the real window uses the system's SF Pro and SF Mono
//! instead ([`install_system`]), read from the system and never shipped.
//!
//! Icons have a family of their own, [`icon`], rather than being a fallback of the text
//! families as egui-phosphor sets them up. Inter has its own glyphs at hundreds of the
//! private-use code points Phosphor uses, so as a fallback behind Inter many icons would come
//! out as stray accented letters. Phosphor can't go first either: it maps a–z to the blank
//! glyphs its ligatures are built from.

#[cfg(any(target_os = "macos", test))]
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "macos", test))]
use egui::epaint::text::{FontTweak, VariationCoords};
use egui::{FontData, FontDefinitions, FontFamily, FontId};

/// Names of the faces in egui's font definitions, by role: the faces behind them are Inter and
/// JetBrains Mono, or SF Pro and SF Mono.
const SANS: &str = "Sans";
const SANS_SEMIBOLD: &str = "Sans SemiBold";
const MONO: &str = "Mono";
const PHOSPHOR: &str = "Phosphor";

/// Text sizes in points.
const BODY_SIZE: f32 = 13.0;
const SMALL_SIZE: f32 = 11.0;
const HEADING_SIZE: f32 = 20.0;
const MONOSPACE_SIZE: f32 = 12.5;

/// The icon font. Text in it that isn't an icon falls back to Inter.
pub fn icon(size: f32) -> FontId {
    FontId::new(size, icon_family())
}

pub fn icon_family() -> FontFamily {
    FontFamily::Name(PHOSPHOR.into())
}

/// A semibold face, for panel titles and headings. egui has no font weights, so it is
/// a family of its own.
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SANS_SEMIBOLD.into()))
}

/// Installs the bundled faces and DbRover's text sizes.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(definitions(Faces::bundled()));
    ctx.all_styles_mut(|style| {
        use egui::TextStyle;
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(SMALL_SIZE)),
            (TextStyle::Body, FontId::proportional(BODY_SIZE)),
            (TextStyle::Button, FontId::proportional(BODY_SIZE)),
            (TextStyle::Heading, semibold(HEADING_SIZE)),
            (TextStyle::Monospace, FontId::monospace(MONOSPACE_SIZE)),
        ]
        .into();
    });
}

/// Uses macOS's own SF Pro and SF Mono instead of the bundled faces, keeping those if the
/// system's can't be read. Text sizes stay as [`install`] set them: SF Pro at 13 points is
/// macOS's own body size.
#[cfg(target_os = "macos")]
pub fn install_system(ctx: &egui::Context) {
    match Faces::from_files(Path::new(SF_PRO), Path::new(SF_MONO)) {
        Ok(faces) => ctx.set_fonts(definitions(faces)),
        Err(error) => tracing::warn!("keeping the bundled fonts: {error}"),
    }
}

/// Where macOS keeps its system faces. Apple has renamed them before (`SFNSDisplay.ttf`,
/// `SFNSText.ttf`), which the fallback covers.
#[cfg(target_os = "macos")]
const SF_PRO: &str = "/System/Library/Fonts/SFNS.ttf";
#[cfg(target_os = "macos")]
const SF_MONO: &str = "/System/Library/Fonts/SFNSMono.ttf";

/// Values on the variable system faces' axes. SF Pro's optical size defaults to 28, its display
/// cut; 17 is the text cut, drawn for the 11–13 point sizes the interface uses. SF Mono's weight
/// defaults to its lightest, 294.67. 400 and 590 are SF's regular and semibold weights.
#[cfg(any(target_os = "macos", test))]
const TEXT_OPTICAL_SIZE: f32 = 17.0;
#[cfg(any(target_os = "macos", test))]
const REGULAR_WEIGHT: f32 = 400.0;
#[cfg(any(target_os = "macos", test))]
const SEMIBOLD_WEIGHT: f32 = 590.0;

/// The system faces couldn't be used.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, thiserror::Error)]
enum SystemFontError {
    #[error("couldn't read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} isn't a variable font with a weight axis", path.display())]
    NotVariable { path: PathBuf },
}

/// The faces behind the text families.
struct Faces {
    sans: FontData,
    sans_semibold: FontData,
    mono: FontData,
}

const INTER_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/inter/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/inter/Inter-SemiBold.ttf");
const JETBRAINS_MONO: &[u8] =
    include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf");

/// The bundled faces, for drawing a diagram's PNG with the app's own text whatever the
/// system has.
pub fn bundled_files() -> dbrover_diagram::Fonts {
    dbrover_diagram::Fonts {
        files: vec![INTER_REGULAR, INTER_SEMIBOLD, JETBRAINS_MONO],
        fallback_family: "Inter",
    }
}

impl Faces {
    fn bundled() -> Self {
        Self {
            sans: FontData::from_static(INTER_REGULAR),
            sans_semibold: FontData::from_static(INTER_SEMIBOLD),
            mono: FontData::from_static(JETBRAINS_MONO),
        }
    }

    /// Variable faces for the sans and the monospace, at the weights and optical size above.
    #[cfg(any(target_os = "macos", test))]
    fn from_files(sans: &Path, mono: &Path) -> Result<Self, SystemFontError> {
        let sans_bytes = read_variable(sans)?;
        let at = |coords: &[(&[u8; 4], f32)]| FontTweak {
            coords: VariationCoords::new(coords.iter().copied()),
            ..FontTweak::default()
        };
        Ok(Self {
            sans: FontData::from_owned(sans_bytes.clone()).tweak(at(&[
                (b"wght", REGULAR_WEIGHT),
                (b"opsz", TEXT_OPTICAL_SIZE),
            ])),
            sans_semibold: FontData::from_owned(sans_bytes).tweak(at(&[
                (b"wght", SEMIBOLD_WEIGHT),
                (b"opsz", TEXT_OPTICAL_SIZE),
            ])),
            mono: FontData::from_owned(read_variable(mono)?)
                .tweak(at(&[(b"wght", REGULAR_WEIGHT)])),
        })
    }
}

/// Reads a variable font that has a weight axis. egui would fail on a file that doesn't parse
/// only once it draws text, so it is checked here, where the bundled faces can still be used.
#[cfg(any(target_os = "macos", test))]
fn read_variable(path: &Path) -> Result<Vec<u8>, SystemFontError> {
    let bytes = std::fs::read(path).map_err(|source| SystemFontError::Read {
        path: path.to_owned(),
        source,
    })?;
    let has_weight = FontData::from_owned(bytes.clone())
        .variation_axes()
        .iter()
        .any(|axis| axis.tag.to_be_bytes() == *b"wght");
    if has_weight {
        Ok(bytes)
    } else {
        Err(SystemFontError::NotVariable {
            path: path.to_owned(),
        })
    }
}

fn definitions(faces: Faces) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    for (name, data) in [
        (SANS, faces.sans),
        (SANS_SEMIBOLD, faces.sans_semibold),
        (MONO, faces.mono),
        (
            PHOSPHOR,
            FontData::from_static(egui_phosphor::Variant::Regular.font_bytes()),
        ),
    ] {
        fonts.font_data.insert(name.to_owned(), data.into());
    }
    // egui's own fonts stay behind ours as fallbacks, for symbols and emoji ours lack.
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, SANS.to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, MONO.to_owned());
    let proportional = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut semibold = vec![SANS_SEMIBOLD.to_owned()];
    semibold.extend(proportional.into_iter().filter(|name| name != SANS));
    fonts
        .families
        .insert(FontFamily::Name(SANS_SEMIBOLD.into()), semibold);
    fonts
        .families
        .insert(icon_family(), vec![PHOSPHOR.to_owned(), SANS.to_owned()]);
    fonts
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTER_REGULAR: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/fonts/inter/Inter-Regular.ttf"
    );

    #[test]
    fn a_missing_system_font_is_named() {
        let missing = Path::new("/nowhere/SFNS.ttf");
        let error = Faces::from_files(missing, missing)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("/nowhere/SFNS.ttf"), "{error}");
    }

    #[test]
    fn a_static_font_is_not_taken_for_the_system_s() {
        let inter = Path::new(INTER_REGULAR);
        assert!(matches!(
            Faces::from_files(inter, inter),
            Err(SystemFontError::NotVariable { .. })
        ));
    }

    /// Only where the system faces are: on macOS.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_faces_load_at_their_weights() {
        let faces = Faces::from_files(Path::new(SF_PRO), Path::new(SF_MONO));
        assert!(faces.is_ok(), "{:?}", faces.err());
    }
}
