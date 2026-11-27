//! Dump…: what to dump, how and where to, with the program that will run it (W17,
//! `docs/dump-restore.md`). The dump itself runs in the status bar once the dialog closes.

use std::num::NonZeroU8;
use std::path::{Path, PathBuf};

use dbrover_core::{
    CatalogPath, DataSourceId, DumpContent, DumpEngine, DumpFormat, DumpOptions, DumpRequest,
    DumpScope, NodeKind, TableName,
};
use dbrover_dump::{DumpSpec, Role, RunFiles, Target, Version};
use dbrover_engine::{Command, Event, ProcessEnvironment, ProgramLine, ProgramPaths, ProgramQuery};
use egui::{Id, Ui};

use crate::dialog::{self, DialogSize};
use crate::form::{self, Segment};
use crate::icons;
use crate::save_dump_form::SaveDumpForm;
use crate::state::DataSourceEntry;
use crate::theme::Palette;
use crate::widgets;

pub enum DumpAction {
    Start(DumpRequest),
    /// Save the dump's settings under a name, to run again (W17h).
    Save(Box<dbrover_store::SavedDump>),
    Browse,
    Close,
}

/// The program a run would use, as the engine answered.
pub(crate) enum ProgramState {
    Choosing,
    Chosen(ProgramLine),
    Missing(String),
    /// SQLite and DuckDB dump and restore themselves.
    Itself(DumpEngine),
}

impl ProgramState {
    /// What a run of `engine` starts with: a program to look for, or none to run.
    pub(crate) fn for_engine(engine: DumpEngine) -> Self {
        if engine.runs_a_program() {
            Self::Choosing
        } else {
            Self::Itself(engine)
        }
    }

    pub(crate) fn is_ready(&self) -> bool {
        matches!(self, Self::Chosen(_) | Self::Itself(_))
    }

    pub(crate) fn apply(&mut self, query: &ProgramQuery, event: &Event) {
        if let Event::DumpProgramChosen {
            query: answered,
            result,
        } = event
            && answered == query
        {
            *self = match result {
                Ok(line) => Self::Chosen(line.clone()),
                Err(why) => Self::Missing(why.clone()),
            };
        }
    }

    /// The program's line: which copy and its version, beside the server's, or why there is
    /// none and how to install one.
    pub(crate) fn show(&self, ui: &mut Ui, server: &str, palette: &Palette) {
        match self {
            Self::Choosing => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Looking for the program…");
                });
            }
            Self::Chosen(line) => {
                ui.label(format!(
                    "{} {}",
                    file_name(&line.path),
                    line.version.version
                ));
                form::description_label(
                    ui,
                    &format!("{}, for a server of version {server}", line.path.display()),
                );
                if let Some(warning) = &line.warning {
                    ui.colored_label(palette.warning, warning);
                }
            }
            Self::Missing(why) => {
                widgets::status(ui, icons::WARNING, palette.error, why);
            }
            Self::Itself(engine) => {
                form::description_label(
                    ui,
                    &format!("{engine} does it itself, inside DbRover: no program to install."),
                );
            }
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

pub struct DumpDialog {
    data_source: DataSourceId,
    name: String,
    engine: DumpEngine,
    server: String,
    query: ProgramQuery,
    program: ProgramState,
    database: String,
    scope: DumpScope,
    format: DumpFormat,
    options: DumpOptions,
    file: String,
    /// Where **Copy command** says the program connects; `None` through an SSH tunnel, whose
    /// port is only known once the dump runs.
    target: Option<Target>,
    /// **Save…**'s form, once asked for.
    saving: Option<SaveDumpForm>,
}

