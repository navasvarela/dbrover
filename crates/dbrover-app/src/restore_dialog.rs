//! Restore…: a dump file read first, then where it goes, what of it and how, and a question
//! every time before anything is written (W17, `docs/dump-restore.md`). Into a new database by
//! default; into an existing one of a careful data source, only once its name is typed.

use std::path::{Path, PathBuf};

use dbrover_core::{
    DataSourceId, DumpEngine, RestoreOptions, RestoreRequest, RestoreTarget, SafetyLevel, TableName,
};
use dbrover_dump::{DumpFileKind, Role};
use dbrover_engine::{Command, DumpFileFound, Event, ProgramPaths, ProgramQuery};
use egui::{Id, RichText, ScrollArea, Ui};

use crate::dialog::{self, DialogSize};
use crate::dump_dialog::ProgramState;
use crate::form::{self, Segment};
use crate::icons;
use crate::state::DataSourceEntry;
use crate::theme::Palette;
use crate::widgets;

pub enum RestoreAction {
    Start(RestoreRequest),
    Browse,
    Close,
}

enum FileState {
    Reading,
    Unreadable(String),
    Read(DumpFileFound),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Into {
    NewDatabase,
    Existing,
    /// SQLite's and DuckDB's: a new database file, never one that exists.
    NewFile,
}

pub struct RestoreDialog {
    data_source: DataSourceId,
    name: String,
    safety: SafetyLevel,
    engine: DumpEngine,
    server: String,
    programs: ProgramPaths,
    file: PathBuf,
    state: FileState,
    query: Option<ProgramQuery>,
    program: ProgramState,
    into: Into,
    new_name: String,
    existing_name: String,
    /// An archive's tables, each ticked or not.
    tables: Vec<(TableName, bool)>,
    options: RestoreOptions,
    /// The question before anything runs, with the name typed so far where one is asked for.
    confirming: Option<String>,
}

impl RestoreDialog {
    /// The dialog restoring `file` into the connected data source `entry`, into `database`
    /// when one was right-clicked, and the command that reads the file.
    pub fn open(
        entry: &DataSourceEntry,
        database: Option<String>,
        file: PathBuf,
        programs: ProgramPaths,
    ) -> Option<(Self, Command)> {
        let engine = entry.dump_engine()?;
        let itself = !engine.runs_a_program();
        let dbrover_engine::SessionState::Connected(server) = &entry.session else {
            return None;
        };
        let own = entry
            .data_source
            .config
            .database()
            .unwrap_or_default()
            .to_owned();
        let command = Command::InspectDumpFile {
            file: file.clone(),
            programs: programs.clone(),
        };
        let dialog = Self {
            data_source: entry.data_source.id,
            name: entry.data_source.name.clone(),
            safety: entry.data_source.safety,
            engine,
            server: server.version.clone(),
            programs,
            new_name: if itself {
                restored_file(&file, engine)
            } else {
                format!("{}_restored", stem(&file))
            },
            file,
            state: FileState::Reading,
            query: None,
            program: ProgramState::for_engine(engine),
            into: if database.is_some() && !itself {
                Into::Existing
            } else {
                Into::NewDatabase
            },
            existing_name: database.unwrap_or(own),
            tables: Vec::new(),
            options: RestoreOptions::default(),
            confirming: None,
        };
        Some((dialog, command))
    }

