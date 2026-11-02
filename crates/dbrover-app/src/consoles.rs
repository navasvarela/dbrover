//! The open query consoles, their files, and the rules for starting runs in them. Returns the
//! engine commands to send rather than sending them, so it can be tested without an engine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dbrover_core::{CatalogPath, DataSource, DataSourceId, FullScan, NodeKind, SafetyLevel};
use dbrover_engine::{
    Command, ConsoleId, Event, RunId, RunOrigin, RunPurpose, ScanApproval, TransactionMode,
    WatchedFile,
};
use dbrover_export::ExportSpec;
use dbrover_language::QueryLanguage;
use dbrover_language::redis::KeyView;
use dbrover_sql::{FormatOptions, ParameterValues};
use dbrover_store::{ConsoleKey, ConsoleName, SqlFileContents};

use crate::console::{
    ConsoleAction, ExportedRows, FormatScope, QueryConsole, RecentCompletions, RowsToOpen,
    RunDestination, RunScope, SubmittedRun,
};
use crate::hazard_dialog::{
    self, HazardChoice, HazardConfirmation, Pending, Review, ReviewContext,
};
use crate::parameters_dialog::{ParametersChoice, ParametersDialog};
use crate::rollback_dialog::Closing;

/// How long a console's text stays unsaved after an edit. Long enough that typing doesn't
/// write a file per keystroke, short enough that a crash loses at most a second of typing.
pub const SAVE_DELAY: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct Consoles {
    open: BTreeMap<ConsoleKey, QueryConsole>,
    /// Completions accepted in any console, which rank first in all of them.
    recent_completions: RecentCompletions,
    saved: SavedConsoles,
    next_console: u64,
    next_run: u64,
    hazard: Option<HazardConfirmation>,
    /// A run waiting for the values of its parameters.
    parameters: Option<ParametersDialog>,
}

/// Console files that aren't open in a tab, with their text, so they can be reopened.
#[derive(Debug, Default)]
pub struct SavedConsoles {
    files: BTreeMap<ConsoleKey, String>,
}

impl SavedConsoles {
    /// The names of `data_source`'s closed consoles, in order.
    pub fn names(&self, data_source: DataSourceId) -> impl Iterator<Item = &ConsoleName> {
        self.files
            .keys()
            .filter(move |key| key.data_source == data_source)
            .map(|key| &key.name)
    }
}

impl Consoles {
    /// The open consoles to draw, the saved ones the explorer offers, and the recent
    /// completions, borrowed at once.
    pub fn open_and_saved_mut(
        &mut self,
    ) -> (
        &mut BTreeMap<ConsoleKey, QueryConsole>,
        &SavedConsoles,
        &RecentCompletions,
    ) {
        (&mut self.open, &self.saved, &self.recent_completions)
    }

    #[cfg(test)]
    pub fn saved(&self) -> &SavedConsoles {
        &self.saved
    }

    #[cfg(test)]
    pub fn open_consoles(&self) -> impl Iterator<Item = &QueryConsole> {
        self.open.values()
    }

    /// Opens a new console on `data_source`, holding `text`, in a file of its own.
    pub fn open(&mut self, data_source: &DataSource, text: String) -> ConsoleKey {
        let key = ConsoleKey {
            data_source: data_source.id,
            name: self.free_name(data_source.id),
        };
        let console = self.new_console(data_source, key.clone(), text);
        self.open.insert(key.clone(), console);
        key
    }

    /// Reopens a saved console. `None` if there is no such file.
    pub fn reopen(&mut self, data_source: &DataSource, key: &ConsoleKey) -> Option<ConsoleKey> {
        let text = self.saved.files.remove(key)?;
        let console = self.new_console(data_source, key.clone(), text).reopened();
        self.open.insert(key.clone(), console);
        Some(key.clone())
    }

    /// Opens a console on `data_source` showing the `.sql` file at `path`, just read. Its key
    /// is none that `in_layout` holds: a tab restored from the last session may still be
    /// waiting for its own file.
    pub fn open_file(
        &mut self,
        data_source: &DataSource,
        path: PathBuf,
        contents: SqlFileContents,
        in_layout: impl Fn(&ConsoleKey) -> bool,
    ) -> ConsoleKey {
        let key = self.free_file_key(data_source.id, in_layout);
        self.reopen_file(data_source, key.clone(), path, contents);
        key
    }

    /// Opens the console `key`, restored from the last session's layout, showing the `.sql`
    /// file at `path`.
    pub fn reopen_file(
        &mut self,
        data_source: &DataSource,
        key: ConsoleKey,
        path: PathBuf,
        contents: SqlFileContents,
    ) {
        let console = self
            .new_console(data_source, key.clone(), contents.text.clone())
            .showing(path, contents);
        self.open.insert(key, console);
    }

    /// The console showing the `.sql` file at `path`, if one does.
    pub fn showing_file(&self, path: &Path) -> Option<ConsoleKey> {
        self.open
            .iter()
            .find(|(_, console)| console.file_path() == Some(path))
            .map(|(key, _)| key.clone())
    }

