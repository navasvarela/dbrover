//! The `.sql` files consoles show: the recently opened ones and the data source each was
//! opened on, the ones being read, and when to look at them for changes other programs made.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dbrover_core::{DataSource, DataSourceId};
use dbrover_store::ConsoleKey;
use egui::{Align, Id, Layout, Modal};
use serde::{Deserialize, Serialize};

use crate::form;

/// How many files File › Open recent lists.
pub const RECENT_FILES: usize = 10;
/// How often open files are looked at for changes, besides whenever the window regains focus.
/// Two seconds feels immediate after switching back from another editor, and a check is one
/// `stat` per file.
pub const CHECK_EVERY: Duration = Duration::from_secs(2);
/// Where [`Remembered`] is kept in the app's saved state. Versioned like the dock layout.
pub const STORAGE_KEY: &str = "sql_files_v1";

/// A file opened lately, and the data source it was opened on, which it opens on again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentFile {
    pub path: PathBuf,
    pub data_source: DataSourceId,
}

/// What outlives the app, in its saved state next to the dock layout: the recent files, and
/// which file each console tab in the layout shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    pub recent: Vec<RecentFile>,
    pub open: Vec<(ConsoleKey, PathBuf)>,
}

/// Where a file being read is to be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// A new console on this data source.
    NewConsole(DataSourceId),
    /// The console tab of the last session's layout that showed it.
    Restored(ConsoleKey),
}

#[derive(Debug, Default)]
pub struct SqlFiles {
    recent: Vec<RecentFile>,
    /// Restored tabs and the files they show, until the files are asked for.
    restoring: Vec<(ConsoleKey, PathBuf)>,
    /// Files asked of the engine, and where each goes once read.
    reading: Vec<(PathBuf, Destination)>,
    last_check: Option<Instant>,
    /// Whether the window had focus last frame, to look at the files when it regains it.
    focused: bool,
}

impl SqlFiles {
    pub fn new(remembered: Remembered) -> Self {
        Self {
            recent: remembered.recent,
            restoring: remembered.open,
            ..Self::default()
        }
    }

    pub fn recent(&self) -> &[RecentFile] {
        &self.recent
    }

    /// The data source `path` was last opened on, if it still exists.
    pub fn data_source_of(
        &self,
        path: &Path,
        exists: impl Fn(DataSourceId) -> bool,
    ) -> Option<DataSourceId> {
        self.recent
            .iter()
            .find(|recent| recent.path == path)
            .map(|recent| recent.data_source)
            .filter(|id| exists(*id))
    }

    /// Puts `path` first in the recent files, to open on `data_source` next time.
    pub fn remember(&mut self, path: PathBuf, data_source: DataSourceId) {
        self.forget(&path);
        self.recent.insert(0, RecentFile { path, data_source });
        self.recent.truncate(RECENT_FILES);
    }

    pub fn forget(&mut self, path: &Path) {
        self.recent.retain(|recent| recent.path != path);
    }

    pub fn clear_recent(&mut self) {
        self.recent.clear();
    }

    /// The file a restored console tab shows, which is then read for it. `None` if the tab
    /// shows a console from the config folder.
    pub fn restore(&mut self, key: &ConsoleKey) -> Option<PathBuf> {
        let index = self.restoring.iter().position(|(tab, _)| tab == key)?;
        let (key, path) = self.restoring.swap_remove(index);
        self.reading
            .push((path.clone(), Destination::Restored(key)));
        Some(path)
    }

    /// Notes that `path` is being read for `destination`.
    pub fn reading(&mut self, path: PathBuf, destination: Destination) {
        self.reading.push((path, destination));
    }

    /// Where the file just read goes: every request for `path` is answered at once.
    pub fn read(&mut self, path: &Path) -> Vec<Destination> {
        let (answered, waiting) = std::mem::take(&mut self.reading)
            .into_iter()
            .partition(|(reading, _)| reading == path);
        self.reading = waiting;
        answered
            .into_iter()
            .map(|(_, destination)| destination)
            .collect()
    }

    /// Whether to look at the open files for changes now: every [`CHECK_EVERY`], and as soon
    /// as the window regains focus, after the user may have edited them elsewhere.
    pub fn check_due(&mut self, now: Instant, focused: bool) -> bool {
        let regained = focused && !self.focused;
        self.focused = focused;
        let due = regained
            || self
                .last_check
                .is_none_or(|last| now.saturating_duration_since(last) >= CHECK_EVERY);
        if due {
            self.last_check = Some(now);
        }
        due
    }

    /// What to save for the next launch, given the files the open consoles show.
    pub fn remembered<'a>(
        &self,
        open: impl Iterator<Item = (&'a ConsoleKey, &'a Path)>,
    ) -> Remembered {
        Remembered {
            recent: self.recent.clone(),
            open: open
                .map(|(key, path)| (key.clone(), path.to_owned()))
                .collect(),
        }
    }
}

