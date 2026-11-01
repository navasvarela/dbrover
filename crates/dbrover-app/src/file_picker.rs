//! The platform's open and save dialogs, for choosing an existing database file (SQLite or
//! DuckDB) or naming a new one, for choosing a CA certificate, for exporting and importing
//! data sources, and for opening and saving SQL files.

use std::path::{Path, PathBuf};

use dbrover_engine::DatabaseFile;

use crate::picture::PictureFormat;

const CA_TITLE: &str = "Choose a CA certificate (PEM)";
const SSH_KEY_TITLE: &str = "Choose an SSH private key";
const EXPORT_TITLE: &str = "Export data sources";
const IMPORT_TITLE: &str = "Import data sources";
const CSV_TITLE: &str = "Choose a CSV file to import";
const OPEN_SQL_TITLE: &str = "Open a SQL file";
const SAVE_SQL_TITLE: &str = "Save the console as";
const EXPORT_FILE_NAME: &str = "dbrover-data-sources.toml";

/// An engine whose databases are files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEngine {
    Sqlite,
    DuckDb,
}

impl FileEngine {
    fn open_title(self) -> &'static str {
        match self {
            Self::Sqlite => "Choose a SQLite database",
            Self::DuckDb => "Choose a DuckDB database",
        }
    }

    fn new_title(self) -> &'static str {
        match self {
            Self::Sqlite => "Create a SQLite database",
            Self::DuckDb => "Create a DuckDB database",
        }
    }

    /// What the Save dialog suggests for a new database.
    fn new_file_name(self) -> &'static str {
        match self {
            Self::Sqlite => "database.db",
            Self::DuckDb => "database.duckdb",
        }
    }

    /// The new database file at `path`, for the engine to create.
    pub fn file(self, path: PathBuf) -> DatabaseFile {
        match self {
            Self::Sqlite => DatabaseFile::Sqlite(path),
            Self::DuckDb => DatabaseFile::DuckDb(path),
        }
    }
}

/// Shows the open-file dialog and returns the chosen file, or `None` if it was cancelled.
///
/// Blocks the UI thread until the dialog closes, unlike everything else in the UI (see
/// ADR-0003). That is deliberate: the dialog is modal anyway, macOS requires it to run on the
/// main thread, and rfd's async variant would need its future polled from egui's frame loop
/// for no visible difference.
///
/// No extension filter: SQLite files are named `.db`, `.sqlite`, `.sqlite3`, `.db3`, or have
/// no extension at all (DuckDB's `.duckdb` or `.db`), and on macOS rfd merges all filters into
/// one allowed list, so an "All files" escape hatch isn't possible. The connection test catches
/// files of another kind.
pub fn pick_database_file(engine: FileEngine, current: &str) -> Option<PathBuf> {
    dialog(engine.open_title(), current).pick_file()
}

/// Shows the save-file dialog for a new database and returns the chosen path, or `None` if it
/// was cancelled. Nothing is created here; the engine creates the file, and refuses if it
/// already exists, even if the dialog asked "Replace?".
pub fn pick_new_database_file(engine: FileEngine, current: &str) -> Option<PathBuf> {
    dialog(engine.new_title(), current)
        .set_file_name(engine.new_file_name())
        .save_file()
}

/// Shows the open-file dialog for a PEM file of trusted authorities. No extension filter, for
/// the same reason as [`pick_database_file`]: they come as `.pem`, `.crt` and `.cer`.
pub fn pick_ca_file(current: &str) -> Option<PathBuf> {
    dialog(CA_TITLE, current).pick_file()
}

