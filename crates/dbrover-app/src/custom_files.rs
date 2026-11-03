//! What `actions.toml`, `snippets.toml`, `theme.toml` and `exports.toml` hold, as the engine
//! last read them. The Settings tab lists each file's problems. See `docs/hackability.md`.

use std::path::{Path, PathBuf};

use crate::theme::Palettes;
use dbrover_engine::Event;
use dbrover_export::CustomFormat;
use dbrover_store::{
    Checked, CustomAction, CustomFile, CustomFileKind, Snippet, ThemeOverrides, ThemeToken,
};

/// What **Reload** reads, for its tooltip wherever it is.
pub const RELOAD_HINT: &str = "Read actions.toml, snippets.toml, theme.toml and exports.toml again";

#[derive(Debug, Default)]
pub struct CustomFiles {
    actions: Loaded<Vec<CustomAction>>,
    snippets: Loaded<Vec<Snippet>>,
    theme: Loaded<ThemeOverrides>,
    exports: Loaded<Vec<CustomFormat>>,
    /// The palettes `theme.toml` makes.
    palettes: Palettes,
}

/// A file as last read: until the engine first reports it, empty and with no path.
#[derive(Debug, Default)]
struct Loaded<T> {
    path: Option<PathBuf>,
    checked: Checked<T>,
}

impl<T> Loaded<T> {
    fn set(&mut self, path: &Path, checked: &Checked<T>)
    where
        T: Clone,
    {
        self.path = Some(path.to_owned());
        self.checked = checked.clone();
    }
}

impl CustomFiles {
    /// Takes in a file the engine read. Returns which, so what depends on it can follow.
    pub fn apply(&mut self, event: &Event) -> Option<CustomFileKind> {
        let Event::CustomFileLoaded { path, file } = event else {
            return None;
        };
        match file {
            CustomFile::Actions(checked) => self.actions.set(path, checked),
            CustomFile::Snippets(checked) => self.snippets.set(path, checked),
            CustomFile::Theme(checked) => {
                self.theme.set(path, checked);
                self.palettes = Palettes::with_overrides(&checked.value);
                let unreadable = self.palettes.contrast_problems();
                self.theme.checked.problems.extend(unreadable);
            }
            CustomFile::Exports(checked) => self.exports.set(path, checked),
        }
        Some(file.kind())
    }

    pub fn actions(&self) -> &[CustomAction] {
        &self.actions.checked.value
    }

    /// The formats `exports.toml` adds, in the file's order.
    pub fn exports(&self) -> &[CustomFormat] {
        &self.exports.checked.value
    }

    pub fn palettes(&self) -> Palettes {
        self.palettes
    }

    pub fn snippets(&self) -> &[Snippet] {
        &self.snippets.checked.value
    }

    pub fn problems(&self, kind: CustomFileKind) -> &[String] {
        match kind {
            CustomFileKind::Actions => &self.actions.checked.problems,
            CustomFileKind::Snippets => &self.snippets.checked.problems,
            CustomFileKind::Theme => &self.theme.checked.problems,
            CustomFileKind::Exports => &self.exports.checked.problems,
        }
    }

    pub fn path(&self, kind: CustomFileKind) -> Option<&Path> {
        match kind {
            CustomFileKind::Actions => self.actions.path.as_deref(),
            CustomFileKind::Snippets => self.snippets.path.as_deref(),
            CustomFileKind::Theme => self.theme.path.as_deref(),
            CustomFileKind::Exports => self.exports.path.as_deref(),
        }
    }

    /// What the file adds, in a few words.
    pub fn summary(&self, kind: CustomFileKind) -> String {
        match kind {
            CustomFileKind::Actions => count(self.actions.checked.value.len(), "action"),
            CustomFileKind::Snippets => count(self.snippets.checked.value.len(), "snippet"),
            CustomFileKind::Theme => {
                let theme = &self.theme.checked.value;
                format!(
                    "{} for the light theme, {} for the dark one",
                    count(theme.light.len(), "colour"),
                    count(theme.dark.len(), "colour")
                )
            }
            CustomFileKind::Exports => count(self.exports.checked.value.len(), "format"),
        }
    }
}

fn count(n: usize, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

/// What `kind`'s file may name that its summary doesn't say: for `theme.toml`, every colour
/// token, shown beside its problems.
pub fn reference(kind: CustomFileKind) -> Option<String> {
    match kind {
        CustomFileKind::Theme => {
            let tokens: Vec<&str> = ThemeToken::ALL.into_iter().map(ThemeToken::name).collect();
            Some(format!("Colours: {}", tokens.join(", ")))
        }
        CustomFileKind::Actions | CustomFileKind::Snippets | CustomFileKind::Exports => None,
    }
}

#[cfg(test)]
mod tests {
    use dbrover_store::parse_theme;

    use super::*;

    #[test]
    fn keeps_each_file_as_last_read_and_says_which_changed() {
        let mut files = CustomFiles::default();
        let theme = parse_theme("[dark]\nprimary = \"#9a8bff\"\n[light]\nprimray = \"#000000\"\n");
        let changed = files.apply(&Event::CustomFileLoaded {
            path: "theme.toml".into(),
            file: CustomFile::Theme(theme),
        });

        assert_eq!(changed, Some(CustomFileKind::Theme));
        assert_eq!(files.problems(CustomFileKind::Theme).len(), 1);
        assert!(files.problems(CustomFileKind::Actions).is_empty());
        assert_eq!(
            files.summary(CustomFileKind::Theme),
            "0 colours for the light theme, 1 colour for the dark one"
        );
        assert_eq!(
            files.path(CustomFileKind::Theme),
            Some(Path::new("theme.toml"))
        );
        assert_eq!(files.apply(&Event::Started), None);
    }
}