    /// Each console showing a `.sql` file, with its path, to restore them at the next launch.
    pub fn files(&self) -> impl Iterator<Item = (&ConsoleKey, &Path)> {
        self.open
            .iter()
            .filter_map(|(key, console)| Some((key, console.file_path()?)))
    }

    /// The `.sql` files the consoles show, to look at for changes made by other programs.
    pub fn watched_files(&self) -> Vec<WatchedFile> {
        self.open
            .values()
            .filter_map(QueryConsole::watched_file)
            .collect()
    }

    /// Writes the console's text to its `.sql` file. `None` if it has none, and Save as must ask
    /// where.
    pub fn save_file(&mut self, key: &ConsoleKey) -> Option<Command> {
        self.open.get_mut(key)?.save_file()
    }

    /// Writes the console's text to `path`, which it shows from then on.
    pub fn save_file_as(&mut self, key: &ConsoleKey, path: PathBuf) -> Option<Command> {
        Some(self.open.get_mut(key)?.save_file_as(path))
    }

    /// The consoles `closing` would close that have changes not written to their `.sql`
    /// files, with their titles. Disconnecting closes no tab, so it loses nothing here.
    pub fn unsaved_files(&self, closing: &Closing) -> Vec<(ConsoleKey, String)> {
        self.open
            .iter()
            .filter(|(key, console)| {
                !matches!(closing, Closing::Disconnect(_))
                    && closing.affects(key)
                    && console.has_unsaved_file()
            })
            .map(|(key, console)| (key.clone(), console.title().to_owned()))
            .collect()
    }

    pub fn get_mut(&mut self, key: &ConsoleKey) -> Option<&mut QueryConsole> {
        self.open.get_mut(key)
    }

    /// The lowest numbered `file_<n>` key no console of the data source uses, for a console
    /// showing a `.sql` file: never one of a console file in the config folder.
    fn free_file_key(
        &self,
        data_source: DataSourceId,
        in_layout: impl Fn(&ConsoleKey) -> bool,
    ) -> ConsoleKey {
        (1..)
            .filter_map(|n| ConsoleName::try_from(format!("file_{n}")).ok())
            .map(|name| ConsoleKey { data_source, name })
            .find(|key| {
                !self.open.contains_key(key)
                    && !self.saved.files.contains_key(key)
                    && !in_layout(key)
            })
            .unwrap_or_else(|| ConsoleKey {
                data_source,
                name: ConsoleName::numbered(0),
            })
    }

    /// The lowest numbered name no console of the data source uses, open or saved.
    fn free_name(&self, data_source: DataSourceId) -> ConsoleName {
        let in_use = |name: &ConsoleName| {
            let key = ConsoleKey {
                data_source,
                name: name.clone(),
            };
            self.open.contains_key(&key) || self.saved.files.contains_key(&key)
        };
        (1..)
            .map(ConsoleName::numbered)
            .find(|name| !in_use(name))
            .unwrap_or_else(|| ConsoleName::numbered(0))
    }

    fn new_console(
        &mut self,
        data_source: &DataSource,
        key: ConsoleKey,
        text: String,
    ) -> QueryConsole {
        self.next_console += 1;
        let title = title(&data_source.name, &key.name);
        let language = QueryLanguage::from(data_source.config.driver());
        QueryConsole::new(ConsoleId(self.next_console), key, title, language, text).on(data_source)
    }

    /// Opens a console that selects everything from the table or view at `path`, reads the
    /// Redis key there whole, finds every document of the MongoDB collection there, searches
    /// every document of the Elasticsearch index, alias or data stream there, or selects every
    /// item of the DynamoDB table or index there, and runs it.
    pub fn open_data(
        &mut self,
        data_source: &DataSource,
        path: &CatalogPath,
    ) -> Option<(ConsoleKey, Command)> {
        let language = QueryLanguage::from(data_source.config.driver());
        if let Some(text) = key_viewer(data_source, path)
            .or_else(|| collection_viewer(data_source, path))
            .or_else(|| search_index_viewer(path))
            .or_else(|| language.read_all(path).map(|text| format!("{text}\n")))
        {
            return self.open_query(data_source, text);
        }
        let dialect = language.dialect()?;
        let name = path.qualified_name(dialect.quoting())?;
        self.open_query(data_source, format!("SELECT * FROM {name};\n"))
    }

    /// Opens a console that selects the rows a foreign key leads to, and runs it.
    pub fn open_rows(
        &mut self,
        data_source: &DataSource,
        rows: &RowsToOpen,
    ) -> Option<(ConsoleKey, Command)> {
        let dialect = QueryLanguage::from(data_source.config.driver()).dialect()?;
        self.open_query(data_source, format!("{};\n", rows.sql(dialect)))
    }