/// Shows the open-file dialog for an SSH private key, in `~/.ssh` unless the field names a
/// file elsewhere. No extension filter: OpenSSH's keys have none (`id_ed25519`).
pub fn pick_ssh_key_file(current: &str) -> Option<PathBuf> {
    let ssh_folder = std::env::home_dir()
        .map(|home| home.join(".ssh"))
        .filter(|folder| folder.is_dir());
    match (starting_directory(current), ssh_folder) {
        (None, Some(folder)) => rfd::FileDialog::new()
            .set_title(SSH_KEY_TITLE)
            .set_directory(folder)
            .pick_file(),
        _ => dialog(SSH_KEY_TITLE, current).pick_file(),
    }
}

/// Shows the save dialog for an export of data sources.
pub fn pick_data_sources_export() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(EXPORT_TITLE)
        .add_filter("TOML", &["toml"])
        .set_file_name(EXPORT_FILE_NAME)
        .save_file()
}

/// Shows the open dialog for a file of data sources to import.
pub fn pick_data_sources_import() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(IMPORT_TITLE)
        .add_filter("TOML", &["toml"])
        .pick_file()
}

fn dialog(title: &str, current: &str) -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new().set_title(title);
    match starting_directory(current) {
        Some(directory) => dialog.set_directory(directory),
        None => dialog,
    }
}

/// Where the dialog opens: the folder of the path already in the field, when that folder
/// exists. Otherwise the platform's default (usually the last folder used).
fn starting_directory(current: &str) -> Option<PathBuf> {
    let path = Path::new(current.trim());
    if path.as_os_str().is_empty() {
        return None;
    }
    if path.is_dir() {
        return Some(path.to_owned());
    }
    path.parent()
        .filter(|parent| parent.is_dir())
        .map(Path::to_path_buf)
}

/// Shows the folder dialog for a settings folder, starting beside `current`, or `None` if it
/// was cancelled.
pub fn pick_settings_folder(current: &Path) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Choose a settings folder");
    if let Some(parent) = current.parent().filter(|parent| parent.is_dir()) {
        dialog = dialog.set_directory(parent);
    }
    dialog.pick_folder()
}

/// Shows the open-file dialog for a CSV file to import into a table, or `None` if it was
/// cancelled. Files of any extension can be chosen: tab-separated files are often `.tsv` or
/// `.txt`.
pub fn pick_csv_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(CSV_TITLE)
        .add_filter("CSV", &["csv", "tsv", "txt"])
        .add_filter("All files", &["*"])
        .pick_file()
}

/// Shows the open-file dialog for a SQL file to open in a console. Files of any extension can
/// be chosen, as with CSV files: scripts are also kept as `.txt` or `.ddl`.
pub fn pick_sql_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(OPEN_SQL_TITLE)
        .add_filter("SQL", &["sql"])
        .add_filter("All files", &["*"])
        .pick_file()
}

/// Shows the save dialog for a console's text, suggesting `file_name`.
pub fn pick_sql_save_path(file_name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(SAVE_SQL_TITLE)
        .add_filter("SQL", &["sql"])
        .set_file_name(file_name)
        .save_file()
}

/// Shows the save dialog for a picture, such as a diagram's, suggesting `name` with the format's
/// extension, and returns the chosen path, or `None` if it was cancelled.
pub fn pick_picture(name: &str, format: PictureFormat) -> Option<PathBuf> {
    let extension = format.extension();
    rfd::FileDialog::new()
        .set_title(format!("Export {name} as {}", format.name()))
        .add_filter(format.name(), &[extension])
        .set_file_name(format!("{name}.{extension}"))
        .save_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_in_the_folder_of_the_current_path_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("shop.db");
        let file_text = file.display().to_string();
        let missing_folder = dir.path().join("gone/shop.db").display().to_string();

        let cases = [
            ("", None),
            ("   ", None),
            (file_text.as_str(), Some(dir.path().to_owned())),
            (
                &dir.path().display().to_string(),
                Some(dir.path().to_owned()),
            ),
            (missing_folder.as_str(), None),
        ];
        for (current, expected) in cases {
            assert_eq!(starting_directory(current), expected, "{current:?}");
        }
    }
}