impl DumpDialog {
    /// The dialog dumping what `path` names on the connected data source `entry`, into
    /// `folder`, and the command that finds its program. `None` where nothing can be dumped.
    pub fn open(
        entry: &DataSourceEntry,
        path: &CatalogPath,
        folder: &Path,
        programs: ProgramPaths,
    ) -> Option<(Self, Option<Command>)> {
        let engine = entry.dump_engine()?;
        let dbrover_engine::SessionState::Connected(server) = &entry.session else {
            return None;
        };
        // A SQLite or DuckDB file has no database name of its own: the data source's stands in.
        let own = entry
            .data_source
            .config
            .database()
            .unwrap_or(&entry.data_source.name);
        let (database, scope) = dump_scope(engine, path, own);
        let format = engine.formats()[0];
        let file = folder.join(dump_file_name(&database, format, &jiff::Zoned::now()));
        let query = ProgramQuery {
            engine,
            role: Role::Dump,
            server: server.version.clone(),
            archive_made_by: None,
            programs,
        };
        let command = engine
            .runs_a_program()
            .then(|| Command::ChooseDumpProgram(query.clone()));
        let dialog = Self {
            data_source: entry.data_source.id,
            name: entry.data_source.name.clone(),
            engine,
            server: server.version.clone(),
            query,
            program: ProgramState::for_engine(engine),
            target: copy_target(entry, &database),
            database,
            scope,
            format,
            options: DumpOptions::default(),
            file: file.display().to_string(),
            saving: None,
        };
        Some((dialog, command))
    }

    pub fn apply(&mut self, event: &Event) {
        self.program.apply(&self.query, event);
    }

    pub fn set_file(&mut self, path: &Path) {
        self.file = path.display().to_string();
    }