    /// Takes in what the engine read of the file, and asks which program will restore it.
    pub fn apply(&mut self, event: &Event) -> Option<Command> {
        if let Some(query) = &self.query {
            self.program.apply(query, event);
        }
        let Event::DumpFileInspected { file, result } = event else {
            return None;
        };
        if *file != self.file {
            return None;
        }
        match result {
            Ok(found) if !self.engine.runs_a_program() => {
                self.state = FileState::Read(found.clone());
                None
            }
            Ok(found) => {
                if let Some(database) = found.contents.as_ref().and_then(|c| c.database.as_ref()) {
                    self.new_name = format!("{database}_restored");
                }
                self.tables = found
                    .contents
                    .iter()
                    .flat_map(|contents| contents.tables.iter().cloned())
                    .map(|table| (table, true))
                    .collect();
                let query = ProgramQuery {
                    engine: self.engine,
                    role: if found.kind.is_archive() {
                        Role::RestoreArchive
                    } else {
                        Role::RunSql
                    },
                    server: self.server.clone(),
                    archive_made_by: found.contents.as_ref().and_then(|c| c.made_by),
                    programs: self.programs.clone(),
                };
                self.state = FileState::Read(found.clone());
                self.program = ProgramState::Choosing;
                self.query = Some(query.clone());
                Some(Command::ChooseDumpProgram(query))
            }
            Err(why) => {
                self.state = FileState::Unreadable(why.clone());
                None
            }
        }
    }

    pub fn data_source(&self) -> DataSourceId {
        self.data_source
    }

    /// The existing database chosen, to keep when another file is chosen.
    pub fn chosen_database(&self) -> Option<String> {
        (self.into == Into::Existing).then(|| self.existing_name.clone())
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<RestoreAction> {
        let modal =
            dialog::modal(ctx, Id::new("restore_dialog"), DialogSize::Wide).show(ctx, |ui| {
                match self.confirming.take() {
                    Some(typed) => self.confirm(ui, typed),
                    None => self.contents(ui),
                }
            });
        if modal.should_close() {
            return Some(RestoreAction::Close);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut Ui) -> Option<RestoreAction> {
        let palette = &Palette::for_visuals(ui.visuals());
        form::Header::new(icons::UPLOAD_SIMPLE, &format!("Restore into {}", self.name))
            .subtitle(&self.file.display().to_string())
            .show(ui);
        let mut browse = false;
        form::card(ui, |rows| {
            rows.row("File", None, |ui| {
                browse = ui.button("Browse…").clicked();
            });
            rows.full(|ui| self.file_summary(ui, palette));
            if !matches!(self.state, FileState::Read(_)) {
                return;
            }
            let itself = !self.engine.runs_a_program();
            if !itself {
                rows.row("Into", None, |ui| {
                    form::segmented(
                        ui,
                        &mut self.into,
                        &[
                            Segment::new(Into::NewDatabase, "A new database"),
                            Segment::new(Into::Existing, "An existing database"),
                        ],
                    );
                });
            }
            let (label, name) = match (self.into, itself) {
                (Into::NewDatabase | Into::NewFile, true) => ("New file", &mut self.new_name),
                (Into::NewDatabase | Into::NewFile, false) => ("New database", &mut self.new_name),
                (Into::Existing, _) => ("Database", &mut self.existing_name),
            };
            rows.row(label, None, |ui| {
                widgets::name(
                    ui.text_edit_singleline(name),
                    egui::WidgetType::TextEdit,
                    label,
                    true,
                );
            });
            if !self.tables.is_empty() {
                rows.full(|ui| tables(ui, &mut self.tables));
            }
            if itself {
                rows.full(|ui| self.program.show(ui, &self.server, palette));
                return;
            }
            rows.row(
                "Leave out owners and privileges",
                Some(
                    "The restoring login owns everything, as on a server without the dump's roles.",
                ),
                |ui| {
                    form::toggle(
                        ui,
                        &mut self.options.leave_out_owners,
                        "Leave out owners and privileges",
                    );
                },
            );
            if self.engine == DumpEngine::Postgres {
                rows.row(
                    "All or nothing",
                    Some("One transaction: a restore that fails leaves nothing behind."),
                    |ui| {
                        form::toggle(ui, &mut self.options.all_or_nothing, "All or nothing");
                    },
                );
            }
            if self.into == Into::Existing && self.archive() {
                rows.row(
                    "Drop each object first",
                    Some(
                        "Every object in the dump is dropped before it is created, with its data.",
                    ),
                    |ui| {
                        form::toggle(ui, &mut self.options.drop_first, "Drop each object first");
                    },
                );
            }
            rows.full(|ui| self.program.show(ui, &self.server, palette));
        });
        if browse {
            return Some(RestoreAction::Browse);
        }
        let restorable =
            matches!(&self.state, FileState::Read(found) if found.kind.restores_into(self.engine));
        let some_tables = self.tables.is_empty() || self.tables.iter().any(|(_, ticked)| *ticked);
        let ready = restorable
            && self.program.is_ready()
            && !self.database().trim().is_empty()
            && some_tables;
        let (restore, cancel) = form::footer(ui, |ui| {
            let restore = form::primary_button(ui, "Restore…", ready).clicked();
            (restore, ui.button("Cancel").clicked())
        });
        if cancel {
            return Some(RestoreAction::Close);
        }
        if restore {
            self.confirming = Some(String::new());
        }
        None
    }

    fn file_summary(&self, ui: &mut Ui, palette: &Palette) {
        match &self.state {
            FileState::Reading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading the file…");
                });
            }
            FileState::Unreadable(why) => {
                widgets::status(ui, icons::WARNING, palette.error, why);
            }
            FileState::Read(found) => {
                ui.label(describe(found));
                if let Some(contents) = &found.contents {
                    let made = [
                        contents
                            .made_by
                            .map(|version| format!("made by pg_dump {version}")),
                        contents
                            .server
                            .map(|version| format!("from PostgreSQL {version}")),
                    ];
                    let made: Vec<String> = made.into_iter().flatten().collect();
                    if !made.is_empty() {
                        form::description_label(ui, &made.join(" "));
                    }
                }
                if !found.kind.restores_into(self.engine) {
                    let file = found
                        .kind
                        .engine()
                        .map_or("another".to_owned(), |e| e.to_string());
                    widgets::status(
                        ui,
                        icons::WARNING,
                        palette.error,
                        &format!("A {file} dump can't be restored into {}.", self.engine),
                    );
                }
            }
        }
    }

