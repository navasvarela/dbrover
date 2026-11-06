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
    pub fn of(&self, paint: Paint) -> Rgb {
        match paint {
            Paint::Series(index) => self.series[index % self.series.len()],
            Paint::Axis => self.axis,
            Paint::Grid => self.grid,
            Paint::Label => self.label,
            Paint::Title => self.title,
        }
    }
}