    /// Opens a console holding `text`, and runs all of it.
    fn open_query(
        &mut self,
        data_source: &DataSource,
        text: String,
    ) -> Option<(ConsoleKey, Command)> {
        let key = self.open(data_source, text);
        let statements = self.open.get(&key)?.statements_to_run(RunScope::All);
        let run = SubmittedRun {
            statements,
            purpose: RunPurpose::Run,
            parameters: ParameterValues::default(),
            origin: RunOrigin::Console,
            destination: RunDestination::Grid,
            // What the explorer opens reads one page and then only what the user pages
            // through, so a scan's cost guard has nothing to ask (docs/nosql.md, N5).
            scans: ScanApproval::Allow,
        };
        let command = self.start_run(&key, run)?;
        Some((key, command))
    }

    /// Opens a console holding an action's filled-in SQL. With `run`, runs all of it as typed
    /// SQL runs: asking for parameters and confirmations first, and recording it as the action's
    /// in the history.
    pub fn open_action(
        &mut self,
        data_source: &DataSource,
        text: String,
        run: Option<RunOrigin>,
    ) -> (ConsoleKey, Option<Command>) {
        let key = self.open(data_source, text);
        let Some(origin) = run else {
            return (key, None);
        };
        let Some(console) = self.open.get(&key) else {
            return (key, None);
        };
        let run = SubmittedRun {
            statements: console.statements_to_run(RunScope::All),
            purpose: RunPurpose::Run,
            parameters: ParameterValues::default(),
            origin,
            destination: RunDestination::Grid,
            scans: ScanApproval::Ask,
        };
        let command = self.submit(&key, run);
        (key, command)
    }

    /// Closes the console's tab and connection. Its file is kept, with the latest text, if
    /// the user typed in it, and removed otherwise.
    pub fn close(&mut self, key: &ConsoleKey) -> Vec<Command> {
        let Some(mut console) = self.open.remove(key) else {
            return Vec::new();
        };
        let mut commands = vec![Command::CloseConsole(console.id())];
        commands.extend(console.file_on_close());
        if console.keeps_file() {
            self.saved
                .files
                .insert(key.clone(), console.text().to_owned());
        }
        commands
    }

    /// Saves for the consoles whose edits are due, and when to look again.
    pub fn due_saves(&mut self, now: Instant) -> (Vec<Command>, Option<Duration>) {
        let mut next_due: Option<Duration> = None;
        let mut saves = Vec::new();
        for console in self.open.values_mut() {
            match console.take_save(now, SAVE_DELAY) {
                Ok(save) => saves.push(save),
                Err(Some(wait)) => next_due = Some(next_due.map_or(wait, |next| next.min(wait))),
                Err(None) => {}
            }
        }
        (saves, next_due)
    }

    /// Saves for every console with unsaved edits, for when the app quits.
    pub fn unsaved(&mut self) -> Vec<Command> {
        self.open
            .values_mut()
            .filter_map(|console| console.take_save(Instant::now(), Duration::ZERO).ok())
            .collect()
    }

    /// The titles of the consoles with an open transaction that `closing` would roll back.
    pub fn rolled_back_by(&self, closing: &Closing) -> Vec<String> {
        self.open
            .iter()
            .filter(|(key, console)| closing.affects(key) && console.has_open_transaction())
            .map(|(_, console)| console.title().to_owned())
            .collect()
    }

    /// The console's selected cells as `copy` turns them into text, if it is open and they can
    /// be copied that way.
    pub fn copy(
        &mut self,
        key: &ConsoleKey,
        copy: crate::console::ClipboardCopy,
        options: &dbrover_export::ExportOptions,
    ) -> Option<String> {
        self.open.get_mut(key)?.copy(copy, options)
    }

    /// The console's selected cells as an export program reads them, and `format`'s program
    /// to run on them, if it is open and they can be.
    pub fn program_input(
        &mut self,
        key: &ConsoleKey,
        format: &dbrover_export::CustomFormat,
    ) -> Option<(dbrover_export::ProgramSpec, String)> {
        self.open.get_mut(key)?.program_input(format)
    }

    /// What a template is told about exporting the rows of `sql` from the console, if it is
    /// open.
    pub fn context_for(
        &self,
        key: &ConsoleKey,
        sql: &str,
    ) -> Option<dbrover_export::ExportContext> {
        Some(self.open.get(key)?.context_for(sql, None))
    }

    /// What the export dialog shows of the console's rows, if it is open and has some.
    pub fn export_preview(&self, key: &ConsoleKey) -> Option<crate::export_dialog::ExportPreview> {
        self.open.get(key)?.export_preview()
    }

    /// Writes the rows the console's grid shows to `path`.
    pub fn export(
        &mut self,
        key: &ConsoleKey,
        rows: ExportedRows,
        spec: ExportSpec,
        path: PathBuf,
    ) -> Option<Command> {
        match rows {
            ExportedRows::Loaded => self.open.get_mut(key)?.export(spec, path),
            ExportedRows::All => {
                let console = self.open.get_mut(key)?;
                self.next_run += 1;
                console.export_all(RunId(self.next_run), spec, path)
            }
        }
    }