/// The dialog File › Open… shows when it can't tell which data source to open a file on, or
/// when the file couldn't be read.
pub enum OpenFileDialog {
    ChooseDataSource {
        path: PathBuf,
        chosen: Option<DataSourceId>,
    },
    Failed {
        error: String,
    },
}

pub enum OpenFileAction {
    Open(PathBuf, DataSourceId),
    Close,
}

impl OpenFileDialog {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        data_sources: &[&DataSource],
    ) -> Option<OpenFileAction> {
        let modal = Modal::new(Id::new("open_file_dialog")).show(ctx, |ui| match self {
            Self::ChooseDataSource { path, chosen } => {
                choose_data_source(ui, path, chosen, data_sources)
            }
            Self::Failed { error } => {
                ui.heading("Couldn't open the file");
                ui.label(error.as_str());
                ui.separator();
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.button("Close")
                        .clicked()
                        .then_some(OpenFileAction::Close)
                })
                .inner
            }
        });
        if modal.should_close() {
            return Some(OpenFileAction::Close);
        }
        modal.inner
    }
}

fn choose_data_source(
    ui: &mut egui::Ui,
    path: &Path,
    chosen: &mut Option<DataSourceId>,
    data_sources: &[&DataSource],
) -> Option<OpenFileAction> {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    ui.heading(format!("Open {name}"));
    if data_sources.is_empty() {
        ui.label("There is no data source to run it on yet. Add one in the explorer first.");
    } else {
        ui.label("Which data source should its statements run on?");
        let selected = data_sources
            .iter()
            .find(|data_source| Some(data_source.id) == *chosen)
            .map_or("Choose…", |data_source| data_source.name.as_str());
        egui::ComboBox::from_label("Data source")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for data_source in data_sources {
                    ui.selectable_value(chosen, Some(data_source.id), &data_source.name);
                }
            });
    }
    ui.separator();
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let open = form::primary_button(ui, "Open", chosen.is_some());
        if open.clicked()
            && let Some(id) = *chosen
        {
            Some(OpenFileAction::Open(path.to_owned(), id))
        } else if ui.button("Cancel").clicked() {
            Some(OpenFileAction::Close)
        } else {
            None
        }
    })
    .inner
}

#[cfg(test)]
mod tests {
    use dbrover_store::ConsoleName;

    use super::*;

    #[test]
    fn keeps_the_last_ten_files_newest_first_without_repeats() {
        let source = DataSourceId::generate();
        let other = DataSourceId::generate();
        let mut files = SqlFiles::default();
        for n in 0..12 {
            files.remember(PathBuf::from(format!("/work/{n}.sql")), source);
        }
        files.remember(PathBuf::from("/work/5.sql"), other);

        let paths: Vec<_> = files
            .recent()
            .iter()
            .map(|recent| recent.path.clone())
            .collect();
        assert_eq!(paths.len(), RECENT_FILES);
        assert_eq!(paths[0], PathBuf::from("/work/5.sql"));
        assert_eq!(paths[1], PathBuf::from("/work/11.sql"));
        assert_eq!(
            paths.iter().filter(|path| path.ends_with("5.sql")).count(),
            1
        );
        let path = Path::new("/work/5.sql");
        assert_eq!(files.data_source_of(path, |_| true), Some(other));
        assert_eq!(files.data_source_of(path, |id| id != other), None);
    }

    #[test]
    fn answers_every_request_for_a_file_once_it_is_read() {
        let key = ConsoleKey {
            data_source: DataSourceId::generate(),
            name: ConsoleName::try_from("file_1".to_owned()).unwrap(),
        };
        let path = PathBuf::from("/work/report.sql");
        let mut files = SqlFiles::new(Remembered {
            recent: Vec::new(),
            open: vec![(key.clone(), path.clone())],
        });
        let source = DataSourceId::generate();

        assert_eq!(files.restore(&key), Some(path.clone()));
        assert_eq!(files.restore(&key), None, "restored once");
        files.reading(path.clone(), Destination::NewConsole(source));
        files.reading("/work/other.sql".into(), Destination::NewConsole(source));

        assert_eq!(
            files.read(&path),
            [Destination::Restored(key), Destination::NewConsole(source)]
        );
        assert!(files.read(&path).is_empty());
    }

    #[test]
    fn checks_every_two_seconds_and_when_the_window_regains_focus() {
        let mut files = SqlFiles::default();
        let start = Instant::now();

        assert!(files.check_due(start, true));
        assert!(!files.check_due(start + CHECK_EVERY / 2, true));
        assert!(!files.check_due(start + CHECK_EVERY / 2, false));
        assert!(
            files.check_due(start + CHECK_EVERY / 2, true),
            "focus regained"
        );
        assert!(files.check_due(start + CHECK_EVERY * 2, true));
    }
}