    fn archive(&self) -> bool {
        matches!(&self.state, FileState::Read(found) if found.kind.is_archive())
    }

    fn database(&self) -> &str {
        match self.into {
            Into::NewDatabase | Into::NewFile => &self.new_name,
            Into::Existing => &self.existing_name,
        }
    }

    /// Whether the user must type the database's name: into an existing database of a careful
    /// data source, where a restore can't be undone.
    fn asks_for_name(&self) -> bool {
        self.into == Into::Existing && self.safety == SafetyLevel::Careful
    }

    /// The question asked before every restore, naming the file, the database and the data
    /// source.
    fn confirm(&mut self, ui: &mut Ui, mut typed: String) -> Option<RestoreAction> {
        let database = self.database().trim().to_owned();
        form::Header::new(icons::WARNING, "Restore?")
            .danger()
            .show(ui);
        let into = if self.engine.runs_a_program() {
            self.into
        } else {
            Into::NewFile
        };
        ui.label(question(
            &self.file,
            into,
            &database,
            &self.name,
            self.options.drop_first && self.archive(),
        ));
        if self.asks_for_name() {
            ui.add_space(ui.spacing().item_spacing.y);
            ui.label(format!(
                "\"{}\" is careful. Type {database} to restore into it.",
                self.name
            ));
            widgets::name(
                ui.text_edit_singleline(&mut typed),
                egui::WidgetType::TextEdit,
                "Database name",
                true,
            );
        }
        let named = !self.asks_for_name() || typed.trim() == database;
        let (restore, back) = form::footer(ui, |ui| {
            let restore = form::danger_button(ui, "Restore", named).clicked();
            (restore, ui.button("Back").clicked())
        });
        if back {
            return None;
        }
        if restore {
            return Some(RestoreAction::Start(self.request(database)));
        }
        self.confirming = Some(typed);
        None
    }