    /// Formats SQL in the console, if it is still open.
    pub fn format(&mut self, key: &ConsoleKey, scope: FormatScope, options: &FormatOptions) {
        if let Some(console) = self.open.get_mut(key) {
            console.format(scope, options, Instant::now());
        }
    }

    /// Puts a statement at the console's cursor, if the console is still open.
    pub fn insert_statement(&mut self, key: &ConsoleKey, sql: &str) {
        if let Some(console) = self.open.get_mut(key) {
            console.insert_statement(sql, Instant::now());
        }
    }

    /// Takes in an engine event. Returns what it calls for: removing a console's file from the
    /// config folder once the console was saved to a `.sql` file.
    pub fn apply(&mut self, event: &Event) -> Option<Command> {
        let command = self.apply_to_files(event);
        self.apply_to_consoles(event);
        command
    }

    fn apply_to_files(&mut self, event: &Event) -> Option<Command> {
        match event {
            Event::SqlFileSaved {
                console, result, ..
            } => self.open.get_mut(console)?.file_saved(result),
            Event::SqlFileChanged { path, contents } => {
                for console in self
                    .open
                    .values_mut()
                    .filter(|console| console.file_path() == Some(path.as_path()))
                {
                    console.file_changed(contents.as_ref());
                }
                None
            }
            _ => None,
        }
    }

    fn apply_to_consoles(&mut self, event: &Event) {
        if let Event::ConsolesLoaded(files) = event {
            self.saved.files.extend(
                files
                    .iter()
                    .filter(|file| !self.open.contains_key(&file.key))
                    .map(|file| (file.key.clone(), file.text.clone())),
            );
        }
        if let Event::DataSourceSaved(data_source) = event {
            for console in self
                .open
                .values_mut()
                .filter(|console| console.data_source() == data_source.id)
            {
                console.follow(data_source);
            }
        }
        for console in self.open.values_mut() {
            console.apply(event);
        }
        if let Event::ScanHeld {
            run,
            statement,
            scan,
        } = event
        {
            self.ask_to_scan(*run, *statement, scan);
        }
    }

    /// Asks whether to run, after all, the statement the cost guard held back in `run`, and
    /// the ones after it.
    fn ask_to_scan(&mut self, run: RunId, statement: usize, scan: &FullScan) {
        let held = self.open.iter().find_map(|(key, console)| {
            let rest = console.rest_of_run(run, statement)?;
            Some((key.clone(), console.data_source_name().to_owned(), rest))
        });
        if let Some((key, data_source, rest)) = held {
            self.hazard = Some(hazard_dialog::confirm_scan(key, &data_source, scan, rest));
        }
    }

    pub fn handle(&mut self, key: &ConsoleKey, action: ConsoleAction) -> Option<Command> {
        let id = self.open.get(key)?.id();
        match action {
            ConsoleAction::Run(run) => self.submit(key, run),
            ConsoleAction::FetchMore(run) => Some(Command::FetchMore { console: id, run }),
            ConsoleAction::Cancel(run) => Some(Command::Cancel { console: id, run }),
            ConsoleAction::EndTransaction(end) => {
                Some(Command::EndTransaction { console: id, end })
            }
            ConsoleAction::DescribeTable(table) => Some(Command::DescribeTable {
                data_source: self.open.get(key)?.data_source(),
                table,
            }),
            ConsoleAction::SubmitEdits => self.submit_edits(key),
            ConsoleAction::KeyCommand { command, then } => {
                let run = self.open.get_mut(key)?.key_command_run(command, &then)?;
                self.review(key, run)
            }
            ConsoleAction::Validate(statements) => Some(Command::ValidateStatements {
                console: id,
                statements,
            }),
            ConsoleAction::Reconnect => Some(Command::ReconnectConsole(id)),
            ConsoleAction::CompletionAccepted(candidate) => {
                self.recent_completions.record(&candidate);
                None
            }
            // The app opens a window, a file dialog or a console, or updates the catalog, for
            // these.
            ConsoleAction::ShowHistory
            | ConsoleAction::OpenExportDialog
            | ConsoleAction::Copy(_)
            | ConsoleAction::RunToFile
            | ConsoleAction::LoadCatalog(_)
            | ConsoleAction::Format(_)
            | ConsoleAction::OpenRows(_)
            | ConsoleAction::RunAction(..)
            | ConsoleAction::ExportChart(_) => None,
        }
    }

    /// The language of the console's data source, if the console is open.
    pub fn language(&self, key: &ConsoleKey) -> Option<QueryLanguage> {
        self.open.get(key).map(QueryConsole::language)
    }

    /// A new run's id, for a run the app starts itself: a table's export.
    pub fn new_run_id(&mut self) -> RunId {
        self.next_run += 1;
        RunId(self.next_run)
    }

    /// Runs `run`'s statement into its file: Run to file, once the format and file are chosen,
    /// going through the parameters and confirmations a run goes through.
    pub fn run_to_file(&mut self, key: &ConsoleKey, run: SubmittedRun) -> Option<Command> {
        self.submit(key, run)
    }

