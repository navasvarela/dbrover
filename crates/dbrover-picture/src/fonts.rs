//! The bundled typefaces, Inter and JetBrains Mono, which pictures are drawn and measured with
//! whatever the machine has. Their licences are beside the files in `assets/fonts/`.

use std::fmt;

/// Inter, the interface's text and every picture's.
pub const INTER_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/inter/Inter-Regular.ttf");
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/inter/Inter-SemiBold.ttf");
/// JetBrains Mono, for SQL, results and a diagram's column types.
pub const JETBRAINS_MONO: &[u8] =
    include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf");

/// The font files the SVG's text is drawn with. Only these: the system's fonts are never
/// looked at, so a PNG looks the same on every machine.
#[derive(Clone)]
pub struct Fonts {
    /// The first is the face for any family the SVG names that isn't among them.
    pub files: Vec<&'static [u8]>,
    /// The family of the first file, which text falls back to.
    pub fallback_family: &'static str,
}

impl Fonts {
    /// The bundled faces, Inter first.
    pub fn bundled() -> Self {
        Self {
            files: vec![INTER_REGULAR, INTER_SEMIBOLD, JETBRAINS_MONO],
            fallback_family: "Inter",
        }
    }
}

impl fmt::Debug for Fonts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} font files", self.files.len())
    }
}
