//! The colours a chart is drawn in, and how its marks look beyond their place: the width of a
//! line, and the rounding of a bar.

use dbrover_picture::Rgb;

use crate::Paint;

/// The series' colours, in the order series take them, for the light theme and the dark one:
/// eight hues whose neighbours stay apart for colour-blind readers, each stepped for its
/// theme's background. A ninth series takes the first colour again.
pub const SERIES_LIGHT: [Rgb; 8] = [
    Rgb(0x2a, 0x78, 0xd6),
    Rgb(0xeb, 0x68, 0x34),
    Rgb(0x1b, 0xaf, 0x7a),
    Rgb(0xed, 0xa1, 0x00),
    Rgb(0xe8, 0x7b, 0xa4),
    Rgb(0x00, 0x83, 0x00),
    Rgb(0x62, 0x50, 0xd6),
    Rgb(0xe3, 0x49, 0x48),
];
pub const SERIES_DARK: [Rgb; 8] = [
    Rgb(0x39, 0x87, 0xe5),
    Rgb(0xd9, 0x59, 0x26),
    Rgb(0x19, 0x9e, 0x70),
    Rgb(0xc9, 0x85, 0x00),
    Rgb(0xd5, 0x51, 0x81),
    Rgb(0x00, 0x83, 0x00),
    Rgb(0x90, 0x85, 0xe9),
    Rgb(0xe6, 0x67, 0x67),
];

/// The size of a file's text in points: the tab's small text, so a report's chart reads as
/// the tab's does.
pub const FILE_TEXT_SIZE: f32 = 11.0;

/// A series' line, in points.
pub const LINE_WIDTH: f32 = 2.0;
/// Bars this wide or wider get rounded ends; narrower ones would round into slivers.
pub const ROUNDED_BAR_WIDTH: f32 = 8.0;
/// The radius of a rounded bar's corners, in points.
pub const BAR_ROUNDING: u8 = 2;

/// The colours of a chart's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileColours {
    pub background: Rgb,
    pub series: [Rgb; 8],
    pub axis: Rgb,
    pub grid: Rgb,
    pub label: Rgb,
    pub title: Rgb,
}

/// A file's colours: the light theme's on white, whatever the app's theme, so the picture reads
/// in a document or a pull request. The app's light palette holds the same values, and its
/// tests say so.
pub const FILE_COLOURS: FileColours = FileColours {
    background: Rgb(0xff, 0xff, 0xff),
    series: SERIES_LIGHT,
    axis: Rgb(0x5c, 0x62, 0x70),
    grid: Rgb(0xd3, 0xd7, 0xdf),
    label: Rgb(0x5c, 0x62, 0x70),
    title: Rgb(0x1d, 0x1f, 0x26),
};

impl FileColours {
    /// `paint`'s colour, with `named` — a spec's `colours` — in the palette's place for the
    /// series it names.
    pub fn of(&self, paint: Paint, named: &[Rgb]) -> Rgb {
        match paint {
            Paint::Series(index) => series_colour(index, named, &self.series),
            Paint::Axis => self.axis,
            Paint::Grid => self.grid,
            Paint::Label => self.label,
            Paint::Title => self.title,
        }
    }
}

/// The hue for the series at `index`: the one a spec named for it, else `palette`'s own, which
/// steps round when a chart has more series than colours.
///
/// Both surfaces ask here — a file's [`FileColours`] and the app's palette — so a colour a spec
/// names is that exact hue on screen and in the file, while a series it doesn't name keeps each
/// surface's own hue, stepped for that surface's background. A list shorter than the series
/// therefore recolours only what it names, which is the common case: one series picked out.
pub fn series_colour(index: usize, named: &[Rgb], palette: &[Rgb; 8]) -> Rgb {
    named
        .get(index)
        .copied()
        .unwrap_or_else(|| palette[index % palette.len()])
}

/// A hex colour as a chart spec's `colours` writes one: `#rrggbb` or `#rgb`, in either case.
///
/// `None` for anything else, a named colour included. The 148 CSS names would be a list to
/// carry and to keep matching between egui and the SVG, for colours a hex code already names.
pub fn hex_colour(text: &str) -> Option<Rgb> {
    let digits = text.strip_prefix('#')?;
    let channel = |at: usize, width: usize| -> Option<u8> {
        u8::from_str_radix(digits.get(at..at + width)?, 16).ok()
    };
    match digits.len() {
        // `#abc` is `#aabbcc`: each digit doubled, which 17 does.
        3 => Some(Rgb(
            channel(0, 1)? * 17,
            channel(1, 1)? * 17,
            channel(2, 1)? * 17,
        )),
        6 => Some(Rgb(channel(0, 2)?, channel(2, 2)?, channel(4, 2)?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hex_colours_in_either_length_and_either_case() {
        let cases = [
            ("#2a78d6", Some(Rgb(0x2a, 0x78, 0xd6))),
            ("#2A78D6", Some(Rgb(0x2a, 0x78, 0xd6))),
            ("#fff", Some(Rgb(0xff, 0xff, 0xff))),
            ("#0F8", Some(Rgb(0x00, 0xff, 0x88))),
            ("red", None),
            ("#2a78d", None),
            ("#2a78d6f", None),
            ("2a78d6", None),
            ("#rrggbb", None),
            ("", None),
            ("#", None),
            // Three bytes, but not three hex digits.
            ("#é8", None),
        ];
        for (text, expected) in cases {
            assert_eq!(hex_colour(text), expected, "{text}");
        }
    }

    #[test]
    fn a_named_colour_takes_the_palettes_place_only_at_its_own_index() {
        let named = [Rgb(0xe3, 0x49, 0x48)];
        assert_eq!(series_colour(0, &named, &SERIES_LIGHT), named[0]);
        assert_eq!(series_colour(0, &named, &SERIES_DARK), named[0]);
        // A list shorter than the series leaves the rest to each surface's own palette.
        assert_eq!(series_colour(1, &named, &SERIES_LIGHT), SERIES_LIGHT[1]);
        assert_eq!(series_colour(1, &named, &SERIES_DARK), SERIES_DARK[1]);
        // With none named, each surface keeps its own, stepping round past the eighth.
        assert_eq!(series_colour(8, &[], &SERIES_LIGHT), SERIES_LIGHT[0]);
        assert_eq!(series_colour(9, &[], &SERIES_DARK), SERIES_DARK[1]);
    }
}