    /// The statement at the console's cursor, as Run to file runs it. `None` if the console
    /// is closed or there is no one statement there.
    pub fn statement_to_file(&self, key: &ConsoleKey) -> Option<SubmittedRun> {
        self.open.get(key)?.statement_to_file()
    }

    /// Runs the statements, first asking for the values of their parameters if the run
    /// doesn't carry them all.
    fn submit(&mut self, key: &ConsoleKey, run: SubmittedRun) -> Option<Command> {
        let console = self.open.get(key)?;
        let names = console
            .dialect()
            .map(|dialect| run.parameter_names(dialect))
            .unwrap_or_default();
        if names.iter().all(|name| run.parameters.contains(name)) {
            return self.review(key, run);
        }
        self.parameters = Some(ParametersDialog::new(
            key.clone(),
            run,
            names,
            console.parameters(),
        ));
        None
    }

    /// Runs the statements, unless one would change every row of a table or commit what the
    /// user may mean to roll back: then the confirmation is shown first and nothing runs
    /// until the user answers.
    fn review(&mut self, key: &ConsoleKey, run: SubmittedRun) -> Option<Command> {
        let console = self.open.get(key)?;
        let context = ReviewContext {
            language: console.language(),
            relies_on_rollback: console.transaction_mode() == TransactionMode::Manual
                || console.has_open_transaction(),
            careful: (console.safety() == SafetyLevel::Careful).then(|| console.data_source_name()),
        };
        match hazard_dialog::review(key.clone(), run, context) {
            Review::Safe(run) => self.start_run(key, run),
            Review::NeedsConfirmation(confirmation) => {
                self.hazard = Some(confirmation);
                None
            }
        }
    }

    /// Applies the grid's edits, asking first on a careful data source.
    fn submit_edits(&mut self, key: &ConsoleKey) -> Option<Command> {
        let console = self.open.get_mut(key)?;
        let changes = console.pending_changes();
        if console.safety() != SafetyLevel::Careful || changes.is_empty() {
            return console.submit_edits();
        }
        self.hazard = Some(hazard_dialog::confirm_edits(
            key.clone(),
            console.data_source_name(),
            changes,
        ));
        None
    }

    fn start_run(&mut self, key: &ConsoleKey, submitted: SubmittedRun) -> Option<Command> {
        let console = self.open.get_mut(key)?;
        self.next_run += 1;
        let run = RunId(self.next_run);
        if let RunDestination::File { path, spec } = submitted.destination {
            let statement = submitted.statements.into_iter().next()?;
            return console.run_to_file(run, statement.sql, submitted.parameters, path, spec);
        }
        console.begin_run(run, &submitted.statements);
        Some(Command::Execute {
            console: console.id(),
            data_source: console.data_source(),
            run,
            transactions: console.transaction_mode(),
            purpose: submitted.purpose,
            statements: submitted
                .statements
                .into_iter()
                .map(|statement| statement.sql)
                .collect(),
            parameters: submitted.parameters,
            origin: submitted.origin,
            scans: submitted.scans,
        })
    }

    /// Shows the parameters dialog, if a run is waiting for values, and goes on with the run
    /// once they are given: to the confirmations, then the engine. The console remembers them.
    pub fn show_parameters_dialog(&mut self, ctx: &egui::Context) -> Option<Command> {
        let choice = self.parameters.as_mut()?.show(ctx)?;
        let dialog = self.parameters.take()?;
        match choice {
            ParametersChoice::Run => {
                let (key, run) = dialog.into_run();
                self.open
                    .get_mut(&key)?
                    .remember_parameters(&run.parameters);
                self.review(&key, run)
            }
            ParametersChoice::Cancel => None,
        }
    }

    pub fn show_hazard_confirmation(&mut self, ctx: &egui::Context) -> Option<Command> {
        let choice = self.hazard.as_ref()?.show(ctx)?;
        let confirmation = self.hazard.take()?;
        match (choice, confirmation.pending) {
            (HazardChoice::RunAnyway, Pending::Run(run) | Pending::Scan(run)) => {
                self.start_run(&confirmation.console, run)
            }
            (HazardChoice::RunAnyway, Pending::Edits) => {
                self.open.get_mut(&confirmation.console)?.submit_edits()
            }
            (HazardChoice::Cancel, _) => None,
        }
    }
}

/// The commands that read the Redis key at `path` whole: its type's read command, after a
/// `SELECT` when the key is in another database than the data source's. `None` for a path
/// that isn't a key's.
fn key_viewer(data_source: &DataSource, path: &CatalogPath) -> Option<String> {
    let key = path.last()?;
    let NodeKind::Key(key_type) = key.kind else {
        return None;
    };
    let [name] = dbrover_language::redis::arguments(&key.name)
        .ok()?
        .try_into()
        .ok()?;
    let read = KeyView::new(name, key_type).read();
    let database = path.name_of(NodeKind::Database)?;
    let own = data_source
        .config
        .server()
        .map(|server| server.database.trim())
        .filter(|own| !own.is_empty())
        .unwrap_or("0");
    Some(if database == own {
        format!("{read}\n")
    } else {
        format!("SELECT {database}\n{read}\n")
    })
}

