//! What `actions.toml`, `snippets.toml` and `theme.toml` hold, as the engine last read them, and
//! the window that lists a file's problems. See `docs/hackability.md`.

use std::path::{Path, PathBuf};

use dbrover_engine::Event;
use dbrover_store::{
    Checked, CustomAction, CustomFile, CustomFileKind, Snippet, ThemeOverrides, ThemeToken,
};
use egui::{ScrollArea, Window};

use crate::theme::Palettes;

const WINDOW_SIZE: [f32; 2] = [560.0, 280.0];

#[derive(Debug, Default)]
pub struct CustomFiles {
    actions: Loaded<Vec<CustomAction>>,
    snippets: Loaded<Vec<Snippet>>,
    theme: Loaded<ThemeOverrides>,
    /// The palettes `theme.toml` makes.
    palettes: Palettes,
    /// The file whose problems window is open.
    pub window: Option<CustomFileKind>,
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
        }
        Some(file.kind())
    }

    pub fn actions(&self) -> &[CustomAction] {
        &self.actions.checked.value
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
        }
    }

    pub fn path(&self, kind: CustomFileKind) -> Option<&Path> {
        match kind {
            CustomFileKind::Actions => self.actions.path.as_deref(),
            CustomFileKind::Snippets => self.snippets.path.as_deref(),
            CustomFileKind::Theme => self.theme.path.as_deref(),
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
        }
    }
}

fn count(n: usize, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

/// Every colour token's name, for the theme window's hint.
fn token_names() -> String {
    ThemeToken::ALL
        .into_iter()
        .map(ThemeToken::name)
        .collect::<Vec<_>>()
        .join(", ")
}

pub enum FileWindowAction {
    Reload,
    Close,
}

/// Draws the problems window of the file in [`CustomFiles::window`], if one is open.
pub fn show(ctx: &egui::Context, files: &CustomFiles) -> Option<FileWindowAction> {
    let kind = files.window?;
    let mut open = true;
    let action = Window::new(kind.file_name())
        .id(egui::Id::new(("custom_file_window", kind.file_name())))
        .open(&mut open)
        .default_size(WINDOW_SIZE)
        .show(ctx, |ui| contents(ui, files, kind))
        .and_then(|response| response.inner.flatten());
    if open {
        action
    } else {
        Some(FileWindowAction::Close)
    }
}

fn contents(
    ui: &mut egui::Ui,
    files: &CustomFiles,
    kind: CustomFileKind,
) -> Option<FileWindowAction> {
    let reload = header(ui, files.path(kind), &files.summary(kind));
    if kind == CustomFileKind::Theme {
        ui.weak(format!("Colours: {}", token_names()));
    }
    ui.separator();
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        problems(ui, files.problems(kind));
    });
    reload.then_some(FileWindowAction::Reload)
}

/// The file's path, what it adds, and a Reload button, which this returns whether was clicked.
fn header(ui: &mut egui::Ui, path: Option<&Path>, summary: &str) -> bool {
    let mut reload = false;
    ui.horizontal(|ui| {
        if let Some(path) = path {
            ui.weak(path.display().to_string());
        }
        reload = ui
            .button("Reload")
            .on_hover_text("Read actions.toml, snippets.toml and theme.toml again")
            .clicked();
    });
    ui.label(summary);
    reload
}

/// What couldn't be used, or that there is nothing wrong.
fn problems(ui: &mut egui::Ui, problems: &[String]) {
    if problems.is_empty() {
        ui.weak("No problems.");
    }
    for problem in problems {
        ui.colored_label(ui.visuals().error_fg_color, problem);
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