    fn request(&self, database: String) -> RestoreRequest {
        let all = self.tables.iter().all(|(_, ticked)| *ticked);
        let tables = (!all).then(|| {
            self.tables
                .iter()
                .filter(|(_, ticked)| *ticked)
                .map(|(table, _)| table.clone())
                .collect()
        });
        let mut options = self.options;
        options.drop_first &= self.into == Into::Existing;
        RestoreRequest {
            data_source: self.data_source,
            file: self.file.clone(),
            target: match self.into {
                Into::NewDatabase | Into::NewFile => RestoreTarget::NewDatabase(database),
                Into::Existing => RestoreTarget::Existing(database),
            },
            tables,
            options,
        }
    }
}

fn tables(ui: &mut Ui, tables: &mut [(TableName, bool)]) {
    ui.label(RichText::new(format!("Tables ({})", tables.len())).strong());
    ScrollArea::vertical()
        .max_height(dialog::LIST_HEIGHT / 2.0)
        .show(ui, |ui| {
            for (table, ticked) in tables.iter_mut() {
                ui.checkbox(ticked, format!("{}.{}", table.schema, table.name));
            }
        });
    form::description_label(
        ui,
        "Some tables alone come back with their rows, without their indexes, keys or triggers.",
    );
}

/// *Restore shop.dump into a new database "shop_restored" on "local"?*
fn question(
    file: &Path,
    into: Into,
    database: &str,
    data_source: &str,
    drop_first: bool,
) -> String {
    let file = file.file_name().map_or_else(
        || file.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    match (into, drop_first) {
        (Into::NewDatabase, _) => {
            format!("Restore {file} into a new database \"{database}\" on \"{data_source}\"?")
        }
        (Into::NewFile, _) => {
            format!("Restore {file}, a dump of \"{data_source}\", into a new file {database}?")
        }
        (Into::Existing, false) => format!(
            "Restore {file} into \"{database}\" on \"{data_source}\"? Objects that already exist \
             will make the restore fail."
        ),
        (Into::Existing, true) => format!(
            "Restore {file} into \"{database}\" on \"{data_source}\"? Every object in the file is \
             dropped first, with its data."
        ),
    }
}

/// *PostgreSQL custom dump, 2 tables, 142 MB*.
fn describe(found: &DumpFileFound) -> String {
    let kind = match found.kind {
        DumpFileKind::Custom => "PostgreSQL custom dump".to_owned(),
        DumpFileKind::Directory => "PostgreSQL directory dump".to_owned(),
        DumpFileKind::SqliteFile => "SQLite database".to_owned(),
        DumpFileKind::DuckDbExport => "DuckDB export".to_owned(),
        DumpFileKind::Sql { engine, gzip } => {
            let engine = engine.map_or("SQL".to_owned(), |engine| format!("{engine} SQL"));
            if gzip {
                format!("{engine} dump, gzipped")
            } else {
                format!("{engine} dump")
            }
        }
    };
    let tables = found
        .contents
        .as_ref()
        .map(|contents| format!(", {} tables", contents.tables.len()))
        .unwrap_or_default();
    format!("{kind}{tables}, {}", dbrover_core::ByteSize(found.bytes))
}

/// Where a SQLite or DuckDB dump is restored by default: beside it, as a new file.
fn restored_file(dump: &Path, engine: DumpEngine) -> String {
    let ending = if engine == DumpEngine::DuckDb {
        "duckdb"
    } else {
        "sqlite"
    };
    dump.with_file_name(format!("{}-restored.{ending}", stem(dump)))
        .display()
        .to_string()
}

/// The file's name without its dump endings: `shop-2026-11-27-1412` for `….sql.gz`.
fn stem(file: &Path) -> String {
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    [".sql.gz", ".dump", ".sql", ".sqlite"]
        .iter()
        .find_map(|ending| name.strip_suffix(ending))
        .unwrap_or(&name)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_naming_the_file_database_and_data_source() {
        let file = Path::new("/d/shop.dump");
        assert_eq!(
            question(file, Into::NewDatabase, "shop_restored", "local", false),
            "Restore shop.dump into a new database \"shop_restored\" on \"local\"?"
        );
        assert!(
            question(file, Into::Existing, "shop", "staging", true).ends_with("with its data.")
        );
        assert_eq!(stem(Path::new("/d/shop-1.sql.gz")), "shop-1");
    }
}