/// The command that finds every document of the MongoDB collection at `path`, in its own
/// database. `None` for a path that isn't a collection's.
fn collection_viewer(data_source: &DataSource, path: &CatalogPath) -> Option<String> {
    let collection = path
        .last()
        .filter(|last| last.kind == NodeKind::Collection)?;
    let database = path.name_of(NodeKind::Database)?;
    let own = data_source.config.default_schema();
    Some(format!(
        "{}\n",
        dbrover_language::mongo::find_all(database, &collection.name, own)
    ))
}

/// The search that reads every document of the Elasticsearch index, alias or data stream at
/// `path`. `None` for a path that isn't one.
fn search_index_viewer(path: &CatalogPath) -> Option<String> {
    let index = path
        .last()
        .filter(|last| last.kind == NodeKind::SearchIndex)?;
    Some(format!(
        "{}\n",
        dbrover_language::search::search_all(&index.name)
    ))
}

/// A console's tab title: the data source's name, numbered from the second console on, as
/// `shop`, `shop (2)`; or with the file's name if the user named it.
fn title(data_source: &str, name: &ConsoleName) -> String {
    match name.number() {
        Some(1) => data_source.to_owned(),
        Some(n) => format!("{data_source} ({n})"),
        None => format!("{data_source}: {name}"),
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{ConnectionConfig, SqliteConfig};
    use dbrover_store::SavedConsole;

    use super::*;
    use crate::console::SubmittedStatement;

    fn data_source(name: &str) -> DataSource {
        DataSource {
            id: DataSourceId::generate(),
            name: name.to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Sqlite(SqliteConfig {
                path: "/data/app.db".into(),
                read_only: false,
            }),
        }
    }

    fn run(statements: &[&str]) -> ConsoleAction {
        run_with(statements, ParameterValues::default())
    }

    fn run_with(statements: &[&str], parameters: ParameterValues) -> ConsoleAction {
        ConsoleAction::Run(SubmittedRun {
            statements: statements
                .iter()
                .map(|sql| SubmittedStatement {
                    sql: (*sql).to_owned(),
                    range: 0..sql.len(),
                    prefix_len: 0,
                    in_database: None,
                    plan: None,
                })
                .collect(),
            purpose: RunPurpose::Run,
            parameters,
            origin: RunOrigin::Console,
            destination: RunDestination::Grid,
            scans: ScanApproval::Ask,
        })
    }

    #[test]
    fn asks_for_parameters_unless_the_run_carries_them_all() {
        let mut consoles = Consoles::default();
        let key = consoles.open(&data_source("shop"), String::new());
        let since: ParameterValues = [("since", Some("2026-01-01".to_owned()))]
            .into_iter()
            .collect();

        assert!(
            consoles
                .handle(&key, run(&["SELECT :since", "SELECT :since, :until"]))
                .is_none()
        );
        let asking = consoles.parameters.take().unwrap();
        assert_eq!(asking.console, key);
        assert!(!consoles.open[&key].is_running());

        assert!(matches!(
            consoles.handle(&key, run_with(&["SELECT :since"], since)),
            Some(Command::Execute { parameters, .. }) if parameters.contains("since")
        ));
        assert!(consoles.parameters.is_none());
    }

    fn named(data_source: &DataSource, name: &str) -> ConsoleKey {
        ConsoleKey {
            data_source: data_source.id,
            name: ConsoleName::try_from(name.to_owned()).unwrap(),
        }
    }

    #[test]
    fn numbers_titles_and_runs_safe_statements_at_once() {
        let mut consoles = Consoles::default();
        let source = data_source("shop");
        let first = consoles.open(&source, String::new());
        let second = consoles.open(&source, String::new());

        assert_eq!(consoles.open[&first].title(), "shop");
        assert_eq!(consoles.open[&second].title(), "shop (2)");
        assert_eq!(second.name.as_str(), "console_2");
        assert!(matches!(
            consoles.handle(&first, run(&["SELECT 1"])),
            Some(Command::Execute { statements, .. }) if statements == ["SELECT 1"]
        ));
    }

    #[test]
    fn holds_back_unfiltered_deletes_until_confirmed() {
        let mut consoles = Consoles::default();
        let key = consoles.open(&data_source("shop"), String::new());

        assert!(
            consoles
                .handle(&key, run(&["SELECT 1", "DELETE FROM orders"]))
                .is_none()
        );

        assert!(consoles.hazard.is_some());
        assert!(!consoles.open[&key].is_running());
    }

    #[test]
    fn opens_table_data_with_a_select() {
        let mut consoles = Consoles::default();
        let path = CatalogPath::root()
            .child(dbrover_core::NodeKind::Schema, "main")
            .child(dbrover_core::NodeKind::Table, "Order Items");

        let (_, command) = consoles.open_data(&data_source("shop"), &path).unwrap();

        assert!(matches!(
            command,
            Command::Execute { statements, .. } if statements == ["SELECT * FROM main.\"Order Items\""]
        ));
    }

    fn redis(name: &str, database: &str) -> DataSource {
        DataSource {
            config: ConnectionConfig::Redis(dbrover_core::RedisConfig {
                server: dbrover_core::ServerConfig {
                    host: "localhost".to_owned(),
                    port: 6379.into(),
                    database: database.to_owned(),
                    user: String::new(),
                    tls: dbrover_core::TlsMode::Disable,
                    ssh: None,
                    password_command: None,
                },
                separator: ":".to_owned(),
            }),
            ..data_source(name)
        }
    }

    /// Open data on a key reads its value whole with its type's command, after selecting its
    /// database when it isn't the data source's.
    #[test]
    fn opens_a_mongodb_collection_with_a_find_in_its_database() {
        let mut consoles = Consoles::default();
        let shop = DataSource {
            config: ConnectionConfig::Mongo(dbrover_core::MongoConfig {
                server: dbrover_core::ServerConfig {
                    host: "localhost".to_owned(),
                    port: 27017.into(),
                    database: "shop".to_owned(),
                    user: String::new(),
                    tls: dbrover_core::TlsMode::Disable,
                    ssh: None,
                    password_command: None,
                },
                auth_database: String::new(),
                connection_string: None,
            }),
            ..data_source("shop")
        };
        let collection = |database: &str, name: &str| {
            CatalogPath::root()
                .child(NodeKind::Database, database)
                .child(NodeKind::Collection, name)
        };
        let cases = [
            (
                collection("shop", "orders"),
                "db.getCollection(\"orders\").find({})",
            ),
            (
                collection("logs", "web.v1"),
                "db.getSiblingDB(\"logs\").getCollection(\"web.v1\").find({})",
            ),
        ];
        for (path, expected) in cases {
            let (_, command) = consoles.open_data(&shop, &path).unwrap();
            let Command::Execute { statements, .. } = command else {
                panic!("expected a run, got {command:?}");
            };
            assert_eq!(statements, [expected], "{path}");
        }
    }

    #[test]
    fn opens_an_elasticsearch_index_with_a_search_of_every_document() {
        let mut consoles = Consoles::default();
        let search = data_source("search");
        let path = CatalogPath::root()
            .child(
                NodeKind::Folder(dbrover_core::FolderKind::Aliases),
                "aliases",
            )
            .child(NodeKind::SearchIndex, "orders-live");
        let (_, command) = consoles.open_data(&search, &path).unwrap();
        let Command::Execute { statements, .. } = command else {
            panic!("expected a run, got {command:?}");
        };
        assert_eq!(
            statements,
            ["GET /orders-live/_search\n{\n  \"query\": { \"match_all\": {} }\n}"]
        );
    }

    #[test]
    fn opens_a_dynamodb_table_or_index_with_a_select_of_every_item() {
        let mut consoles = Consoles::default();
        let dynamodb = DataSource {
            config: ConnectionConfig::DynamoDb(dbrover_core::DynamoDbConfig::default()),
            ..data_source("orders")
        };
        let table = CatalogPath::root().child(NodeKind::Table, "orders");
        let index = table
            .child(
                NodeKind::Folder(dbrover_core::FolderKind::Indexes),
                "indexes",
            )
            .child(NodeKind::Index, "by-status");
        for (path, expected) in [
            (table, "SELECT * FROM \"orders\""),
            (index, "SELECT * FROM \"orders\".\"by-status\""),
        ] {
            let (_, command) = consoles.open_data(&dynamodb, &path).unwrap();
            let Command::Execute { statements, .. } = command else {
                panic!("expected a run, got {command:?}");
            };
            assert_eq!(statements, [expected], "{path}");
        }
    }

    /// A statement the cost guard held back is asked about, and runs again from there, with
    /// the ones after it, allowed to scan once.
    #[test]
    fn asks_to_run_a_held_scan_and_the_statements_after_it() {
        let mut consoles = Consoles::default();
        let dynamodb = DataSource {
            config: ConnectionConfig::DynamoDb(dbrover_core::DynamoDbConfig::default()),
            ..data_source("orders")
        };
        let key = consoles.open(&dynamodb, String::new());
        let statement = |sql: &str| SubmittedStatement {
            sql: sql.to_owned(),
            range: 0..sql.len(),
            prefix_len: 0,
            in_database: None,
            plan: None,
        };
        let run = SubmittedRun {
            statements: vec![
                statement("INSERT INTO orders VALUE {'pk': 'a'}"),
                statement("SELECT * FROM orders"),
                statement("SELECT * FROM orders WHERE pk = 'a'"),
            ],
            purpose: RunPurpose::Run,
            parameters: ParameterValues::default(),
            origin: RunOrigin::Console,
            destination: RunDestination::Grid,
            scans: ScanApproval::Ask,
        };
        let Some(Command::Execute { run: id, scans, .. }) = consoles.start_run(&key, run) else {
            panic!("expected a run");
        };
        assert_eq!(scans, ScanApproval::Ask);
        consoles.apply(&Event::ScanHeld {
            run: id,
            statement: 1,
            scan: FullScan {
                table: "orders".to_owned(),
                index: None,
                partition_key: "pk".to_owned(),
            },
        });
        let Some(Pending::Scan(rest)) = consoles.hazard.take().map(|hazard| hazard.pending) else {
            panic!("expected to be asked about the scan");
        };
        let sql: Vec<&str> = rest.statements.iter().map(|s| s.sql.as_str()).collect();
        assert_eq!(
            sql,
            [
                "SELECT * FROM orders",
                "SELECT * FROM orders WHERE pk = 'a'"
            ]
        );
        assert_eq!(rest.scans, ScanApproval::AllowFirst);
    }

    #[test]
    fn opens_a_redis_key_with_its_types_read_command() {
        let mut consoles = Consoles::default();
        let database = |number: &str| CatalogPath::root().child(NodeKind::Database, number);
        let key = |number: &str, name: &str, key_type| {
            database(number).child(NodeKind::Key(key_type), name)
        };
        let cases = [
            (
                key("0", "user:42", dbrover_core::KeyType::Hash),
                vec!["HSCAN user:42 0 COUNT 1000"],
            ),
            (
                key("3", "\"a b\"", dbrover_core::KeyType::List),
                vec!["SELECT 3", "LRANGE \"a b\" 0 -1"],
            ),
        ];
        for (path, expected) in cases {
            let (_, command) = consoles.open_data(&redis("cache", "0"), &path).unwrap();
            let Command::Execute { statements, .. } = command else {
                panic!("expected a run, got {command:?}");
            };
            assert_eq!(statements, expected, "{path}");
        }
        assert!(
            consoles
                .open_data(&redis("cache", "0"), &database("0"))
                .is_none()
        );
    }

    #[test]
    fn saves_edits_once_they_settle() {
        let mut consoles = Consoles::default();
        let key = consoles.open(&data_source("shop"), "select 1".to_owned());
        let start = Instant::now();
        assert_eq!(consoles.due_saves(start).0.len(), 0, "nothing was edited");

        consoles.open.get_mut(&key).unwrap().edited(start);
        let (saves, next) = consoles.due_saves(start + SAVE_DELAY / 2);
        assert!(saves.is_empty());
        assert_eq!(next, Some(SAVE_DELAY / 2));

        let (saves, next) = consoles.due_saves(start + SAVE_DELAY);
        assert!(matches!(
            saves.as_slice(),
            [Command::SaveConsole { key: saved, text }] if *saved == key && text == "select 1"
        ));
        assert_eq!(next, None);
        assert!(consoles.unsaved().is_empty());
    }

    #[test]
    fn keeps_the_files_of_consoles_the_user_typed_in() {
        let mut consoles = Consoles::default();
        let source = data_source("shop");
        let untouched = consoles.open(&source, "SELECT * FROM t;".to_owned());
        let typed_in = consoles.open(&source, "select 2".to_owned());
        consoles
            .open
            .get_mut(&typed_in)
            .unwrap()
            .edited(Instant::now());

        let closed = consoles.close(&untouched);
        assert!(matches!(
            closed.as_slice(),
            [Command::CloseConsole(_), Command::DeleteConsoleFile(key)] if *key == untouched
        ));
        let closed = consoles.close(&typed_in);
        assert!(matches!(
            closed.as_slice(),
            [Command::CloseConsole(_), Command::SaveConsole { key, .. }] if *key == typed_in
        ));

        assert_eq!(
            consoles.saved().names(source.id).collect::<Vec<_>>(),
            [&typed_in.name]
        );
        assert_eq!(consoles.reopen(&source, &typed_in), Some(typed_in.clone()));
        assert_eq!(consoles.open[&typed_in].text(), "select 2");
        assert_eq!(consoles.saved().names(source.id).count(), 0);
    }

    #[test]
    fn new_consoles_skip_the_names_of_saved_files() {
        let mut consoles = Consoles::default();
        let source = data_source("shop");
        consoles.apply(&Event::ConsolesLoaded(vec![SavedConsole {
            key: named(&source, "console"),
            text: "select 1".to_owned(),
        }]));

        let key = consoles.open(&source, String::new());

        assert_eq!(key, named(&source, "console_2"));
    }

    #[test]
    fn titles_name_the_data_source_and_the_console() {
        let cases = [
            ("console", "shop"),
            ("console_3", "shop (3)"),
            ("report", "shop: report"),
        ];
        for (name, expected) in cases {
            let name = ConsoleName::try_from(name.to_owned()).unwrap();
            assert_eq!(title("shop", &name), expected);
        }
    }
}
