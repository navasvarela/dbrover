//! Formats of the user's own, from `exports.toml` (W8): what each one writes and how it is
//! told apart. The store reads the file; this crate says what a preset's options are, so the
//! dialog, the clipboard and the file agree on them. See `docs/exports.md`.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::text::{CsvOptions, HtmlOptions, InsertOptions, MarkdownOptions, RowSource, TsvOptions};
use crate::{ExportFormat, ExportOptions, ExportSpec, ParquetOptions, XlsxOptions};

/// A custom format's stable name: what `keymap.toml` binds as `export.<id>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CustomFormatId(String);

impl CustomFormatId {
    /// The prefix that keeps a custom format's keys apart from the built-in
    /// `grid.copy-as.<format>`, so a custom format named `csv` can't take CSV's.
    pub const KEYMAP_PREFIX: &str = "export.";

    /// Lowercase letters, digits, `-` and `_`, starting with a letter or digit, as an action's
    /// id is.
    pub fn new(id: &str) -> Option<Self> {
        let mut chars = id.chars();
        let valid = chars
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        valid.then(|| Self(id.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `export.<id>`.
    pub fn keymap_id(&self) -> String {
        format!("{}{}", Self::KEYMAP_PREFIX, self.0)
    }
}

impl fmt::Display for CustomFormatId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A format from `exports.toml`, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomFormat {
    pub id: CustomFormatId,
    pub name: String,
    /// What it writes, for a tooltip.
    pub description: Option<String>,
    /// The file name extension, without its dot.
    pub extension: String,
    pub kind: CustomFormatKind,
}

/// What a custom format is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomFormatKind {
    /// A built-in format with options of its own.
    Preset(Preset),
}

impl CustomFormat {
    /// The format to write rows from `source` in.
    ///
    /// # Errors
    ///
    /// Why the format can't write these rows, as a built-in format's would say.
    pub fn spec(&self, source: &RowSource) -> Result<ExportSpec, &'static str> {
        match &self.kind {
            CustomFormatKind::Preset(preset) => preset.options.spec(preset.format, source),
        }
    }

    /// Whether it writes text, which can go on the clipboard, rather than only files.
    pub fn writes_text(&self) -> bool {
        match &self.kind {
            CustomFormatKind::Preset(preset) => preset.format.writes_text(),
        }
    }

    /// The built-in format it writes, for the rules that depend on one: which rows it can
    /// write, and its icon.
    pub fn builtin(&self) -> ExportFormat {
        match &self.kind {
            CustomFormatKind::Preset(preset) => preset.format,
        }
    }

    /// What it writes, for a tooltip: its description, or the format it is a preset of.
    pub fn summary(&self) -> String {
        match (&self.description, &self.kind) {
            (Some(description), _) => description.clone(),
            (None, CustomFormatKind::Preset(preset)) => {
                format!("{} with options from exports.toml", preset.format.name())
            }
        }
    }
}

/// A built-in format with its options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub format: ExportFormat,
    /// Only `format`'s field is read; every other is its default.
    pub options: ExportOptions,
}

impl Preset {
    /// `format` with the options `options` holds, each named as `app.ron` names it, and the
    /// default for each it leaves out.
    ///
    /// # Errors
    ///
    /// What `options` gets wrong: a value of the wrong type, or one an option doesn't take.
    /// Options `format` doesn't have aren't checked here: see [`Preset::option_names`].
    pub fn read<'de, D: Deserializer<'de>>(
        format: ExportFormat,
        options: D,
    ) -> Result<Self, D::Error> {
        let mut all = ExportOptions::default();
        match format {
            ExportFormat::Csv => all.csv = CsvOptions::deserialize(options)?,
            ExportFormat::Xlsx => all.xlsx = XlsxOptions::deserialize(options)?,
            ExportFormat::Tsv => all.tsv = TsvOptions::deserialize(options)?,
            ExportFormat::Markdown => all.markdown = MarkdownOptions::deserialize(options)?,
            ExportFormat::SqlInsert => all.insert = InsertOptions::deserialize(options)?,
            ExportFormat::Parquet => all.parquet = ParquetOptions::deserialize(options)?,
            ExportFormat::Html => all.html = HtmlOptions::deserialize(options)?,
            ExportFormat::Json | ExportFormat::JsonLines | ExportFormat::SqlUpdate => {
                serde::de::IgnoredAny::deserialize(options)?;
            }
        }
        Ok(Self {
            format,
            options: all,
        })
    }

    /// The options `format` has, as a preset writes them: the names its options are saved by.
    pub fn option_names(format: ExportFormat) -> Vec<String> {
        options_object(format, &ExportOptions::default())
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    /// The options this preset sets to something other than the default, each as
    /// `exports.toml` would write its value (`";"`, `true`), in the format's order.
    pub fn changed_options(&self) -> Vec<(String, String)> {
        let defaults = options_object(self.format, &ExportOptions::default());
        options_object(self.format, &self.options)
            .into_iter()
            .filter(|option| !defaults.contains(option))
            .map(|(name, value)| (name, value.to_string()))
            .collect()
    }
}

/// `format`'s options among `options`, by name, in the order its type declares them; empty for
/// a format without options.
fn options_object(
    format: ExportFormat,
    options: &ExportOptions,
) -> Vec<(String, serde_json::Value)> {
    let value = match format {
        ExportFormat::Csv => serde_json::to_value(&options.csv),
        ExportFormat::Xlsx => serde_json::to_value(&options.xlsx),
        ExportFormat::Tsv => serde_json::to_value(&options.tsv),
        ExportFormat::Markdown => serde_json::to_value(&options.markdown),
        ExportFormat::SqlInsert => serde_json::to_value(options.insert),
        ExportFormat::Parquet => serde_json::to_value(options.parquet),
        ExportFormat::Html => serde_json::to_value(&options.html),
        ExportFormat::Json | ExportFormat::JsonLines | ExportFormat::SqlUpdate => {
            return Vec::new();
        }
    };
    match value {
        Ok(serde_json::Value::Object(options)) => options.into_iter().collect(),
        // Every options type is a struct of plain fields, which serialize as an object.
        _ => Vec::new(),
    }
}