    pub fn file(&self) -> PathBuf {
        PathBuf::from(&self.file)
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<DumpAction> {
        let modal = dialog::modal(ctx, Id::new("dump_dialog"), DialogSize::Wide)
            .show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(DumpAction::Close);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut Ui) -> Option<DumpAction> {
        let palette = &Palette::for_visuals(ui.visuals());
        form::Header::new(icons::DOWNLOAD_SIMPLE, &format!("Dump from {}", self.name))
            .subtitle(&scope_label(&self.database, &self.scope))
            .show(ui);
        let mut browse = false;
        form::card(ui, |rows| {
            rows.row("Content", None, |ui| {
                form::segmented(
                    ui,
                    &mut self.options.content,
                    &[
                        Segment::new(DumpContent::SchemaAndData, "Schema and data"),
                        Segment::new(DumpContent::SchemaOnly, "Schema only"),
                        Segment::new(DumpContent::DataOnly, "Data only"),
                    ],
                );
            });
            rows.row("Format", Some(self.format.description()), |ui| {
                let previous = self.format;
                egui::ComboBox::from_id_salt("dump_format")
                    .selected_text(format_name(self.format))
                    .show_ui(ui, |ui| {
                        for &format in self.engine.formats() {
                            ui.selectable_value(&mut self.format, format, format_name(format));
                        }
                    });
                if self.format != previous {
                    self.file = with_extension(&self.file, previous, self.format);
                }
            });
            if self.engine == DumpEngine::Postgres {
                rows.row("Leave out owners", None, |ui| {
                    form::toggle(ui, &mut self.options.leave_out_owners, "Leave out owners");
                });
                rows.row("Leave out privileges", None, |ui| {
                    form::toggle(
                        ui,
                        &mut self.options.leave_out_privileges,
                        "Leave out privileges",
                    );
                });
            }
            if self.format == DumpFormat::Directory {
                rows.row(
                    "Jobs",
                    Some("Connections dumping at once, each reading a whole table."),
                    |ui| jobs(ui, &mut self.options.jobs),
                );
            }
            rows.row("To", None, |ui| {
                browse = ui.button("Browse…").clicked();
                widgets::name(
                    ui.text_edit_singleline(&mut self.file),
                    egui::WidgetType::TextEdit,
                    "Dump to",
                    true,
                );
            });
            rows.full(|ui| self.program.show(ui, &self.server, palette));
        });
        if browse {
            return Some(DumpAction::Browse);
        }
        let ready = self.program.is_ready() && !self.file.trim().is_empty();
        let request = self.request();
        let data_source = self.data_source;
        if let Some(saving) = &mut self.saving {
            form::section(ui, "Save to run again", |ui| saving.show(ui));
            let problem = saving.problem();
            let (save, back) = form::footer(ui, |ui| {
                let save = form::primary_button(
                    ui,
                    "Save",
                    problem.is_none() && !self.file.trim().is_empty(),
                )
                .on_disabled_hover_text(problem.unwrap_or("Choose where its files go"))
                .clicked();
                (save, ui.button("Back").clicked())
            });
            if back {
                self.saving = None;
                return None;
            }
            let saved = save.then(|| saving.saved(data_source, &request)).flatten();
            return saved.map(|saved| DumpAction::Save(Box::new(saved)));
        }
        let ((save, ()), (dump, cancel)) = form::footer_with_leading(
            ui,
            |ui| (ui.button("Save…").clicked(), self.copy_command(ui)),
            |ui| {
                let dump = form::primary_button(ui, "Dump", ready).clicked();
                (dump, ui.button("Cancel").clicked())
            },
        );
        if cancel {
            return Some(DumpAction::Close);
        }
        if save {
            self.saving = Some(SaveDumpForm::new(&self.database));
        }
        dump.then(|| DumpAction::Start(self.request()))
    }

    fn request(&self) -> DumpRequest {
        DumpRequest {
            data_source: self.data_source,
            database: self.database.clone(),
            scope: self.scope.clone(),
            format: self.format,
            options: self.options,
            file: self.file(),
        }
    }

    /// **Copy command**: the command as it will run, without the password, which is never in
    /// it, for a terminal where `~/.pgpass` or `~/.my.cnf` holds it.
    fn copy_command(&self, ui: &mut Ui) {
        let command = match (&self.program, &self.target) {
            (ProgramState::Chosen(line), Some(target)) => shown_command(self, line, target),
            _ => None,
        };
        let copy = ui
            .add_enabled(command.is_some(), egui::Button::new("Copy command"))
            .on_disabled_hover_text(
                "Known once the dump runs, through an SSH tunnel or inside DbRover: the log and \
                 History have it then",
            );
        if let Some(command) = command.filter(|_| copy.clicked()) {
            ui.ctx().copy_text(command);
        }
    }
}

fn shown_command(dialog: &DumpDialog, line: &ProgramLine, target: &Target) -> Option<String> {
    let spec = DumpSpec {
        engine: dialog.engine,
        program: line.version.version,
        server: Version::parse_leading(&dialog.server)?,
        scope: &dialog.scope,
        format: dialog.format,
        options: dialog.options,
    };
    let files = RunFiles {
        secret: PathBuf::from("~/.pgpass"),
        filter: PathBuf::from("filter.txt"),
        output: dialog.file(),
    };
    let command = dbrover_dump::dump_command(&spec, target, &files).ok()?;
    Some(dbrover_dump::command_line(&line.path, &command.shown))
}

/// Where the program connects, for **Copy command**, from the data source's settings; `None`
/// through an SSH tunnel.
fn copy_target(entry: &DataSourceEntry, database: &str) -> Option<Target> {
    let config = entry.data_source.config.resolve(&ProcessEnvironment).ok()?;
    let server = config.server()?;
    if server.ssh.is_some() {
        return None;
    }
    Some(Target {
        host: server.host.clone(),
        port: server.port_number().ok()?,
        user: server.user.clone(),
        database: database.to_owned(),
        tls: server.tls.clone(),
        tunnel_port: None,
    })
}

fn jobs(ui: &mut Ui, jobs: &mut NonZeroU8) {
    let mut value = jobs.get();
    ui.add(egui::DragValue::new(&mut value).range(1..=16));
    if let Some(chosen) = NonZeroU8::new(value) {
        *jobs = chosen;
    }
}

fn format_name(format: DumpFormat) -> &'static str {
    match format {
        DumpFormat::Custom => "Custom (.dump)",
        DumpFormat::Directory => "Directory",
        DumpFormat::Sql => "Plain SQL (.sql)",
        DumpFormat::SqlGz => "Plain SQL, gzipped (.sql.gz)",
        DumpFormat::DatabaseFile => "A copy of the file (.sqlite)",
        DumpFormat::Parquet => "Parquet files in a folder",
        DumpFormat::Csv => "CSV files in a folder",
    }
}

/// The database a dump of the node at `path` reads, and what of it: a PostgreSQL schema or
/// table, a MySQL database (its tree's schemas) or table, or the data source's own database.
pub(crate) fn dump_scope(engine: DumpEngine, path: &CatalogPath, own: &str) -> (String, DumpScope) {
    let table = path.name_of(NodeKind::Table);
    let schema = path.name_of(NodeKind::Schema);
    let database = path.name_of(NodeKind::Database).unwrap_or(own);
    match engine {
        DumpEngine::Postgres => {
            let scope = match (schema, table) {
                (Some(schema), Some(table)) => DumpScope::Tables(vec![TableName {
                    schema: schema.to_owned(),
                    name: table.to_owned(),
                }]),
                (Some(schema), None) => DumpScope::Schemas(vec![schema.to_owned()]),
                _ => DumpScope::Database,
            };
            (database.to_owned(), scope)
        }
        DumpEngine::Mysql | DumpEngine::MariaDb => {
            let database = schema.unwrap_or(database).to_owned();
            let scope = match table {
                Some(table) => DumpScope::Tables(vec![TableName {
                    schema: database.clone(),
                    name: table.to_owned(),
                }]),
                None => DumpScope::Database,
            };
            (database, scope)
        }
        // A whole database, the one file.
        DumpEngine::Sqlite | DumpEngine::DuckDb => (own.to_owned(), DumpScope::Database),
    }
}

/// *shop › public*, *shop › public.orders*, or *shop*.
fn scope_label(database: &str, scope: &DumpScope) -> String {
    match scope {
        DumpScope::Database => database.to_owned(),
        DumpScope::Schemas(schemas) => format!("{database} › {}", schemas.join(", ")),
        DumpScope::Tables(tables) => {
            let names: Vec<String> = tables
                .iter()
                .map(|table| format!("{}.{}", table.schema, table.name))
                .collect();
            format!("{database} › {}", names.join(", "))
        }
    }
}

pub(crate) use dbrover_store::dump_file_name;

/// `file` with `from`'s ending swapped for `to`'s, so changing the format keeps the name.
fn with_extension(file: &str, from: DumpFormat, to: DumpFormat) -> String {
    let stem = from
        .extension()
        .and_then(|extension| file.strip_suffix(&format!(".{extension}")))
        .unwrap_or(file);
    match to.extension() {
        Some(extension) => format!("{stem}.{extension}"),
        None => stem.to_owned(),
    }
}

/// `Documents/DbRover dumps/<data source>` in the home folder, where a dump goes first.
pub(crate) fn default_folder(data_source: &str) -> PathBuf {
    let home = std::env::home_dir().unwrap_or_default();
    home.join("Documents")
        .join("DbRover dumps")
        .join(sanitized(data_source))
}

/// A data source's name as a folder's: the characters no system allows in one replaced.
fn sanitized(name: &str) -> String {
    name.chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '-' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(segments: &[(NodeKind, &str)]) -> CatalogPath {
        segments
            .iter()
            .fold(CatalogPath::root(), |path, (kind, name)| {
                path.child(*kind, *name)
            })
    }

    #[test]
    fn dumps_what_was_right_clicked() {
        let table = path(&[(NodeKind::Schema, "public"), (NodeKind::Table, "orders")]);
        assert_eq!(
            dump_scope(DumpEngine::Postgres, &table, "shop"),
            (
                "shop".to_owned(),
                DumpScope::Tables(vec![TableName {
                    schema: "public".to_owned(),
                    name: "orders".to_owned()
                }])
            )
        );
        let schema = path(&[(NodeKind::Schema, "public")]);
        assert_eq!(
            dump_scope(DumpEngine::Postgres, &schema, "shop").1,
            DumpScope::Schemas(vec!["public".to_owned()])
        );
        assert_eq!(
            dump_scope(DumpEngine::Postgres, &CatalogPath::root(), "shop"),
            ("shop".to_owned(), DumpScope::Database)
        );
        let mysql_database = path(&[(NodeKind::Schema, "sales")]);
        assert_eq!(
            dump_scope(DumpEngine::Mysql, &mysql_database, "shop"),
            ("sales".to_owned(), DumpScope::Database)
        );
    }

    #[test]
    fn names_dumps_by_database_and_time() {
        let now: jiff::Zoned = "2026-11-27T14:12:00[Europe/Madrid]".parse().unwrap();
        assert_eq!(
            dump_file_name("shop", DumpFormat::Custom, &now),
            "shop-2026-11-27-141200.dump"
        );
        assert_eq!(
            dump_file_name("shop", DumpFormat::Directory, &now),
            "shop-2026-11-27-141200"
        );
        assert_eq!(
            with_extension("/d/shop-1.dump", DumpFormat::Custom, DumpFormat::SqlGz),
            "/d/shop-1.sql.gz"
        );
        assert_eq!(
            with_extension("/d/shop-1", DumpFormat::Directory, DumpFormat::Sql),
            "/d/shop-1.sql"
        );
        assert_eq!(sanitized("prod: eu/west"), "prod- eu-west");
    }
}
