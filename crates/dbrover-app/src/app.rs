use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dbrover_core::{CatalogPath, ChildrenQuery, DataSource, DataSourceId, NodeKind, TableName};
use dbrover_engine::{
    Command, EngineConfig, EngineError, EngineHandle, Event, PasswordKept, Repaint, RunOrigin,
};
use dbrover_export::{ExportFormat, ExportSpec};
use dbrover_language::QueryLanguage;
use dbrover_store::{
    ActionBody, ActionId, ConsoleKey, CustomFileKind, DiagramFolder, DiagramKey, DiagramName,
    KeymapBindings, McpPermissions, SqlFileContents,
};
use egui::{Frame, Margin, RichText};
use egui_dock::{DockArea, DockState};

use crate::about_dialog;
use crate::command_runs::{CommandRuns, Finished};
use crate::commands::{self, AppCommand, CommandState};
use crate::console::{ConsoleAction, ExportedRows, FormatScope, RowsToOpen, RunDestination};
use crate::consoles::Consoles;
use crate::csv_import::{CsvImportAction, CsvImportDialog};
use crate::custom_actions::{self, Subject, SubjectKind, SubjectOn};
use crate::custom_files::{self, CustomFiles, FileWindowAction};
use crate::data_source_dialog::{DataSourceDialog, DialogAction};
use crate::data_source_form::McpPasswordChange;
use crate::definition_window::DefinitionWindow;
use crate::delete_dialog::{DeleteChoice, DeleteConfirmation};
use crate::diagram::{
    Diagram, DiagramAction, DiagramDialog, DiagramDialogAction, Diagrams, Growth,
};
use crate::explorer::{ExplorerAction, ExplorerOptions};
use crate::export_dialog::{
    self, ExportChoices, ExportDialog, ExportDialogAction, ExportPreview, ExportSubject,
    TableTarget,
};
use crate::export_sources_dialog::{ExportAction, ExportSourcesDialog};
use crate::file_picker;
use crate::history_window::{HistoryAction, HistoryWindow};
use crate::host_key_prompt::{HostKeyAction, HostKeyPrompt};
use crate::import_dialog::{ImportAction, ImportDialog};
use crate::keymap::{self, Keymap};
use crate::log_console::{LogConsole, LogFeed};
use crate::mcp_settings::McpDefaultsEditor;
use crate::menu_bar;
use crate::native_menu::{Chosen, NativeMenu};
use crate::object_dialog::{self, ObjectDialog, ObjectDialogAction, Target};
use crate::password_command_prompt::{PasswordCommandAction, PasswordCommandPrompt};
use crate::password_prompt::{PasswordPrompt, PromptAction};
use crate::picture::PictureFormat;
use crate::previous_install::AdoptionNotice;
use crate::project_dialog::{
    DeleteProjectChoice, DeleteProjectConfirmation, ProjectDialogAction, ProjectNameDialog,
};
use crate::project_settings::{ProjectSettingsAction, ProjectSettingsDialog};
use crate::rollback_dialog::{Closing, RollbackChoice, RollbackConfirmation};
use crate::settings::AppSettings;
use crate::settings_tab::{Page, SettingsAction, SettingsTab};
use crate::sql_files::{self, Destination, OpenFileAction, OpenFileDialog, SqlFiles};
use crate::state::{AppState, DataSources, EngineStatus};
use crate::system_look::SystemLook;
use crate::theme::{self, Palette};
use crate::title_bar;
use crate::unsaved_dialog::{UnsavedChoice, UnsavedConfirmation};
use crate::update_dialog::{self, UpdateAction};
use crate::updates::Updates;
use crate::workspace::{self, Panels, Tab};
use crate::{feedback, fonts, relaunch, repaint, welcome, widgets};

/// Where the dock layout is saved. Versioned so a change to the default layout reaches people
/// who already have one saved: layouts under an older key are ignored once, and the default
/// takes their place. Version 2: the explorer became a full-height column, and layouts saved
/// before it could give the explorer four fifths of the window. Open console tabs aren't
/// restored after a version change, but their files are, under Open saved console.
const LAYOUT_STORAGE_KEY: &str = "dock_layout_v2";

/// Asks where to export `what` ("rows", "history") in a format; `None` if cancelled.
type PickExportPath = dyn Fn(ExportFormat, &str) -> Option<PathBuf>;

/// How long typing in the explorer's filter must pause before a level the server pages is asked
/// for again with it. Chosen to sit between keystrokes of steady typing, and short enough that
/// the tree follows without seeming to wait.
const SERVER_FILTER_PAUSE: Duration = Duration::from_millis(300);

/// The explorer's filter text, and when it last changed, until the pause after that passes.
#[derive(Default)]
struct ServerFilter {
    text: String,
    changed: Option<Instant>,
}

/// Asks where to save a file, suggesting a name for it.
type PickSavePath = Box<dyn Fn(&str) -> Option<PathBuf>>;

/// Asks where to export a diagram named by the `&str` as a picture; `None` if cancelled.
type PickPicturePath = dyn Fn(&str, PictureFormat) -> Option<PathBuf>;

pub struct DbRoverApp {
    /// `None` once the engine has been shut down, at exit or after it stopped unexpectedly.
    engine: Option<EngineHandle>,
    state: AppState,
    dock: DockState<Tab>,
    log_console: LogConsole,
    explorer_options: ExplorerOptions,
    /// The explorer's filter as the server last saw it, or is about to.
    server_filter: ServerFilter,
    data_source_dialog: Option<DataSourceDialog>,
    delete_confirmation: Option<DeleteConfirmation>,
    project_dialog: Option<ProjectNameDialog>,
    project_settings: Option<ProjectSettingsDialog>,
    /// View › Settings › MCP's draft, kept while the window is closed.
    mcp_defaults: McpDefaultsEditor,
    settings_tab: SettingsTab,
    delete_project_confirmation: Option<DeleteProjectConfirmation>,
    /// The object helper being used: New table, Drop…, and so on.
    object_dialog: Option<ObjectDialog>,
    /// The table whose DDL Copy DDL is waiting for.
    copying_ddl: Option<(DataSourceId, CatalogPath)>,
    /// Text to put on the clipboard on the next frame, from an engine event.
    clipboard: Option<String>,
    definition_window: Option<DefinitionWindow>,
    history_window: Option<HistoryWindow>,
    rollback_confirmation: Option<RollbackConfirmation>,
    password_prompt: Option<PasswordPrompt>,
    host_key_prompt: Option<HostKeyPrompt>,
    password_command_prompt: Option<PasswordCommandPrompt>,
    export_sources_dialog: Option<ExportSourcesDialog>,
    export_dialog: Option<ExportDialog>,
    /// The export dialog's format and options, as last used.
    export_choices: ExportChoices,
    import_dialog: Option<ImportDialog>,
    csv_import: Option<CsvImportDialog>,
    /// Ask where to export data sources to and which file to import. The platform's file
    /// dialogs, except in tests.
    pick_data_sources_export: Box<dyn Fn() -> Option<PathBuf>>,
    pick_data_sources_import: Box<dyn Fn() -> Option<PathBuf>>,
    /// Set once quitting may roll back open transactions, so the close isn't asked again.
    quit_confirmed: bool,
    keymap: Keymap,
    /// What `keymap.toml` holds, kept to bind custom actions again when `actions.toml` changes.
    keymap_file: Result<KeymapBindings, String>,
    /// Where `keymap.toml` is, once the engine has reported it.
    keymap_path: Option<PathBuf>,
    custom_files: CustomFiles,
    /// `theme.toml` was read, and its colours are yet to be installed.
    theme_changed: bool,
    /// Why the last custom action couldn't run, shown in the status bar until the next one.
    action_problem: Option<String>,
    command_runs: CommandRuns,
    about_open: bool,
    /// What happened to the previous install's files on this first start, until dismissed.
    adoption_notice: Option<AdoptionNotice>,
    settings: AppSettings,
    updates: Updates,
    /// Asks where to export rows or the history (named by the `&str`). The platform's save
    /// dialog, except in tests.
    pick_export_path: Box<PickExportPath>,
    /// Asks for a CSV file to import into a table. A field so tests can answer it.
    pub(crate) pick_csv_file: Box<dyn Fn() -> Option<PathBuf>>,
    consoles: Consoles,
    diagrams: Diagrams,
    /// **Diagram…**, choosing a schema's tables.
    diagram_dialog: Option<DiagramDialog>,
    /// The `.sql` files consoles show, and the recent ones.
    sql_files: SqlFiles,
    open_file_dialog: Option<OpenFileDialog>,
    /// Files to open once the data sources are known: from the command line.
    files_to_open: Vec<PathBuf>,
    unsaved_confirmation: Option<UnsavedConfirmation>,
    /// A close waiting for these consoles' files to be written; a failed save cancels it.
    closing_after_saves: Option<(Closing, Vec<ConsoleKey>)>,
    /// Ask for a SQL file to open, and where to save a console. The platform's file dialogs,
    /// except in tests.
    pub(crate) pick_sql_file: Box<dyn Fn() -> Option<PathBuf>>,
    pub(crate) pick_sql_save_path: PickSavePath,
    /// Asks where to export a diagram's picture, suggesting a name. The platform's save
    /// dialog, except in tests.
    pub(crate) pick_picture: Box<PickPicturePath>,
    /// Set on first launch until the window has been asked to maximize. After that, eframe
    /// restores whatever size the user left it at.
    maximize_window: bool,
    /// The menu bar macOS draws, when `main` installed one; egui's is drawn only without it.
    native_menu: Option<NativeMenu>,
    /// The window's content runs under its title bar, so the app draws the top strip itself.
    integrated_title_bar: bool,
    /// The system's faces and accent colour, when `main` took them.
    system_look: Option<SystemLook>,
}

impl DbRoverApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        log_feed: LogFeed,
        engine_config: EngineConfig,
    ) -> Result<Self, EngineError> {
        let saved_layout: Option<DockState<Tab>> = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, LAYOUT_STORAGE_KEY));
        // No saved layout means a first launch (or a layout version change): maximize the
        // window rather than keep the small default size.
        let maximize_window = saved_layout.is_none();
        let dock = saved_layout.unwrap_or_else(workspace::default_layout);
        let export_choices = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, export_dialog::STORAGE_KEY))
            .unwrap_or_default();
        let remembered_files = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, sql_files::STORAGE_KEY))
            .unwrap_or_default();
        // Asked before the engine takes the config: it only looks at how this copy was
        // installed, so the update dialog knows at once whether to offer installing.
        let install_support = engine_config.updater.support();
        let engine = EngineHandle::start(engine_config, EguiRepaint(cc.egui_ctx.clone()))?;
        fonts::install(&cc.egui_ctx);
        theme::install(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);

        Ok(Self {
            engine: Some(engine),
            state: AppState::default(),
            dock,
            log_console: LogConsole::new(log_feed, &cc.egui_ctx),
            explorer_options: ExplorerOptions::default(),
            server_filter: ServerFilter::default(),
            data_source_dialog: None,
            delete_confirmation: None,
            project_dialog: None,
            project_settings: None,
            mcp_defaults: McpDefaultsEditor::new(&McpPermissions::default()),
            settings_tab: SettingsTab::default(),
            delete_project_confirmation: None,
            object_dialog: None,
            copying_ddl: None,
            clipboard: None,
            definition_window: None,
            history_window: None,
            rollback_confirmation: None,
            password_prompt: None,
            host_key_prompt: None,
            password_command_prompt: None,
            export_sources_dialog: None,
            export_dialog: None,
            export_choices,
            import_dialog: None,
            csv_import: None,
            pick_data_sources_export: Box::new(file_picker::pick_data_sources_export),
            pick_data_sources_import: Box::new(file_picker::pick_data_sources_import),
            quit_confirmed: false,
            pick_export_path: Box::new(export_dialog::pick_export_path),
            pick_csv_file: Box::new(file_picker::pick_csv_file),
            keymap: Keymap::default(),
            keymap_file: Ok(KeymapBindings::new()),
            keymap_path: None,
            custom_files: CustomFiles::default(),
            theme_changed: false,
            action_problem: None,
            command_runs: CommandRuns::default(),
            about_open: false,
            adoption_notice: None,
            settings: AppSettings::default(),
            updates: Updates::new(install_support),
            consoles: Consoles::default(),
            diagrams: Diagrams::default(),
            diagram_dialog: None,
            sql_files: SqlFiles::new(remembered_files),
            open_file_dialog: None,
            files_to_open: Vec::new(),
            unsaved_confirmation: None,
            closing_after_saves: None,
            pick_sql_file: Box::new(file_picker::pick_sql_file),
            pick_sql_save_path: Box::new(file_picker::pick_sql_save_path),
            pick_picture: Box::new(file_picker::pick_picture),
            maximize_window,
            native_menu: None,
            integrated_title_bar: false,
            system_look: None,
        })
    }

    /// Opens these files once the data sources and consoles are loaded: the ones named on the
    /// command line.
    pub fn with_files_to_open(mut self, paths: Vec<PathBuf>) -> Self {
        self.files_to_open = paths;
        self
    }

    /// Tells the user once what happened to the previous install's files (see
    /// `previous_install`).
    pub fn with_adoption_notice(mut self, notice: Option<AdoptionNotice>) -> Self {
        self.adoption_notice = notice;
        self
    }

    /// Uses the operating system's menu bar instead of drawing egui's, if there is one.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "only macOS has a native menu bar")
    )]
    pub fn with_native_menu(mut self, menu: Option<NativeMenu>) -> Self {
        self.native_menu = menu;
        self
    }

    /// Draws the strip under a transparent title bar: `main` makes the title bar so on macOS.
    #[cfg_attr(
        not(any(target_os = "macos", test)),
        expect(dead_code, reason = "only macOS integrates the title bar")
    )]
    pub fn with_integrated_title_bar(mut self) -> Self {
        self.integrated_title_bar = true;
        self
    }

    /// Draws with the system's faces and accent colour, following changes to the accent.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "only macOS has a system look DbRover takes")
    )]
    pub fn with_system_look(mut self, look: SystemLook) -> Self {
        self.system_look = Some(look);
        self
    }

    /// Makes the export and import file dialogs answer `path`, for tests and screenshots.
    #[cfg(test)]
    pub fn answer_file_dialogs_with(&mut self, path: &std::path::Path) {
        let export = path.to_owned();
        let import = path.to_owned();
        self.pick_data_sources_export = Box::new(move || Some(export.clone()));
        self.pick_data_sources_import = Box::new(move || Some(import.clone()));
    }

    /// The open diagrams, for tests that look at what a diagram holds.
    #[cfg(test)]
    pub fn open_diagrams(&self) -> impl Iterator<Item = (&DiagramKey, &Diagram)> {
        self.diagrams.iter()
    }

    /// Where a diagram's table was last drawn on screen.
    #[cfg(test)]
    pub fn diagram_table_on_screen(&self, key: &DiagramKey, table: &str) -> Option<egui::Rect> {
        self.diagrams.screen_rect(key, table)
    }

    /// The open consoles, for tests that look at what a console drew.
    #[cfg(test)]
    pub fn open_consoles(&self) -> impl Iterator<Item = &crate::console::QueryConsole> {
        self.consoles.open_consoles()
    }

    fn drain_engine_events(&mut self) {
        loop {
            let Some(engine) = &mut self.engine else {
                return;
            };
            match engine.try_recv() {
                Ok(Some(event)) => self.apply(&event),
                Ok(None) => return,
                Err(error) => {
                    tracing::error!("{error}");
                    self.shut_down_engine();
                    return;
                }
            }
        }
    }

    fn apply(&mut self, event: &Event) {
        self.state.apply(event);
        match event {
            Event::ProjectsLoaded(projects) => self.mcp_defaults.follow(&projects.mcp_defaults),
            Event::McpDefaultsSaved(defaults) => self.mcp_defaults.follow(defaults),
            Event::McpDefaultsSaveFailed { error } => self.mcp_defaults.failed(error),
            _ => {}
        }
        if self
            .data_source_dialog
            .as_mut()
            .is_some_and(|dialog| dialog.apply(event))
        {
            self.data_source_dialog = None;
        }
        if self
            .project_dialog
            .as_mut()
            .is_some_and(|dialog| dialog.apply(event))
        {
            self.project_dialog = None;
        }
        if self
            .project_settings
            .as_mut()
            .is_some_and(|dialog| dialog.apply(event))
        {
            self.project_settings = None;
        }
        if let Some(changed) = self
            .object_dialog
            .as_mut()
            .and_then(|dialog| dialog.apply(event))
            && let Some(dialog) = self.object_dialog.take()
        {
            self.show_change(dialog.data_source(), changed);
        }
        self.apply_to_copied_ddl(event);
        if let Some(Finished::OpenConsole { data_source, text }) = self.command_runs.apply(event) {
            self.open_program_output(data_source, text);
        }
        if let Some(window) = &mut self.definition_window {
            window.apply(event);
        }
        if let Some(window) = &mut self.history_window {
            window.apply(event);
        }
        // The run's statements are recorded by now; show them.
        if matches!(event, Event::RunFinished { .. })
            && let Some(window) = &mut self.history_window
        {
            let search = window.search_command();
            self.send(search);
        }
        self.apply_to_password_prompt(event);
        self.apply_to_host_key_prompt(event);
        self.apply_to_password_command_prompt(event);
        if let Some(dialog) = &mut self.export_sources_dialog {
            dialog.apply(event);
        }
        if let Some(dialog) = &mut self.import_dialog {
            dialog.apply(event);
        }
        if let Some(dialog) = &mut self.csv_import {
            dialog.apply(event);
        }
        if let Some(dialog) = &mut self.export_dialog {
            dialog.apply(event);
        }
        let command = self.consoles.apply(event);
        self.send_if(command);
        let batches = self.diagrams.apply(event);
        self.send_all(batches);
        self.apply_to_sql_files(event);
        self.settings.apply(event);
        self.updates.apply(event);
        if matches!(event, Event::SettingsLoaded { .. }) {
            let check = self.updates.start(self.settings.settings());
            self.send_if(check);
        }
        match self.custom_files.apply(event) {
            Some(CustomFileKind::Actions) => self.rebuild_keymap(),
            Some(CustomFileKind::Theme) => self.theme_changed = true,
            Some(CustomFileKind::Snippets) | None => {}
        }
        if let Event::KeymapLoaded { path, result } = event {
            self.keymap_file = result.clone();
            self.keymap_path = Some(path.clone());
            self.rebuild_keymap();
        }
        if matches!(event, Event::DiagramsLoaded(_)) {
            self.restore_diagram_tabs();
        }
        if matches!(event, Event::ConsolesLoaded(_)) {
            self.restore_console_tabs();
            for path in std::mem::take(&mut self.files_to_open) {
                self.open_sql_file(path);
            }
        }
    }

    fn apply_to_sql_files(&mut self, event: &Event) {
        match event {
            Event::SqlFileRead { path, result } => self.sql_file_read(path, result),
            Event::SqlFileSaved {
                console, result, ..
            } => self.close_once_saved(console, result.is_ok()),
            _ => {}
        }
    }

    /// Shows a file just read where it was asked for: in a new console, or in a tab restored
    /// from the last session.
    fn sql_file_read(&mut self, path: &Path, result: &Result<SqlFileContents, String>) {
        for destination in self.sql_files.read(path) {
            match (destination, result) {
                (Destination::NewConsole(id), Ok(contents)) => {
                    if let Some(key) = self.consoles.showing_file(path) {
                        workspace::focus_console_tab(&mut self.dock, &key);
                        continue;
                    }
                    let Some(entry) = self.state.data_source(id) else {
                        continue;
                    };
                    let tabs = workspace::console_tabs(&self.dock);
                    let key = self.consoles.open_file(
                        &entry.data_source,
                        path.to_owned(),
                        contents.clone(),
                        |key| tabs.contains(key),
                    );
                    self.sql_files.remember(path.to_owned(), id);
                    workspace::open_console_tab(&mut self.dock, key);
                }
                (Destination::NewConsole(_), Err(error)) => {
                    self.sql_files.forget(path);
                    self.open_file_dialog = Some(OpenFileDialog::Failed {
                        error: error.clone(),
                    });
                }
                (Destination::Restored(key), Ok(contents)) => {
                    match self.state.data_source(key.data_source) {
                        Some(entry) => self.consoles.reopen_file(
                            &entry.data_source,
                            key,
                            path.to_owned(),
                            contents.clone(),
                        ),
                        None => workspace::close_console_tab(&mut self.dock, &key),
                    }
                }
                // The engine logged why; the tab goes, as for a console file that is gone.
                (Destination::Restored(key), Err(_)) => {
                    workspace::close_console_tab(&mut self.dock, &key);
                }
            }
        }
    }

    /// Asks for a SQL file, and opens it.
    fn choose_sql_file(&mut self) {
        if let Some(path) = (self.pick_sql_file)() {
            self.open_sql_file(path);
        }
    }

    /// Opens `path` in a console: the one already showing it, or a new one on the data source
    /// it was last opened on (or the only one there is). Otherwise asks which data source.
    fn open_sql_file(&mut self, path: PathBuf) {
        if let Some(key) = self.consoles.showing_file(&path) {
            workspace::focus_console_tab(&mut self.dock, &key);
            return;
        }
        let focused = self.focused_console().map(|key| key.data_source);
        let entries = self.state.data_sources.entries();
        let remembered = self
            .sql_files
            .data_source_of(&path, |id| self.state.data_source(id).is_some());
        let only = match entries {
            [only] => Some(only.data_source.id),
            _ => None,
        };
        let data_source = remembered.or(only);
        match data_source {
            Some(id) => self.read_sql_file(path, id),
            None => {
                let chosen = focused.or_else(|| entries.first().map(|entry| entry.data_source.id));
                self.open_file_dialog = Some(OpenFileDialog::ChooseDataSource { path, chosen });
            }
        }
    }

    fn read_sql_file(&mut self, path: PathBuf, data_source: DataSourceId) {
        self.sql_files
            .reading(path.clone(), Destination::NewConsole(data_source));
        self.send(Command::OpenSqlFile(path));
    }

    fn show_open_file_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.open_file_dialog else {
            return;
        };
        let data_sources: Vec<&DataSource> = self
            .state
            .data_sources
            .entries()
            .iter()
            .map(|entry| &entry.data_source)
            .collect();
        let Some(action) = dialog.show(ctx, &data_sources) else {
            return;
        };
        self.open_file_dialog = None;
        if let OpenFileAction::Open(path, data_source) = action {
            self.read_sql_file(path, data_source);
        }
    }

    /// Writes the console to its `.sql` file, or asks where if it has none.
    fn save_console(&mut self, key: &ConsoleKey) {
        match self.consoles.save_file(key) {
            Some(save) => self.send(save),
            None => self.save_console_as(key),
        }
    }

    /// Asks where to write the console, which then shows that file.
    fn save_console_as(&mut self, key: &ConsoleKey) {
        let Some(console) = self.consoles.get_mut(key) else {
            return;
        };
        let suggested = match console.file_path() {
            Some(_) => console.title().to_owned(),
            None => format!("{}.sql", console.title()),
        };
        let Some(path) = (self.pick_sql_save_path)(&suggested) else {
            return;
        };
        if let Some(save) = self.consoles.save_file_as(key, path.clone()) {
            self.send(save);
            self.sql_files.remember(path, key.data_source);
        }
    }

    /// Looks at the open files for changes other programs made, when it is time to.
    fn check_sql_files(&mut self, ctx: &egui::Context) {
        let files = self.consoles.watched_files();
        if files.is_empty() {
            return;
        }
        // A window whose focus isn't known (in tests) counts as focused.
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
        if self.sql_files.check_due(Instant::now(), focused) {
            self.send(Command::CheckSqlFiles(files));
        }
        ctx.request_repaint_after(sql_files::CHECK_EVERY);
    }

    /// Opens the files dropped on the window.
    fn open_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_owned())
                .collect()
        });
        for path in dropped {
            self.open_sql_file(path);
        }
    }

    /// The keys that open and save files, wherever the focus is.
    fn file_shortcuts(&mut self, ctx: &egui::Context) {
        let triggered = ctx.input_mut(|input| {
            self.keymap.consume(
                input,
                &[
                    keymap::Action::OpenFile,
                    keymap::Action::Save,
                    keymap::Action::SaveAs,
                    keymap::Action::Settings,
                ],
            )
        });
        for command in triggered.into_iter().filter_map(AppCommand::for_action) {
            self.run_command(command, ctx);
        }
    }

    fn rebuild_keymap(&mut self) {
        let custom: Vec<&ActionId> = self
            .custom_files
            .actions()
            .iter()
            .map(|action| &action.id)
            .collect();
        self.keymap = match &self.keymap_file {
            Ok(bindings) => Keymap::from_file(bindings, &custom),
            Err(error) => Keymap::unreadable(error.clone()),
        };
    }

    /// The keys `keymap.toml` gives custom actions, wherever the focus is.
    fn custom_action_shortcuts(&mut self, ctx: &egui::Context) {
        let triggered = ctx.input_mut(|input| self.keymap.consume_custom(input));
        for action in triggered {
            self.run_action_on_selection(&action);
        }
    }

    /// Runs a custom action from a key or the Actions menu, on what is selected: the focused
    /// console's cells, or else the explorer's node.
    fn run_action_on_selection(&mut self, id: &ActionId) {
        let subject = self.action_subject();
        let Some(action) = self
            .custom_files
            .actions()
            .iter()
            .find(|action| action.id == *id)
        else {
            return;
        };
        match subject {
            Some((data_source, subject, kind)) if kind.offers(action) => {
                self.run_action(id, data_source, &subject);
            }
            _ => {
                let targets: Vec<&str> = action
                    .applies_to
                    .iter()
                    .map(|target| target.name())
                    .collect();
                self.action_problem = Some(format!(
                    "{}: select something it applies to first ({})",
                    action.name,
                    targets.join(", ")
                ));
            }
        }
    }

    /// What keys and the Actions menu act on: the focused console's selected cells, or else
    /// the explorer's selected node.
    fn action_subject(&mut self) -> Option<(DataSourceId, Subject, SubjectKind)> {
        let kind = self.action_subject_kind()?;
        let (data_source, subject) = match kind.on {
            SubjectOn::Cells => {
                let key = self.focused_console()?;
                let cells = self.consoles.get_mut(&key)?.selected_cells()?;
                (key.data_source, Subject::Cells(cells))
            }
            SubjectOn::Node(_) => {
                let (data_source, path) = self.explorer_options.selected.clone()?;
                (data_source, Subject::Node(path))
            }
        };
        Some((data_source, subject, kind))
    }

    /// What [`Self::action_subject`] would act on, without copying the cells: the menus ask
    /// every frame.
    fn action_subject_kind(&mut self) -> Option<SubjectKind> {
        let cells = self.focused_console().filter(|key| {
            self.consoles
                .get_mut(key)
                .is_some_and(|console| console.has_selected_cells())
        });
        let (data_source, on) = match cells {
            Some(key) => (key.data_source, SubjectOn::Cells),
            None => {
                let (data_source, path) = self.explorer_options.selected.as_ref()?;
                (
                    *data_source,
                    SubjectOn::Node(custom_actions::node_target(path)?),
                )
            }
        };
        let driver = self
            .state
            .data_source(data_source)?
            .data_source
            .config
            .driver();
        Some(SubjectKind { driver, on })
    }

    /// Puts the DDL Copy DDL asked for on the clipboard once it arrives.
    fn apply_to_copied_ddl(&mut self, event: &Event) {
        let Event::DdlLoaded {
            data_source,
            path,
            result,
        } = event
        else {
            return;
        };
        if self.copying_ddl.as_ref() != Some(&(*data_source, path.clone())) {
            return;
        }
        self.copying_ddl = None;
        match result {
            Ok(ddl) => {
                tracing::info!("copied the DDL of {path}");
                self.clipboard = Some(ddl.clone());
            }
            Err(error) => tracing::error!("could not copy the DDL of {path}: {error}"),
        }
    }

    /// Opens the prompt when a connection needs a password, unless one is open already (the
    /// explorer and a console may both have asked), and connects again once it is given.
    fn apply_to_password_prompt(&mut self, event: &Event) {
        if let Event::PasswordNeeded {
            data_source,
            secret,
            reason,
        } = event
            && self.password_prompt.is_none()
            && let Some(entry) = self.state.data_source(*data_source)
        {
            self.password_prompt = Some(PasswordPrompt::new(
                &entry.data_source,
                *secret,
                reason.clone(),
            ));
        }
        let Some(prompt) = &self.password_prompt else {
            return;
        };
        let Some(kept) = prompt.apply(event) else {
            return;
        };
        let data_source = prompt.data_source();
        self.password_prompt = None;
        if let PasswordKept::SaveFailed(error) = kept {
            tracing::warn!(
                "the password was not saved to the keychain, so it is kept until DbRover quits: \
                 {error}"
            );
        }
        self.send(Command::Connect(data_source));
    }

    /// Opens the prompt when a connection meets an SSH server whose host key isn't trusted
    /// yet, and connects again once the user has trusted it.
    fn apply_to_host_key_prompt(&mut self, event: &Event) {
        if let Event::HostKeyUnknown { data_source, key } = event
            && self.host_key_prompt.is_none()
            && let Some(entry) = self.state.data_source(*data_source)
        {
            self.host_key_prompt = Some(HostKeyPrompt::new(&entry.data_source, key.clone()));
        }
        let Some(prompt) = &mut self.host_key_prompt else {
            return;
        };
        if !prompt.apply(event) {
            return;
        }
        let data_source = prompt.data_source();
        self.host_key_prompt = None;
        self.send(Command::Connect(data_source));
    }

    /// Opens the prompt when a connection's password command hasn't been trusted yet, and
    /// connects again once the user has trusted it.
    fn apply_to_password_command_prompt(&mut self, event: &Event) {
        if let Event::PasswordCommandUntrusted {
            data_source,
            command,
        } = event
            && self.password_command_prompt.is_none()
            && let Some(entry) = self.state.data_source(*data_source)
        {
            self.password_command_prompt = Some(PasswordCommandPrompt::new(
                &entry.data_source,
                command.clone(),
            ));
        }
        let Some(prompt) = &self.password_command_prompt else {
            return;
        };
        if !prompt.apply(event) {
            return;
        }
        let data_source = prompt.data_source();
        self.password_command_prompt = None;
        self.send(Command::Connect(data_source));
    }

    fn show_password_command_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.password_command_prompt else {
            return;
        };
        let data_source = prompt.data_source();
        match prompt.show(ctx) {
            Some(PasswordCommandAction::Trust(command)) => {
                self.send(Command::TrustPasswordCommand {
                    data_source,
                    command,
                });
            }
            Some(PasswordCommandAction::Cancel) => self.password_command_prompt = None,
            None => {}
        }
    }

    fn show_host_key_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.host_key_prompt else {
            return;
        };
        let data_source = prompt.data_source();
        match prompt.show(ctx) {
            Some(HostKeyAction::Trust(key)) => {
                self.send(Command::TrustHostKey { data_source, key });
            }
            Some(HostKeyAction::Cancel) => self.host_key_prompt = None,
            None => {}
        }
    }

    fn show_password_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.password_prompt else {
            return;
        };
        let data_source = prompt.data_source();
        let secret = prompt.secret();
        let Some(action) = prompt.show(ctx) else {
            return;
        };
        match action {
            PromptAction::Provide { password, save } => self.send(Command::ProvidePassword {
                data_source,
                secret,
                password,
                save,
            }),
            PromptAction::Cancel => self.password_prompt = None,
        }
    }

    /// Reopens the consoles whose tabs the saved layout holds. The engine reports the data
    /// sources before the console files, so both are known by now.
    /// A tab that showed a `.sql` file stays, showing "Loading…", while the file is read.
    fn restore_console_tabs(&mut self) {
        let Self {
            dock,
            consoles,
            state,
            sql_files,
            ..
        } = self;
        let mut reads = Vec::new();
        workspace::restore_consoles(dock, |key| {
            if let Some(path) = sql_files.restore(key) {
                reads.push(Command::OpenSqlFile(path));
                return true;
            }
            state
                .data_source(key.data_source)
                .and_then(|entry| consoles.reopen(&entry.data_source, key))
                .is_some()
        });
        self.send_all(reads);
    }

    /// Reopens the diagram tabs the saved layout holds, now that their files are read.
    fn restore_diagram_tabs(&mut self) {
        let Self {
            dock,
            diagrams,
            state,
            ..
        } = self;
        let mut first_batches = Vec::new();
        workspace::restore_diagrams(dock, |key| {
            let data_source_exists = diagrams
                .data_source_of(key)
                .is_some_and(|id| state.data_source(id).is_some());
            if !data_source_exists {
                return false;
            }
            first_batches.extend(diagrams.reopen(key));
            diagrams.is_open(key)
        });
        self.send_all(first_batches);
    }

    fn save_diagrams(&mut self, ctx: &egui::Context) {
        let (saves, next_due) = self.diagrams.due_saves(Instant::now());
        self.send_all(saves);
        if let Some(delay) = next_due {
            repaint::request_repaint_after_pause(ctx, delay);
        }
    }

    fn handle_diagram_action(&mut self, action: DiagramAction) {
        match action {
            DiagramAction::Delete(key) => {
                self.diagrams.forget(&key);
                workspace::close_diagram_tab(&mut self.dock, &key);
                self.send(Command::DeleteDiagram(key));
            }
            DiagramAction::Export(key, format) => {
                let Some(path) = (self.pick_picture)(key.name.as_str(), format) else {
                    return;
                };
                let export = self.diagrams.export(&key, format, &path);
                self.send_if(export);
            }
        }
    }

    /// Asks where to write the console's chart as `format`, then writes it.
    fn export_chart(&mut self, key: &ConsoleKey, format: PictureFormat) {
        let Some(name) = self.consoles.get_mut(key).and_then(|c| c.chart_file_name()) else {
            return;
        };
        let Some(path) = (self.pick_picture)(&name, format) else {
            return;
        };
        let export = self
            .consoles
            .get_mut(key)
            .and_then(|console| console.export_chart(format, &path));
        self.send_if(export);
    }

    fn send_all(&self, commands: impl IntoIterator<Item = Command>) {
        for command in commands {
            self.send(command);
        }
    }

    /// Writes the consoles whose edits have settled, and wakes up when the next ones will.
    fn save_consoles(&mut self, ctx: &egui::Context) {
        let (saves, next_due) = self.consoles.due_saves(Instant::now());
        self.send_all(saves);
        if let Some(delay) = next_due {
            repaint::request_repaint_after_pause(ctx, delay);
        }
    }

    fn send_if(&self, command: Option<Command>) {
        if let Some(command) = command {
            self.send(command);
        }
    }

    fn send(&self, command: Command) {
        let Some(engine) = &self.engine else {
            tracing::warn!("the engine is not running; ignoring {command:?}");
            return;
        };
        if let Err(error) = engine.send(command) {
            tracing::error!("{error}");
        }
    }

    /// Maximizes the window, once, on first launch.
    ///
    /// Why not size it from the monitor: with several screens, egui reported another screen's
    /// size than the one the window opened on (a laptop's 1512×982 for a window on a 2560×1440
    /// display), so the window came out barely bigger and on the wrong screen. Maximizing lets
    /// the OS fill whichever screen the window is on.
    fn maximize_window_once(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.maximize_window) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
    }

    fn shut_down_engine(&mut self) {
        if let Some(engine) = self.engine.take()
            && let Err(error) = engine.shutdown()
        {
            tracing::error!("engine shutdown failed: {:#}", anyhow::Error::new(error));
        }
        self.state.engine_status = EngineStatus::Stopped;
    }

    fn handle_explorer_action(&mut self, action: ExplorerAction, ctx: &egui::Context) {
        match action {
            ExplorerAction::New(driver) => {
                self.data_source_dialog = Some(DataSourceDialog::create(driver));
            }
            ExplorerAction::Edit(id) => {
                if let Some(entry) = self.state.data_source(id) {
                    self.data_source_dialog = Some(DataSourceDialog::edit(&entry.data_source));
                }
            }
            ExplorerAction::Delete(id) => {
                if let Some(entry) = self.state.data_source(id) {
                    self.delete_confirmation = Some(DeleteConfirmation::new(&entry.data_source));
                }
            }
            ExplorerAction::Connect(id) => self.send(Command::Connect(id)),
            ExplorerAction::Disconnect(id) => self.close_connections(Closing::Disconnect(id), ctx),
            ExplorerAction::Expand(id, path) => {
                let needs_loading = self
                    .state
                    .catalog_mut(id)
                    .is_some_and(|catalog| catalog.expand(&path));
                self.load_children_if(needs_loading, id, path);
            }
            ExplorerAction::Collapse(id, path) => {
                if let Some(catalog) = self.state.catalog_mut(id) {
                    catalog.collapse(&path);
                }
            }
            ExplorerAction::LoadMore(id, path) => {
                let more = self
                    .state
                    .catalog_mut(id)
                    .and_then(|catalog| catalog.load_more(&path));
                if let Some(query) = more {
                    self.send(Command::LoadChildren { id, path, query });
                }
            }
            ExplorerAction::Refresh(id, path) => {
                let needs_loading = self
                    .state
                    .catalog_mut(id)
                    .is_some_and(|catalog| catalog.refresh(&path));
                self.load_children_if(needs_loading, id, path);
            }
            ExplorerAction::ShowDefinition { id, path, title } => {
                self.definition_window = Some(DefinitionWindow::new(id, path.clone(), title));
                self.send(Command::LoadDefinition { id, path });
            }
            ExplorerAction::NewConsole(id) => {
                if let Some(entry) = self.state.data_source(id) {
                    let console = self.consoles.open(&entry.data_source, String::new());
                    workspace::open_console_tab(&mut self.dock, console);
                }
            }
            ExplorerAction::OpenSavedConsole(key) => {
                let reopened = self
                    .state
                    .data_source(key.data_source)
                    .and_then(|entry| self.consoles.reopen(&entry.data_source, &key));
                if let Some(console) = reopened {
                    workspace::open_console_tab(&mut self.dock, console);
                }
            }
            ExplorerAction::OpenData(id, path) => {
                let opened = self
                    .state
                    .data_source(id)
                    .and_then(|entry| self.consoles.open_data(&entry.data_source, &path));
                if let Some((console, command)) = opened {
                    workspace::open_console_tab(&mut self.dock, console);
                    self.send(command);
                }
            }
            ExplorerAction::ImportCsv(id, path) => self.open_csv_import(id, &path),
            ExplorerAction::ExportData(id, path) => self.open_table_export(id, &path),
            ExplorerAction::Diagram(id, path) => self.open_diagram_dialog(id, &path),
            ExplorerAction::OpenDiagram(key) => {
                let first = self.diagrams.reopen(&key);
                if self.diagrams.is_open(&key) {
                    workspace::open_diagram_tab(&mut self.dock, key);
                }
                self.send_if(first);
            }
            ExplorerAction::DiagramRelated(id, path) => {
                let schema = path.name_of(NodeKind::Schema).map(str::to_owned);
                if let (Some(schema), Some(table)) = (schema, path.last()) {
                    let start = TableName {
                        schema: schema.clone(),
                        name: table.name.clone(),
                    };
                    let name = DiagramName::from_text(&table.name);
                    self.open_diagram(id, schema, vec![start], Some(Growth::Related), &name);
                }
            }
            ExplorerAction::Select(id, path) => self.explorer_options.selected = Some((id, path)),
            ExplorerAction::RunAction {
                data_source,
                path,
                action,
            } => self.run_action(&action, data_source, &Subject::Node(path)),
            ExplorerAction::NewProject { then_move } => {
                self.project_dialog = Some(ProjectNameDialog::create(then_move));
            }
            ExplorerAction::RenameProject(id) => {
                if let Some(project) = self.state.projects.get(id) {
                    self.project_dialog = Some(ProjectNameDialog::rename(project));
                }
            }
            ExplorerAction::ProjectSettings(id) => {
                if let Some(project) = self.state.projects.get(id) {
                    self.project_settings = Some(ProjectSettingsDialog::new(
                        project,
                        &self.state.mcp_defaults,
                    ));
                }
            }
            ExplorerAction::DeleteProject(id) => {
                if let Some(project) = self.state.projects.get(id) {
                    let members = self
                        .state
                        .data_sources
                        .entries()
                        .iter()
                        .filter(|entry| entry.data_source.project == Some(id))
                        .count();
                    self.delete_project_confirmation =
                        Some(DeleteProjectConfirmation::new(project, members));
                }
            }
            ExplorerAction::MoveToProject(data_source, project) => {
                self.send(Command::MoveToProject {
                    data_source,
                    project,
                });
            }
            ExplorerAction::CopyDdl(data_source, path) => {
                self.copying_ddl = Some((data_source, path.clone()));
                self.send(Command::LoadDdl { data_source, path });
            }
            ExplorerAction::Helper(id, helper) => {
                if let Some(entry) = self.state.data_source(id) {
                    let Some(target) = Target::new(&entry.data_source) else {
                        return;
                    };
                    let Some(form) = object_dialog::form(helper, target.dialect) else {
                        return;
                    };
                    let dialog = ObjectDialog::new(target, form);
                    if let Some(path) = dialog.definition_request() {
                        self.send(Command::LoadDefinition { id, path });
                    }
                    if let Some(path) = dialog.role_request() {
                        self.send(Command::DescribeRole {
                            data_source: id,
                            path,
                        });
                    }
                    if let Some(path) = dialog.table_request() {
                        self.send(Command::LoadTable {
                            data_source: id,
                            path,
                        });
                    }
                    self.object_dialog = Some(dialog);
                }
            }
        }
    }

    fn show_object_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.object_dialog else {
            return;
        };
        let id = dialog.data_source();
        let Some(entry) = self.state.data_source(id) else {
            self.object_dialog = None;
            return;
        };
        let action = dialog.show(ctx, &entry.catalog);
        let requests = dialog.catalog_requests();
        self.load_catalog(id, requests);
        let Some(action) = action else {
            return;
        };
        match action {
            ObjectDialogAction::Execute {
                data_source,
                statements,
            } => self.send(Command::ApplyDdl {
                data_source,
                statements,
            }),
            ObjectDialogAction::OpenInConsole { data_source, text } => {
                self.object_dialog = None;
                if let Some(entry) = self.state.data_source(data_source) {
                    let console = self.consoles.open(&entry.data_source, text);
                    workspace::open_console_tab(&mut self.dock, console);
                }
            }
            ObjectDialogAction::Close => self.object_dialog = None,
        }
    }

    /// Shows what a helper changed: reloads the node whose children changed, and opens it and
    /// every node above it, loading those that never were (a new index's table may have been
    /// closed).
    fn show_change(&mut self, id: DataSourceId, changed: CatalogPath) {
        let Some(catalog) = self.state.catalog_mut(id) else {
            return;
        };
        let refreshed = catalog.refresh(&changed);
        let mut ancestors = Vec::new();
        let mut path = changed.parent();
        while let Some(ancestor) = path {
            path = ancestor.parent();
            ancestors.push(ancestor);
        }
        let mut loads: Vec<CatalogPath> = ancestors
            .into_iter()
            .rev()
            .filter(|ancestor| catalog.expand(ancestor))
            .collect();
        if catalog.expand(&changed) || refreshed {
            loads.push(changed);
        }
        for path in loads {
            self.send(Command::LoadChildren {
                id,
                path,
                query: ChildrenQuery::default(),
            });
        }
    }

    /// Passes the explorer's filter to the server for the levels it pages, once typing has
    /// paused, so each keystroke doesn't send a catalog query per paged level.
    fn filter_paged_levels(&mut self, ctx: &egui::Context) {
        let typed = self.explorer_options.filter.trim();
        let now = Instant::now();
        if typed != self.server_filter.text {
            self.server_filter = ServerFilter {
                text: typed.to_owned(),
                changed: Some(now),
            };
        }
        if let Some(changed) = self.server_filter.changed {
            if let Some(left) = SERVER_FILTER_PAUSE.checked_sub(now.duration_since(changed)) {
                repaint::request_repaint_after_pause(ctx, left);
                return;
            }
            self.server_filter.changed = None;
        }
        let filter = Some(self.server_filter.text.as_str()).filter(|text| !text.is_empty());
        let DataSources::Loaded(entries) = &mut self.state.data_sources else {
            return;
        };
        let mut requests = Vec::new();
        for entry in entries {
            let id = entry.data_source.id;
            requests.extend(
                entry
                    .catalog
                    .filter_paged_levels(filter)
                    .into_iter()
                    .map(|(path, query)| Command::LoadChildren { id, path, query }),
            );
        }
        for request in requests {
            self.send(request);
        }
    }

    /// Loads catalog levels completion asked for, without expanding them in the explorer.
    fn load_catalog(&mut self, id: DataSourceId, paths: Vec<CatalogPath>) {
        for path in paths {
            let needs_loading = self
                .state
                .catalog_mut(id)
                .is_some_and(|catalog| catalog.request(&path));
            self.load_children_if(needs_loading, id, path);
        }
    }

    fn load_children_if(&self, needs_loading: bool, id: DataSourceId, path: CatalogPath) {
        if needs_loading {
            self.send(Command::LoadChildren {
                id,
                path,
                query: ChildrenQuery::default(),
            });
        }
    }

    fn show_data_source_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self
            .data_source_dialog
            .as_mut()
            .and_then(|dialog| dialog.show(ctx))
        else {
            return;
        };
        match action {
            DialogAction::Test {
                data_source,
                password,
                ssh_secret,
            } => self.send(Command::TestConnection {
                data_source,
                password,
                ssh_secret,
            }),
            DialogAction::Save {
                data_source,
                password,
                ssh_secret,
                mcp_password,
            } => {
                let id = data_source.id;
                self.send(Command::SaveDataSource {
                    data_source,
                    password,
                    ssh_secret,
                });
                let password = match mcp_password {
                    McpPasswordChange::Keep => return,
                    McpPasswordChange::Set(password) => Some(password),
                    McpPasswordChange::Remove => None,
                };
                self.send(Command::SaveMcpPassword { id, password });
            }
            DialogAction::CreateDatabaseFile(file) => {
                self.send(Command::CreateDatabaseFile(file));
            }
            DialogAction::Close => self.data_source_dialog = None,
        }
    }

    fn show_project_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self
            .project_dialog
            .as_mut()
            .and_then(|dialog| dialog.show(ctx))
        else {
            return;
        };
        match action {
            ProjectDialogAction::Create {
                id,
                name,
                then_move,
            } => {
                self.send(Command::CreateProject { id, name });
                // Commands are handled in order, so the project exists by the time this runs.
                if let Some(data_source) = then_move {
                    self.send(Command::MoveToProject {
                        data_source,
                        project: Some(id),
                    });
                }
            }
            ProjectDialogAction::Rename { id, name } => {
                self.send(Command::RenameProject { id, name });
            }
            ProjectDialogAction::Close => self.project_dialog = None,
        }
    }

    fn show_project_settings(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.project_settings else {
            return;
        };
        let members: Vec<&DataSource> = self
            .state
            .data_sources
            .entries()
            .iter()
            .map(|entry| &entry.data_source)
            .filter(|data_source| data_source.project == Some(dialog.project()))
            .collect();
        let Some(action) = dialog.show(ctx, &self.state.mcp_defaults, &members) else {
            return;
        };
        match action {
            ProjectSettingsAction::Save {
                id,
                mcp_enabled,
                overrides,
            } => self.send(Command::SaveProjectMcp {
                id,
                mcp_enabled,
                overrides,
            }),
            ProjectSettingsAction::Close => self.project_settings = None,
        }
    }

    fn show_delete_project_confirmation(&mut self, ctx: &egui::Context) {
        let Some(choice) = self
            .delete_project_confirmation
            .as_ref()
            .and_then(|confirmation| confirmation.show(ctx))
        else {
            return;
        };
        self.delete_project_confirmation = None;
        if let DeleteProjectChoice::Confirmed(id) = choice {
            self.send(Command::DeleteProject(id));
        }
    }

    fn show_delete_confirmation(&mut self, ctx: &egui::Context) {
        let Some(choice) = self
            .delete_confirmation
            .as_ref()
            .and_then(|confirmation| confirmation.show(ctx))
        else {
            return;
        };
        self.delete_confirmation = None;
        if let DeleteChoice::Confirmed(id) = choice {
            self.send(Command::DeleteDataSource(id));
        }
    }

    /// Does what `closing` asks, first asking whether to save the files it would close with
    /// unsaved changes, then whether to roll back the transactions it would end.
    fn request_close(&mut self, closing: Closing, ctx: &egui::Context) {
        let unsaved = self.consoles.unsaved_files(&closing);
        if unsaved.is_empty() {
            self.close_connections(closing, ctx);
        } else if self.unsaved_confirmation.is_none() && self.closing_after_saves.is_none() {
            self.unsaved_confirmation = Some(UnsavedConfirmation {
                closing,
                consoles: unsaved,
            });
        }
    }

    fn show_unsaved_confirmation(&mut self, ctx: &egui::Context) {
        let Some(choice) = self
            .unsaved_confirmation
            .as_ref()
            .and_then(|confirmation| confirmation.show(ctx))
        else {
            return;
        };
        let Some(confirmation) = self.unsaved_confirmation.take() else {
            return;
        };
        match choice {
            UnsavedChoice::Save => {
                let mut waiting = Vec::new();
                for (key, _) in confirmation.consoles {
                    if let Some(save) = self.consoles.save_file(&key) {
                        self.send(save);
                        waiting.push(key);
                    }
                }
                self.closing_after_saves = Some((confirmation.closing, waiting));
                self.close_once_saved_all(ctx);
            }
            UnsavedChoice::Discard => self.close_connections(confirmation.closing, ctx),
            UnsavedChoice::Cancel if matches!(confirmation.closing, Closing::Quit) => {
                self.updates.cancel_restart();
            }
            UnsavedChoice::Cancel => {}
        }
    }

    /// Takes in a save that a close may be waiting for: a failure cancels the close, so the
    /// text stays open with its error showing.
    fn close_once_saved(&mut self, console: &ConsoleKey, saved: bool) {
        let Some((closing, waiting)) = &mut self.closing_after_saves else {
            return;
        };
        if !waiting.contains(console) {
            return;
        }
        if !saved {
            if matches!(closing, Closing::Quit) {
                self.updates.cancel_restart();
            }
            self.closing_after_saves = None;
            return;
        }
        waiting.retain(|key| key != console);
    }

    fn close_once_saved_all(&mut self, ctx: &egui::Context) {
        if self
            .closing_after_saves
            .as_ref()
            .is_some_and(|(_, waiting)| waiting.is_empty())
            && let Some((closing, _)) = self.closing_after_saves.take()
        {
            self.close_connections(closing, ctx);
        }
    }

    /// Does what `closing` asks, first asking whether to roll back the open transactions it
    /// would end, if there are any.
    fn close_connections(&mut self, closing: Closing, ctx: &egui::Context) {
        let rolled_back = self.consoles.rolled_back_by(&closing);
        if rolled_back.is_empty() {
            self.finish_closing(closing, ctx);
        } else if self.rollback_confirmation.is_none() {
            self.rollback_confirmation = Some(RollbackConfirmation::new(closing, rolled_back));
        }
    }

    fn finish_closing(&mut self, closing: Closing, ctx: &egui::Context) {
        match closing {
            Closing::Console(key) => {
                let commands = self.consoles.close(&key);
                self.send_all(commands);
                workspace::close_console_tab(&mut self.dock, &key);
            }
            Closing::Disconnect(id) => self.send(Command::Disconnect(id)),
            Closing::Quit => {
                self.quit_confirmed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Holds the window open while quitting would roll back transactions the user hasn't
    /// agreed to lose.
    fn confirm_quit(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) || self.quit_confirmed {
            return;
        }
        if !self.consoles.rolled_back_by(&Closing::Quit).is_empty()
            || !self.consoles.unsaved_files(&Closing::Quit).is_empty()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_close(Closing::Quit, ctx);
        }
    }

    fn show_rollback_confirmation(&mut self, ctx: &egui::Context) {
        let Some(choice) = self
            .rollback_confirmation
            .as_ref()
            .and_then(|confirmation| confirmation.show(ctx))
        else {
            return;
        };
        let Some(confirmation) = self.rollback_confirmation.take() else {
            return;
        };
        match choice {
            RollbackChoice::Proceed => self.finish_closing(confirmation.closing, ctx),
            RollbackChoice::Cancel if matches!(confirmation.closing, Closing::Quit) => {
                self.updates.cancel_restart();
            }
            RollbackChoice::Cancel => {}
        }
    }

    /// Runs a custom action on `subject`: a command action's program, or its SQL, filled in
    /// and opened in a new console, run unless the action says to edit it first. What the
    /// action can't fill is said in the status bar.
    fn run_action(&mut self, id: &ActionId, data_source: DataSourceId, subject: &Subject) {
        let Some(action) = self
            .custom_files
            .actions()
            .iter()
            .find(|action| action.id == *id)
        else {
            return;
        };
        let Some(entry) = self.state.data_source(data_source) else {
            return;
        };
        let quoting = QueryLanguage::from(entry.data_source.config.driver()).quoting();
        if let ActionBody::Command(command) = &action.body {
            self.action_problem = None;
            let values = custom_actions::command_values(&entry.data_source, subject, quoting);
            let run = self
                .command_runs
                .start(action, command, data_source, values);
            self.send_if(run);
            return;
        }
        match custom_actions::fill(action, &entry.data_source, subject, quoting) {
            Ok(text) => {
                self.action_problem = None;
                let run = matches!(action.body, ActionBody::Sql { run: true, .. })
                    .then(|| RunOrigin::Action(action.id.clone()));
                let (key, command) = self.consoles.open_action(&entry.data_source, text, run);
                workspace::open_console_tab(&mut self.dock, key);
                self.send_if(command);
            }
            Err(problem) => {
                tracing::warn!("{problem}");
                self.action_problem = Some(problem);
            }
        }
    }

    /// Opens what a command action's program wrote, SQL, in a new console on the data source
    /// it ran on, without running it.
    fn open_program_output(&mut self, data_source: DataSourceId, text: String) {
        let Some(entry) = self.state.data_source(data_source) else {
            return;
        };
        let (key, command) = self.consoles.open_action(&entry.data_source, text, None);
        workspace::open_console_tab(&mut self.dock, key);
        self.send_if(command);
    }

    /// ⌘. stops running programs, unless the focused console has a run of its own to stop,
    /// which the console then handles.
    fn program_shortcuts(&mut self, ctx: &egui::Context) {
        if !self.command_runs.is_running() {
            return;
        }
        let console_busy = self.focused_console().is_some_and(|key| {
            self.consoles
                .get_mut(&key)
                .is_some_and(|console| console.cancellable_run().is_some())
        });
        if console_busy {
            return;
        }
        let stop = ctx.input_mut(|input| {
            !self
                .keymap
                .consume(input, &[keymap::Action::Cancel])
                .is_empty()
        });
        if stop {
            self.send_all(self.command_runs.stop_all());
        }
    }

    /// Opens the rows a foreign key leads to from `console`'s grid, in a new console on the
    /// same data source.
    fn open_rows(&mut self, console: &ConsoleKey, rows: &RowsToOpen) {
        let opened = self
            .state
            .data_source(console.data_source)
            .and_then(|entry| self.consoles.open_rows(&entry.data_source, rows));
        if let Some((key, command)) = opened {
            workspace::open_console_tab(&mut self.dock, key);
            self.send(command);
        }
    }

    fn open_history(&mut self, console: ConsoleKey) {
        let Some(entry) = self.state.data_source(console.data_source) else {
            return;
        };
        let names = self
            .state
            .data_sources
            .entries()
            .iter()
            .map(|entry| (entry.data_source.id, entry.data_source.name.clone()))
            .collect();
        let (window, search) = HistoryWindow::open(console, entry.data_source.name.clone(), names);
        self.history_window = Some(window);
        self.send(search);
    }

    fn show_history_window(&mut self, ctx: &egui::Context) {
        let Some(action) = self
            .history_window
            .as_mut()
            .and_then(|window| window.show(ctx))
        else {
            return;
        };
        match action {
            HistoryAction::Search(query) => self.send(Command::SearchHistory(query)),
            HistoryAction::Insert { console, sql } => {
                self.consoles.insert_statement(&console, &sql);
            }
            HistoryAction::Export(spec) => {
                if let Some(path) = (self.pick_export_path)(spec.format(), "history")
                    && let Some(window) = &mut self.history_window
                {
                    let command = window.export(path, spec);
                    self.send(command);
                }
            }
            HistoryAction::Close => self.history_window = None,
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let state = self.command_state(ui.ctx());
        let recent = self.sql_files.recent().iter().map(|file| file.path.clone());
        let menus = commands::egui_menu_bar(recent, self.custom_files.actions());
        if let Some(command) = menu_bar::show(ui, &menus, &state, &self.keymap) {
            self.run_command(command, ui.ctx());
        }
    }

    /// The title bar strip, the egui menu bar, both, or neither (macOS with a native menu bar
    /// and a title bar of its own, which never happens).
    fn top_strip(&mut self, ui: &mut egui::Ui, bar: Frame) {
        let draws_menu = self.native_menu.is_none();
        if self.integrated_title_bar {
            egui::Panel::top("title_bar")
                .frame(bar.inner_margin(Margin::ZERO))
                .exact_size(title_bar::HEIGHT)
                .show(ui, |ui| {
                    title_bar::show(ui, "DbRover", |ui| {
                        if draws_menu {
                            self.menu_bar(ui);
                        }
                    });
                });
        } else if draws_menu {
            egui::Panel::top("menu_bar")
                .frame(bar)
                .show(ui, |ui| self.menu_bar(ui));
        }
    }

    fn sync_native_menu(&mut self, ctx: &egui::Context) {
        if self.native_menu.is_none() {
            return;
        }
        let state = self.command_state(ctx);
        let recent: Vec<_> = self
            .sql_files
            .recent()
            .iter()
            .map(|file| file.path.clone())
            .collect();
        if let Some(menu) = &mut self.native_menu {
            menu.sync(&state, &self.keymap, &recent, self.custom_files.actions());
        }
    }

    fn command_state(&mut self, ctx: &egui::Context) -> CommandState {
        CommandState {
            console_focused: self.focused_console().is_some(),
            has_data_sources: !self.state.data_sources.entries().is_empty(),
            theme: ctx.options(|options| options.theme_preference),
            subject: self.action_subject_kind(),
        }
    }

    /// Does what a menu item, or its shortcut, asks for.
    fn run_command(&mut self, command: AppCommand, ctx: &egui::Context) {
        tracing::debug!(command = %command.id(), "running a menu command");
        match command {
            AppCommand::OpenFile => self.choose_sql_file(),
            AppCommand::OpenRecent(path) => self.open_sql_file(path),
            AppCommand::ClearRecentFiles => self.sql_files.clear_recent(),
            AppCommand::Save => {
                if let Some(console) = self.focused_console() {
                    self.save_console(&console);
                }
            }
            AppCommand::SaveAs => {
                if let Some(console) = self.focused_console() {
                    self.save_console_as(&console);
                }
            }
            AppCommand::ExportDataSources => {
                self.export_sources_dialog = Some(ExportSourcesDialog::new());
            }
            AppCommand::ImportDataSources => self.import_data_sources(),
            AppCommand::Theme(preference) => ctx.set_theme(preference),
            AppCommand::ShowKeymap => self.open_settings_page(Page::Keymap),
            AppCommand::ShowActions => self.open_settings_page(Page::Actions),
            AppCommand::ShowSettings => workspace::open_settings_tab(&mut self.dock),
            AppCommand::ResetLayout => {
                self.dock = workspace::default_layout();
                tracing::info!("layout reset");
            }
            AppCommand::Format => self.format_focused_console(FormatScope::Current),
            AppCommand::RunToFile => {
                if let Some(console) = self.focused_console() {
                    self.open_run_to_file(console);
                }
            }
            AppCommand::Copy(copy) => {
                if let Some(console) = self.focused_console()
                    && let Some(text) =
                        self.consoles
                            .copy(&console, copy, &self.export_choices.options)
                {
                    ctx.copy_text(text);
                }
            }
            AppCommand::FormatAll => self.format_focused_console(FormatScope::All),
            AppCommand::CheckForUpdates => {
                let check = self.updates.check_now();
                self.send(check);
            }
            AppCommand::About => self.about_open = true,
            AppCommand::OpenManual => ctx.open_url(egui::OpenUrl::new_tab(welcome::MANUAL_URL)),
            AppCommand::ReportProblem => {
                ctx.open_url(egui::OpenUrl::new_tab(feedback::bug_report_url()));
            }
            AppCommand::AskQuestion => {
                ctx.open_url(egui::OpenUrl::new_tab(feedback::new_question_url()));
            }
            AppCommand::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            AppCommand::RunAction(action) => self.run_action_on_selection(&action.id),
        }
    }

    fn import_data_sources(&mut self) {
        if let Some(path) = (self.pick_data_sources_import)() {
            let (dialog, read) = ImportDialog::open(path);
            self.import_dialog = Some(dialog);
            self.send(read);
        }
    }

    fn format_focused_console(&mut self, scope: FormatScope) {
        if let Some(console) = self.focused_console() {
            self.consoles
                .format(&console, scope, &self.settings.format_options());
        }
    }

    fn open_export_dialog(&mut self, console: ConsoleKey) {
        if let Some(preview) = self.consoles.export_preview(&console) {
            let subject = ExportSubject::Grid(console);
            self.export_dialog = Some(ExportDialog::new(subject, preview, &self.export_choices));
        }
    }

    /// Run to file: the export dialog for the statement at the console's cursor.
    fn open_run_to_file(&mut self, console: ConsoleKey) {
        let Some(run) = self.consoles.statement_to_file(&console) else {
            return;
        };
        let Some(language) = self.consoles.language(&console) else {
            return;
        };
        let Some(shape) = run
            .statements
            .first()
            .and_then(|statement| language.result_shape(&statement.sql))
        else {
            return;
        };
        let preview = ExportPreview {
            columns: Vec::new(),
            sample: Vec::new(),
            document_sample: Vec::new(),
            loaded: 0,
            source: dbrover_export::RowSource {
                dialect: language.dialect(),
                table: None,
                shape,
            },
        };
        let subject = ExportSubject::Statement { console, run };
        self.export_dialog = Some(ExportDialog::new(subject, preview, &self.export_choices));
    }

    /// Export data… on a table or view: the export dialog, while the table is described.
    fn open_table_export(&mut self, id: DataSourceId, path: &CatalogPath) {
        let opened = self
            .state
            .data_source(id)
            .and_then(|entry| TableTarget::open(&entry.data_source, path));
        if let Some((target, describe)) = opened {
            let preview = target.preview();
            let subject = ExportSubject::Table(target);
            self.export_dialog = Some(ExportDialog::new(subject, preview, &self.export_choices));
            self.send(describe);
        }
    }

    /// Opens **Diagram…** on a schema, loading its tables folder if the explorer hasn't.
    fn open_diagram_dialog(&mut self, id: DataSourceId, schema: &CatalogPath) {
        let Some(dialog) = DiagramDialog::new(id, schema) else {
            return;
        };
        let needs_loading = self
            .state
            .catalog_mut(id)
            .is_some_and(|catalog| catalog.request(dialog.tables_path()));
        self.load_children_if(needs_loading, id, dialog.tables_path().clone());
        self.diagram_dialog = Some(dialog);
    }

    fn show_diagram_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.diagram_dialog else {
            return;
        };
        let catalog = self
            .state
            .data_source(dialog.data_source)
            .map(|entry| &entry.catalog);
        let Some(action) = dialog.show(ctx, catalog) else {
            return;
        };
        let Some(dialog) = self.diagram_dialog.take() else {
            return;
        };
        if let DiagramDialogAction::Open {
            tables,
            growth,
            name,
        } = action
        {
            let schema = dialog.schema().to_owned();
            let tables = tables
                .into_iter()
                .map(|name| TableName {
                    schema: schema.clone(),
                    name,
                })
                .collect();
            let name = DiagramName::from_text(&name);
            self.open_diagram(dialog.data_source, schema, tables, growth, &name);
        }
    }

    /// Opens a diagram of `tables` in a tab of its own, named `name` unless another open
    /// diagram of its folder is, and starts describing them.
    fn open_diagram(
        &mut self,
        id: DataSourceId,
        schema: String,
        tables: Vec<TableName>,
        growth: Option<Growth>,
        name: &DiagramName,
    ) {
        let Some(entry) = self.state.data_source(id) else {
            return;
        };
        let project = entry
            .data_source
            .project
            .and_then(|id| self.state.projects.get(id));
        let folder = DiagramFolder::of(project.map(|project| project.slug.clone()));
        let name = self.diagrams.unused_name(&folder, name);
        let key = DiagramKey { folder, name };
        let diagram = Diagram::new(id, Some(schema), tables, growth);
        let first = self.diagrams.open(key.clone(), diagram);
        workspace::open_diagram_tab(&mut self.dock, key);
        self.send_if(first);
    }

    /// The export dialog; once it has a format, asks where to save and starts the export.
    fn show_export_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.export_dialog else {
            return;
        };
        match dialog.show(ctx) {
            Some(ExportDialogAction::Export(spec)) => {
                self.export_choices = dialog.choices().clone();
                let subject = dialog.subject().clone();
                let rows = dialog.rows();
                self.export_to_file(subject, rows, spec);
            }
            Some(ExportDialogAction::Cancel(run)) => {
                if let ExportSubject::Table(target) = dialog.subject() {
                    let data_source = target.data_source;
                    self.send(Command::CancelTableExport { data_source, run });
                }
            }
            Some(ExportDialogAction::Close) => self.export_dialog = None,
            None => {}
        }
    }

    /// Asks where to save, then exports `subject` there: a console's rows and a statement in
    /// the console, which shows how it goes, and a table in the dialog, which stays open.
    fn export_to_file(&mut self, subject: ExportSubject, rows: ExportedRows, spec: ExportSpec) {
        let what = match &subject {
            ExportSubject::Table(target) => target.table.name.clone(),
            ExportSubject::Grid(_) | ExportSubject::Statement { .. } => "rows".to_owned(),
        };
        let Some(path) = (self.pick_export_path)(spec.format(), &what) else {
            return;
        };
        let command = match subject {
            ExportSubject::Grid(console) => {
                self.export_dialog = None;
                self.consoles.export(&console, rows, spec, path)
            }
            ExportSubject::Statement { console, mut run } => {
                self.export_dialog = None;
                run.destination = RunDestination::File { path, spec };
                self.consoles.run_to_file(&console, run)
            }
            ExportSubject::Table(target) => {
                let run = self.consoles.new_run_id();
                if let Some(dialog) = &mut self.export_dialog {
                    dialog.table_export_started(run, path.clone());
                }
                Some(Command::ExportTable {
                    data_source: target.data_source,
                    export: dbrover_engine::TableExport {
                        run,
                        table: target.table,
                        path,
                        spec,
                    },
                })
            }
        };
        self.send_if(command);
    }

    fn show_export_sources_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.export_sources_dialog else {
            return;
        };
        let data_sources: Vec<_> = self
            .state
            .data_sources
            .entries()
            .iter()
            .map(|entry| entry.data_source.clone())
            .collect();
        match dialog.show(ctx, self.state.projects.projects(), &data_sources) {
            Some(ExportAction::ChooseFile) => {
                if let Some(path) = (self.pick_data_sources_export)() {
                    let command = dialog.command(path);
                    self.send(command);
                }
            }
            Some(ExportAction::Close) => self.export_sources_dialog = None,
            None => {}
        }
    }

    /// Asks for a CSV file, then opens the dialog that imports it into the table at `path`.
    fn open_csv_import(&mut self, id: DataSourceId, path: &CatalogPath) {
        let Some(file) = (self.pick_csv_file)() else {
            return;
        };
        let opened = self
            .state
            .data_source(id)
            .and_then(|entry| CsvImportDialog::open(&entry.data_source, path, file));
        if let Some((dialog, describe)) = opened {
            self.csv_import = Some(dialog);
            self.send(describe);
        }
    }

    fn show_csv_import(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.csv_import else {
            return;
        };
        match dialog.show(ctx) {
            Some(CsvImportAction::ChooseFile) => {
                if let Some(file) = (self.pick_csv_file)() {
                    dialog.set_file(file);
                }
            }
            Some(CsvImportAction::Import(data_source, import)) => {
                self.send(Command::ImportRows {
                    data_source,
                    import,
                });
            }
            Some(CsvImportAction::Close) => self.csv_import = None,
            None => {}
        }
    }

    fn show_import_dialog(&mut self, ctx: &egui::Context) {
        match self
            .import_dialog
            .as_mut()
            .and_then(|dialog| dialog.show(ctx))
        {
            Some(ImportAction::Apply(plan)) => self.send(Command::ApplyImport(plan)),
            Some(ImportAction::Close) => self.import_dialog = None,
            None => {}
        }
    }

    fn show_about(&mut self, ctx: &egui::Context) {
        if self.about_open && about_dialog::show(ctx) {
            self.about_open = false;
        }
    }

    fn show_adoption_notice(&mut self, ctx: &egui::Context) {
        if self
            .adoption_notice
            .as_ref()
            .is_some_and(|notice| notice.show(ctx))
        {
            self.adoption_notice = None;
        }
    }

    fn show_update_dialog(&mut self, ctx: &egui::Context) {
        if !self.updates.dialog_open {
            return;
        }
        match update_dialog::show(ctx, &self.updates) {
            Some(UpdateAction::Close) => self.updates.dialog_open = false,
            Some(UpdateAction::Install) => {
                let install = self.updates.install();
                self.send_if(install);
            }
            // Quits the usual way, so open transactions are still confirmed first; `main`
            // starts the installed copy, or the Windows installer, once this one has stopped.
            Some(UpdateAction::Restart) if self.updates.request_restart() => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(UpdateAction::Restart) | None => {}
        }
    }

    /// The console in the dock's focused tab, if that is a console.
    fn focused_console(&mut self) -> Option<ConsoleKey> {
        match self.dock.find_active_focused() {
            Some((_, Tab::QueryConsole(key))) => Some(key.clone()),
            _ => None,
        }
    }

    fn handle_settings_action(&mut self, action: SettingsAction, ctx: &egui::Context) {
        match action {
            SettingsAction::SaveMcpDefaults(defaults) => {
                self.send(Command::SaveMcpDefaults(defaults));
            }
            SettingsAction::Save(settings) => {
                self.settings.set(settings.clone());
                self.send(Command::SaveSettings(settings));
            }
            SettingsAction::Reload => self.send(Command::ReloadSettings),
            SettingsAction::Theme(preference) => {
                self.run_command(AppCommand::Theme(preference), ctx)
            }
            SettingsAction::ReloadKeymap => self.send(Command::ReloadKeymap),
            SettingsAction::CreateKeymap => self.send(Command::CreateKeymap {
                text: keymap::template(),
            }),
            SettingsAction::ReloadCustomFiles => self.send(Command::ReloadCustomFiles),
            SettingsAction::CreateActions => self.send(Command::CreateCustomFile {
                kind: CustomFileKind::Actions,
                text: custom_actions::TEMPLATE.to_owned(),
            }),
        }
    }

    /// Opens the Settings tab, or brings it to the front, on `page`.
    fn open_settings_page(&mut self, page: Page) {
        self.settings_tab.show_page(page);
        workspace::open_settings_tab(&mut self.dock);
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let palette = &Palette::for_visuals(ui.visuals());
        let dot = match self.state.engine_status {
            EngineStatus::Starting => palette.warning,
            EngineStatus::Running => palette.success,
            EngineStatus::Stopped => palette.error,
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("●").small().color(dot));
            ui.label(
                RichText::new(format!(
                    "Engine {} · {} connected",
                    self.state.engine_status.label(),
                    self.state.connected_count()
                ))
                .small()
                .color(palette.text_weak),
            );
            self.problem_links(ui, palette.warning);
            let stops = self.command_runs.status(ui, palette);
            self.send_all(stops);
            if let Some(problem) = &self.action_problem
                && ui
                    .link(RichText::new(problem).small().color(palette.error))
                    .on_hover_text("Dismiss")
                    .clicked()
            {
                self.action_problem = None;
            }
            if let Some(notice) = self.updates.notice()
                && ui
                    .link(RichText::new(notice).small().color(palette.primary))
                    .clicked()
            {
                self.updates.dialog_open = true;
            }
        });
    }

    /// A link per config file with problems, opening the window that lists them.
    fn problem_links(&mut self, ui: &mut egui::Ui, color: egui::Color32) {
        let keymap = self.keymap.problems().len();
        if keymap > 0 && widgets::problems_link(ui, "keymap.toml", keymap, color).clicked() {
            self.open_settings_page(Page::Keymap);
        }
        for kind in CustomFileKind::ALL {
            let count = self.custom_files.problems(kind).len();
            if count > 0 && widgets::problems_link(ui, kind.file_name(), count, color).clicked() {
                match kind {
                    CustomFileKind::Actions => self.open_settings_page(Page::Actions),
                    CustomFileKind::Snippets | CustomFileKind::Theme => {
                        self.custom_files.window = Some(kind);
                    }
                }
            }
        }
    }

    fn show_custom_file_window(&mut self, ctx: &egui::Context) {
        match custom_files::show(ctx, &self.custom_files) {
            Some(FileWindowAction::Reload) => self.send(Command::ReloadCustomFiles),
            Some(FileWindowAction::Close) => self.custom_files.window = None,
            None => {}
        }
    }
}

impl eframe::App for DbRoverApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let chosen = self
            .native_menu
            .as_mut()
            .map(NativeMenu::drain)
            .unwrap_or_default();
        for chosen in chosen {
            match chosen {
                Chosen::Command(command) => self.run_command(command, ctx),
                Chosen::Input(events) => raw_input.events.extend(events),
            }
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.sync_native_menu(ctx);
        if let Some(look) = &mut self.system_look {
            look.follow(ctx);
        }
        self.maximize_window_once(ctx);
        self.drain_engine_events();
        self.filter_paged_levels(ctx);
        if std::mem::take(&mut self.theme_changed) {
            theme::install_palettes(ctx, self.custom_files.palettes());
        }
        self.log_console.drain();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.file_shortcuts(&ctx);
        self.custom_action_shortcuts(&ctx);
        self.program_shortcuts(&ctx);
        self.open_dropped_files(&ctx);
        // The base colour shows in the gaps between the dock's islands.
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);
        let bar = Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(Margin::symmetric(10, 2));
        self.top_strip(ui, bar);
        egui::Panel::bottom("status_bar")
            .frame(bar)
            .show(ui, |ui| self.status_bar(ui));

        let mut explorer_actions = Vec::new();
        let mut console_actions = Vec::new();
        let mut close_requests = Vec::new();
        let mut settings_actions = Vec::new();
        let mut diagram_actions = Vec::new();
        let palette = &Palette::for_visuals(ui.visuals());
        let lint = self.settings.lint_options();
        let (open_consoles, saved_consoles, recent_completions) =
            self.consoles.open_and_saved_mut();
        DockArea::new(&mut self.dock)
            .style(theme::dock_style(ui.style()))
            // Calmer tab bars: tabs close one at a time, and panels don't fold away.
            .show_leaf_collapse_buttons(false)
            .show_leaf_close_all_buttons(false)
            .show_inside(
                ui,
                &mut Panels {
                    palette,
                    log_console: &mut self.log_console,
                    data_sources: &self.state.data_sources,
                    projects: self.state.projects.projects(),
                    saved_consoles,
                    explorer_options: &mut self.explorer_options,
                    custom_actions: self.custom_files.actions(),
                    snippets: self.custom_files.snippets(),
                    explorer_actions: &mut explorer_actions,
                    consoles: open_consoles,
                    keymap: &self.keymap,
                    keymap_path: self.keymap_path.as_deref(),
                    custom_files: &self.custom_files,
                    command_runs: &self.command_runs,
                    lint: &lint,
                    recent_completions,
                    console_actions: &mut console_actions,
                    close_requests: &mut close_requests,
                    settings_tab: &mut self.settings_tab,
                    settings: &self.settings,
                    mcp_defaults: &mut self.mcp_defaults,
                    settings_actions: &mut settings_actions,
                    diagrams: &mut self.diagrams,
                    diagram_actions: &mut diagram_actions,
                },
            );
        if let Some(text) = self.clipboard.take() {
            ctx.copy_text(text);
        }
        for action in explorer_actions {
            self.handle_explorer_action(action, &ctx);
        }
        for action in settings_actions {
            self.handle_settings_action(action, &ctx);
        }
        for action in diagram_actions {
            self.handle_diagram_action(action);
        }
        for (console, action) in console_actions {
            match action {
                ConsoleAction::ShowHistory => self.open_history(console),
                ConsoleAction::OpenExportDialog => self.open_export_dialog(console),
                ConsoleAction::RunToFile => self.open_run_to_file(console),
                ConsoleAction::Copy(copy) => {
                    let options = &self.export_choices.options;
                    if let Some(text) = self.consoles.copy(&console, copy, options) {
                        ctx.copy_text(text);
                    }
                }
                ConsoleAction::OpenRows(rows) => self.open_rows(&console, &rows),
                ConsoleAction::ExportChart(format) => self.export_chart(&console, format),
                ConsoleAction::RunAction(action, cells) => {
                    self.run_action(&action, console.data_source, &Subject::Cells(cells));
                }
                ConsoleAction::LoadCatalog(paths) => self.load_catalog(console.data_source, paths),
                ConsoleAction::Format(scope) => {
                    self.consoles
                        .format(&console, scope, &self.settings.format_options());
                }
                action => {
                    let command = self.consoles.handle(&console, action);
                    self.send_if(command);
                }
            }
        }
        for console in close_requests {
            self.request_close(Closing::Console(console), &ctx);
        }
        self.show_unsaved_confirmation(&ctx);
        self.close_once_saved_all(&ctx);
        self.confirm_quit(&ctx);
        self.show_rollback_confirmation(&ctx);

        self.save_consoles(&ctx);
        self.save_diagrams(&ctx);
        self.check_sql_files(&ctx);
        self.show_open_file_dialog(&ctx);
        let given = self.consoles.show_parameters_dialog(&ctx);
        self.send_if(given);
        let confirmed = self.consoles.show_hazard_confirmation(&ctx);
        self.send_if(confirmed);
        self.show_data_source_dialog(&ctx);
        self.show_delete_confirmation(&ctx);
        self.show_project_dialog(&ctx);
        self.show_project_settings(&ctx);
        self.show_delete_project_confirmation(&ctx);
        self.show_object_dialog(&ctx);
        self.show_password_prompt(&ctx);
        self.show_host_key_prompt(&ctx);
        self.show_password_command_prompt(&ctx);
        self.show_export_sources_dialog(&ctx);
        self.show_export_dialog(&ctx);
        self.show_import_dialog(&ctx);
        self.show_csv_import(&ctx);
        self.show_diagram_dialog(&ctx);
        if self
            .definition_window
            .as_ref()
            .is_some_and(|window| !window.show(&ctx))
        {
            self.definition_window = None;
        }
        self.show_history_window(&ctx);
        self.show_custom_file_window(&ctx);
        let commands = self.command_runs.show(&ctx);
        self.send_all(commands);
        self.show_about(&ctx);
        self.show_adoption_notice(&ctx);
        self.show_update_dialog(&ctx);

        // Entries logged on the UI thread don't wake the UI (see `UiWaker`), so collect any
        // logged during this frame and ask for one more frame to show them.
        if self.log_console.drain() {
            ctx.request_repaint();
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, LAYOUT_STORAGE_KEY, &self.dock);
        eframe::set_value(
            storage,
            sql_files::STORAGE_KEY,
            &self.sql_files.remembered(self.consoles.files()),
        );
        eframe::set_value(storage, export_dialog::STORAGE_KEY, &self.export_choices);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Sent before the engine stops: it handles every queued command before shutting down.
        let unsaved = self.consoles.unsaved();
        self.send_all(unsaved);
        let unsaved = self.diagrams.unsaved();
        self.send_all(unsaved);
        self.shut_down_engine();
        if let Some(restart) = self.updates.restart_after_quit() {
            relaunch::request(restart.clone());
        }
    }
}

/// Lets the engine wake egui without depending on it.
struct EguiRepaint(egui::Context);

impl Repaint for EguiRepaint {
    fn request_repaint(&self) {
        self.0.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    mod actions;
    mod charts;
    mod commands;
    mod customisation;
    mod diagrams;
    mod files;
    mod find;
    mod grid;
    mod history;
    mod menus;
    mod modify_table;
    mod parameters;
    mod password_commands;
    mod plans;
    mod settings;
    mod title_bar;

    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use dbrover_core::{DataSourceId, Password};
    use dbrover_store::{
        ConsoleFiles, ConsoleName, CustomFiles, DataSourceFile, HistoryFile, KeymapFile,
        ProjectFile, Secret, SecretError, SecretStore, SettingsFile, TrustedCommandsFile,
    };
    use egui::accesskit::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::log_console;
    use crate::test_support::{FixedReleases, step_until, step_until_visible};

    const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

    /// The tests never need a password, and must not touch the real keychain.
    struct NoSecrets;

    impl SecretStore for NoSecrets {
        fn get(&self, _: DataSourceId, _: Secret) -> Result<Option<Password>, SecretError> {
            Ok(None)
        }

        fn set(&self, _: DataSourceId, _: Secret, _: &Password) -> Result<(), SecretError> {
            Ok(())
        }

        fn delete(&self, _: DataSourceId, _: Secret) -> Result<(), SecretError> {
            Ok(())
        }
    }

    fn engine_config(dir: &Path) -> EngineConfig {
        engine_config_with_releases(dir, FixedReleases::current())
    }

    fn engine_config_with_releases(dir: &Path, releases: FixedReleases) -> EngineConfig {
        EngineConfig {
            data_sources: DataSourceFile::new(dir.join("data_sources.toml")),
            projects: ProjectFile::new(dir.join("projects.toml")),
            secrets: Arc::new(NoSecrets),
            consoles: ConsoleFiles::new(dir.join("consoles")),
            diagrams: dbrover_store::DiagramFiles::new(dir.join("diagrams")),
            history: HistoryFile::new(dir.join("history.sqlite")),
            keymap: KeymapFile::new(dir.join("keymap.toml")),
            settings: SettingsFile::new(dir.join("settings.toml")),
            custom_files: CustomFiles::new(dir),
            trusted_commands: TrustedCommandsFile::new(dir.join("trusted_commands.toml")),
            programs: Arc::new(crate::test_support::ScriptedPrograms),
            releases: Arc::new(releases),
            updater: Arc::new(crate::test_support::FakeInstaller::default()),
            known_hosts: dir.join("known_hosts"),
            idle_ping_after: dbrover_engine::IDLE_PING_AFTER,
        }
    }

    /// The app's own default window size, so tests see the layout a new user sees. At
    /// egui_kittest's 800x600 the console is too narrow for the results summary row.
    fn harness_at_default_size() -> egui_kittest::HarnessBuilder<DbRoverApp> {
        Harness::builder().with_size(crate::DEFAULT_WINDOW_SIZE)
    }

    fn start_app(dir: &Path, log_feed: LogFeed) -> Harness<'static, DbRoverApp> {
        let config = engine_config(dir);
        harness_at_default_size().build_eframe(|cc| DbRoverApp::new(cc, log_feed, config).unwrap())
    }

    /// Saves one SQLite data source named `shop`, over a database with `schema`.
    fn save_shop(dir: &Path, schema: &str) -> dbrover_core::DataSource {
        let database = dir.join("shop.db");
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch(schema)
            .unwrap();
        let data_source = dbrover_core::DataSource {
            id: DataSourceId::generate(),
            name: "shop".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: dbrover_core::ConnectionConfig::Sqlite(dbrover_core::SqliteConfig {
                path: database,
                read_only: false,
            }),
        };
        DataSourceFile::new(dir.join("data_sources.toml"))
            .save(std::slice::from_ref(&data_source))
            .unwrap();
        data_source
    }

    #[test]
    fn shows_every_panel_the_engine_status_and_log_entries() {
        let dir = tempfile::tempdir().unwrap();
        let (layer, feed) = log_console::pipeline();
        // Thread-local, so it only captures events logged on this (the UI) thread.
        let _subscriber =
            tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));
        tracing::info!("logged before the window opened");

        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "Engine running");
        step_until_visible(&mut harness, "No data sources yet.");

        // egui_dock doesn't expose tab titles to accessibility, so check each panel's content.
        harness.get_by_label_contains("Welcome to DbRover");
        harness.get_by_label_contains("logged before the window opened");
    }

    #[test]
    fn adds_a_sqlite_data_source_through_the_dialog() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("chinook.db");
        rusqlite::Connection::open(&database).unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "No data sources yet.");

        // The explorer's toolbar and the Welcome tab both offer it; either opens the menu.
        harness
            .get_all_by_label("New data source")
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_by_label("SQLite").click();
        harness.run();
        // The native open and save dialogs can't be driven headlessly; check they're offered.
        harness.get_by_label("Browse…");
        harness.get_by_label("New…");
        let file_field = harness.get_by_role_and_label(Role::TextInput, "File");
        file_field.focus();
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, "File")
            .type_text(&database.display().to_string());
        harness.run();
        harness.get_by_label("Save").click();

        step_until_visible(&mut harness, "Expand chinook.db");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].name, "chinook.db");
    }

    #[test]
    fn adds_a_mysql_data_source_with_the_servers_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "No data sources yet.");

        harness
            .get_all_by_label("New data source")
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_by_label("MySQL / MariaDB").click();
        harness.run();
        harness.get_by_label_contains("New MySQL / MariaDB data source");
        harness.get_by_label("Save").click();

        step_until_visible(&mut harness, "Expand mysql@localhost");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        let [saved] = saved.as_slice() else {
            panic!("expected one data source, got {saved:?}");
        };
        let dbrover_core::ConnectionConfig::Mysql(config) = &saved.config else {
            panic!("expected a MySQL data source, got {saved:?}");
        };
        let server = &config.server;
        assert_eq!(
            (
                server.host.as_str(),
                server.port.clone(),
                server.database.as_str(),
                server.user.as_str()
            ),
            ("localhost", 3306.into(), "mysql", "root")
        );
    }

    #[test]
    fn adds_a_sql_server_data_source_with_the_servers_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "No data sources yet.");

        harness
            .get_all_by_label("New data source")
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_by_label("SQL Server").click();
        harness.run();
        harness.get_by_label_contains("New SQL Server data source");
        harness.get_by_label("Save").click();

        step_until_visible(&mut harness, "Expand master@localhost");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        let [saved] = saved.as_slice() else {
            panic!("expected one data source, got {saved:?}");
        };
        let dbrover_core::ConnectionConfig::SqlServer(config) = &saved.config else {
            panic!("expected a SQL Server data source, got {saved:?}");
        };
        let server = &config.server;
        assert_eq!(
            (
                server.host.as_str(),
                server.port.clone(),
                server.database.as_str(),
                server.user.as_str()
            ),
            ("localhost", 1433.into(), "master", "sa")
        );
    }

    #[test]
    fn adds_an_in_memory_duckdb_data_source() {
        let dir = tempfile::tempdir().unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "No data sources yet.");

        harness
            .get_all_by_label("New data source")
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_by_label("DuckDB").click();
        harness.run();
        harness.get_by_label_contains("New DuckDB data source");
        harness.get_by_label("In memory").click();
        harness.run();
        harness.get_by_label("Save").click();

        step_until_visible(&mut harness, "Expand memory");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        let [saved] = saved.as_slice() else {
            panic!("expected one data source, got {saved:?}");
        };
        assert_eq!(
            saved.config,
            dbrover_core::ConnectionConfig::DuckDb(dbrover_core::DuckDbConfig {
                database: dbrover_core::DuckDbDatabase::InMemory,
            })
        );
    }

    /// A Redis data source starts on database 0 as the `default` user, and offers no `prefer`,
    /// since Redis serves TLS on a port of its own.
    #[test]
    fn adds_a_redis_data_source_on_database_0() {
        let dir = tempfile::tempdir().unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "No data sources yet.");

        harness
            .get_all_by_label("New data source")
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_by_label("Redis / Valkey").click();
        harness.run();
        harness.get_by_label_contains("New Redis / Valkey data source");
        harness.get_by_label("Database number");
        harness.get_by_label("TLS").click();
        harness.run();
        assert!(harness.query_by_label("prefer").is_none());
        harness.get_by_label("disable").click();
        harness.run();
        harness.get_by_label("Save").click();

        step_until_visible(&mut harness, "Expand db0@localhost");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        let [saved] = saved.as_slice() else {
            panic!("expected one data source, got {saved:?}");
        };
        let dbrover_core::ConnectionConfig::Redis(config) = &saved.config else {
            panic!("expected a Redis data source, got {saved:?}");
        };
        let server = &config.server;
        assert_eq!(
            (
                server.port.clone(),
                server.database.as_str(),
                server.user.as_str(),
                &server.tls
            ),
            (6379.into(), "0", "default", &dbrover_core::TlsMode::Disable)
        );
    }

    /// A MongoDB data source starts on the server form, in `test` with no user; pasting a
    /// connection string moves its password into the password field, out of the string.
    #[test]
    fn adds_a_mongodb_data_source_from_a_connection_string() {
        let dir = tempfile::tempdir().unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "No data sources yet.");

        harness
            .get_all_by_label("New data source")
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_by_label("MongoDB").click();
        harness.run();
        harness.get_by_label_contains("New MongoDB data source");
        harness.get_by_label("Auth database");
        harness.get_by_label("Connection string").click();
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, "Connection string")
            .focus();
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, "Connection string")
            .type_text("mongodb+srv://app:secret@cluster0.example.mongodb.net/shop");
        harness.run();
        harness.get_by_label("Save").click();

        step_until_visible(&mut harness, "Expand shop@cluster0.example.mongodb.net");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        let [saved] = saved.as_slice() else {
            panic!("expected one data source, got {saved:?}");
        };
        let dbrover_core::ConnectionConfig::Mongo(config) = &saved.config else {
            panic!("expected a MongoDB data source, got {saved:?}");
        };
        assert_eq!(
            (
                config.server.database.as_str(),
                config.server.user.as_str(),
                config.connection_string.as_deref(),
            ),
            (
                "shop",
                "app",
                Some("mongodb+srv://cluster0.example.mongodb.net/shop")
            )
        );
    }

    /// An Elasticsearch data source has no Database row; signing in with an API key hides the
    /// user and saves none, and the key goes where a password would.
    #[test]
    fn adds_an_elasticsearch_data_source_with_an_api_key() {
        let dir = tempfile::tempdir().unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "No data sources yet.");

        harness
            .get_all_by_label("New data source")
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_by_label("Elasticsearch / OpenSearch").click();
        harness.run();
        harness.get_by_label_contains("New Elasticsearch / OpenSearch data source");
        assert!(harness.query_by_label("Database").is_none());
        harness.get_by_label("User");
        harness.get_by_label("API key").click();
        harness.run();
        assert!(harness.query_by_label("User").is_none());
        harness
            .get_by_role_and_label(Role::PasswordInput, "API key")
            .focus();
        harness.run();
        harness
            .get_by_role_and_label(Role::PasswordInput, "API key")
            .type_text("aWQ6a2V5");
        harness.run();
        harness.get_by_label("Save").click();

        step_until_visible(&mut harness, "Expand localhost");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        let [saved] = saved.as_slice() else {
            panic!("expected one data source, got {saved:?}");
        };
        let dbrover_core::ConnectionConfig::Elasticsearch(config) = &saved.config else {
            panic!("expected an Elasticsearch data source, got {saved:?}");
        };
        assert_eq!(
            (
                config.server.user.as_str(),
                config.server.database.as_str(),
                config.server.port.to_string(),
                config.auth,
            ),
            (
                "",
                "",
                "9200".to_owned(),
                dbrover_core::ElasticsearchAuth::ApiKey
            )
        );
    }

    /// A Cassandra data source names its keyspace in the database's place, which may be left
    /// empty, and saves its local datacenter and statement timeout.
    #[test]
    fn adds_a_cassandra_data_source_with_its_datacenter_and_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "No data sources yet.");

        harness
            .get_all_by_label("New data source")
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_by_label("Cassandra / ScyllaDB").click();
        harness.run();
        harness.get_by_label_contains("New Cassandra / ScyllaDB data source");
        assert!(harness.query_by_label("Database").is_none());
        for (label, text) in [
            ("Keyspace", "shop"),
            ("Local datacenter", "dc1"),
            ("Statement timeout", "30"),
        ] {
            harness
                .get_by_role_and_label(Role::TextInput, label)
                .focus();
            harness.run();
            harness
                .get_by_role_and_label(Role::TextInput, label)
                .type_text(text);
            harness.run();
        }
        harness.get_by_label("Save").click();

        step_until_visible(&mut harness, "Expand shop@localhost");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        let [saved] = saved.as_slice() else {
            panic!("expected one data source, got {saved:?}");
        };
        let dbrover_core::ConnectionConfig::Cassandra(config) = &saved.config else {
            panic!("expected a Cassandra data source, got {saved:?}");
        };
        assert_eq!(
            (
                config.server.database.as_str(),
                config.server.port.to_string(),
                config.local_datacenter.as_str(),
                config.statement_timeout.map(std::num::NonZeroU32::get),
            ),
            ("shop", "9042".to_owned(), "dc1", Some(30))
        );
    }

    #[test]
    fn adds_a_clickhouse_data_source_whose_port_follows_its_tls_mode() {
        let dir = tempfile::tempdir().unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "No data sources yet.");

        harness
            .get_all_by_label("New data source")
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_by_label("ClickHouse").click();
        harness.run();
        harness.get_by_label_contains("New ClickHouse data source");
        harness.get_by_label("TLS").click();
        harness.run();
        assert!(harness.query_by_label("prefer").is_none());
        harness.get_by_label("require").click();
        harness.run();
        harness.get_by_label("Save").click();

        step_until_visible(&mut harness, "Expand default@localhost");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        let [saved] = saved.as_slice() else {
            panic!("expected one data source, got {saved:?}");
        };
        let dbrover_core::ConnectionConfig::ClickHouse(config) = &saved.config else {
            panic!("expected a ClickHouse data source, got {saved:?}");
        };
        let server = &config.server;
        assert_eq!(
            (
                server.port.clone(),
                server.database.as_str(),
                server.user.as_str(),
                &server.tls
            ),
            (
                8443.into(),
                "default",
                "default",
                &dbrover_core::TlsMode::Require
            )
        );
    }

    #[test]
    fn chooses_a_tls_mode_and_ca_file_for_a_postgres_data_source() {
        let dir = tempfile::tempdir().unwrap();
        let ca_file = dir.path().join("internal-ca.pem");
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "No data sources yet.");

        harness
            .get_all_by_label("New data source")
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_by_label("PostgreSQL").click();
        harness.run();
        // Only verify-full reads a CA file, so the row only shows for it.
        assert!(harness.query_by_label("CA file").is_none());
        harness
            .get_by(|node| {
                node.role() == Role::ComboBox && node.value().as_deref() == Some("prefer")
            })
            .click();
        harness.run();
        harness.get_by_label("verify-full").click();
        harness.run();
        harness.get_by_label("Browse…");
        harness
            .get_by_role_and_label(Role::TextInput, "CA file")
            .focus();
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, "CA file")
            .type_text(&ca_file.display().to_string());
        harness.run();
        harness.get_by_label("Save").click();

        step_until_visible(&mut harness, "Expand postgres@localhost");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        let [saved] = saved.as_slice() else {
            panic!("expected one data source, got {saved:?}");
        };
        let dbrover_core::ConnectionConfig::Postgres(config) = &saved.config else {
            panic!("expected a PostgreSQL data source, got {saved:?}");
        };
        assert_eq!(
            config.server.tls,
            dbrover_core::TlsMode::VerifyFull {
                ca_file: Some(ca_file)
            }
        );
    }

    /// Types `text` into the empty field labelled `label`, whose role is `role`: a text or a
    /// password input.
    fn type_into(harness: &mut Harness<'_, DbRoverApp>, role: Role, label: &str, text: &str) {
        harness.get_by_role_and_label(role, label).focus();
        harness.run();
        harness.get_by_role_and_label(role, label).type_text(text);
        harness.run();
    }

    #[test]
    fn sets_an_ssh_server_with_a_key_file_for_a_postgres_data_source() {
        let dir = tempfile::tempdir().unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "No data sources yet.");

        harness
            .get_all_by_label("New data source")
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_by_label("PostgreSQL").click();
        harness.run();
        assert!(harness.query_by_label("SSH host").is_none());
        harness
            .get_by_label("Connect through an SSH server")
            .click();
        harness.run();
        type_into(
            &mut harness,
            Role::TextInput,
            "SSH host",
            "bastion.example.com",
        );
        type_into(&mut harness, Role::TextInput, "SSH user", "deploy");
        // The agent is the default login, and it needs no secret.
        assert!(harness.query_by_label("Passphrase").is_none());
        harness
            .get_by(|node| {
                node.role() == Role::ComboBox && node.value().as_deref() == Some("SSH agent")
            })
            .click();
        harness.run();
        harness.get_by_label("Key file").click();
        harness.run();
        harness.get_by_label("Browse…");
        type_into(
            &mut harness,
            Role::TextInput,
            "Key file",
            "~/.ssh/id_ed25519",
        );
        type_into(
            &mut harness,
            Role::PasswordInput,
            "Passphrase",
            "unlocks the key",
        );
        harness.get_by_label("Save").click();

        step_until_visible(&mut harness, "Expand postgres@localhost");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        let [saved] = saved.as_slice() else {
            panic!("expected one data source, got {saved:?}");
        };
        let dbrover_core::ConnectionConfig::Postgres(config) = &saved.config else {
            panic!("expected a PostgreSQL data source, got {saved:?}");
        };
        assert_eq!(
            config.server.ssh,
            Some(dbrover_core::SshConfig {
                host: "bastion.example.com".to_owned(),
                port: 22.into(),
                user: "deploy".to_owned(),
                login: dbrover_core::SshLogin::KeyFile {
                    key_file: "~/.ssh/id_ed25519".into()
                },
            })
        );
    }

    /// Replaces the explorer filter's text, `old`, with `new`. The field is found by its
    /// placeholder while empty, and by its text otherwise.
    fn replace_filter(harness: &mut Harness<'_, DbRoverApp>, old: &str, new: &str) {
        let old = old.to_owned();
        let filter = move || {
            let old = old.clone();
            egui_kittest::kittest::By::new().predicate(move |node| {
                node.role() == Role::TextInput
                    && if old.is_empty() {
                        node.placeholder() == Some("Filter loaded nodes")
                    } else {
                        node.value().as_deref() == Some(old.as_str())
                    }
            })
        };
        harness.get(filter()).focus();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.key_press(egui::Key::Backspace);
        harness.run();
        if !new.is_empty() {
            harness
                .get(
                    egui_kittest::kittest::By::new()
                        .predicate(|node| node.placeholder() == Some("Filter loaded nodes")),
                )
                .type_text(new);
        }
        harness.run();
    }

    fn saved_project_of(dir: &Path, name: &str) -> Option<dbrover_core::ProjectId> {
        DataSourceFile::new(dir.join("data_sources.toml"))
            .load()
            .unwrap()
            .into_iter()
            .find(|data_source| data_source.name == name)
            .and_then(|data_source| data_source.project)
    }

    fn saved_project(dir: &Path) -> dbrover_store::Project {
        let projects = ProjectFile::new(dir.join("projects.toml")).load().unwrap();
        let [project] = projects.projects.as_slice() else {
            panic!("expected one project, got {projects:?}");
        };
        project.clone()
    }

    #[test]
    fn sets_a_projects_mcp_permissions_warning_about_shared_logins() {
        let dir = tempfile::tempdir().unwrap();
        let mut shop = save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let project = dbrover_store::Project::new(
            dbrover_core::ProjectId::generate(),
            "Analytics".to_owned(),
            dbrover_store::ProjectSlug::from_name("Analytics"),
        );
        ProjectFile::new(dir.path().join("projects.toml"))
            .save(&dbrover_store::Projects {
                projects: vec![project.clone()],
                ..dbrover_store::Projects::default()
            })
            .unwrap();
        shop.project = Some(project.id);
        let warehouse = dbrover_core::DataSource {
            id: DataSourceId::generate(),
            name: "warehouse".to_owned(),
            config: dbrover_core::ConnectionConfig::Postgres(dbrover_core::PostgresConfig {
                server: dbrover_core::ServerConfig {
                    host: "warehouse.invalid".to_owned(),
                    port: 5432.into(),
                    database: "warehouse".to_owned(),
                    user: "owner".to_owned(),
                    tls: dbrover_core::TlsMode::Prefer,
                    ssh: None,
                    password_command: None,
                },
            }),
            ..shop.clone()
        };
        DataSourceFile::new(dir.path().join("data_sources.toml"))
            .save(&[shop, warehouse])
            .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "Collapse Analytics");

        harness.get_by_label("Analytics").click_secondary();
        harness.run();
        harness.get_by_label("Project settings…").click();
        harness.run();
        harness.get_by_label_contains("dbrover mcp --project analytics");
        harness
            .get_by_label("Allow MCP clients to use this project")
            .click();
        harness.run();
        assert!(
            harness.query_by_label_contains("No MCP login").is_none(),
            "reading needs no warning"
        );
        harness.get_by_label("Write").click();
        harness.run();
        harness.get_by_label_contains("No MCP login on warehouse.");
        harness.get_by_label("Save").click();
        step_until(&mut harness, "the project being saved", |harness| {
            harness
                .query_by_label("Allow MCP clients to use this project")
                .is_none()
        });
        let saved = saved_project(dir.path());
        assert!(saved.mcp_enabled);
        assert_eq!(saved.mcp.write, Some(true));
        assert_eq!(
            saved.mcp.schema, None,
            "untouched settings stay at the default"
        );

        harness.get_by_label("Analytics").click_secondary();
        harness.run();
        harness.get_by_label("Project settings…").click();
        harness.run();
        harness.get_by_label("Reset Write").click();
        harness.run();
        harness.get_by_label("Save").click();
        step_until(&mut harness, "the reset being saved", |_| {
            saved_project(dir.path()).mcp.write.is_none()
        });
    }

    #[test]
    fn saves_the_mcp_defaults_from_the_settings_tab() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "Expand shop");

        harness.get_by_label("View").click();
        harness.run();
        harness.get_by_label_contains("Settings…").click();
        harness.run();
        harness.get_by_label("MCP").click();
        harness.run();
        harness.get_by_label("Schema").click();
        harness.run();
        harness.get_by_label("Only").click();
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, "Allowed schemas")
            .focus();
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, "Allowed schemas")
            .type_text("public, reports");
        harness.run();
        harness.get_by_label("Save MCP defaults").click();
        step_until(&mut harness, "the defaults being saved", |_| {
            ProjectFile::new(dir.path().join("projects.toml"))
                .load()
                .is_ok_and(|projects| projects.mcp_defaults.schema)
        });
        let defaults = ProjectFile::new(dir.path().join("projects.toml"))
            .load()
            .unwrap()
            .mcp_defaults;
        assert_eq!(
            defaults.schemas,
            dbrover_store::SchemaScope::Only(vec!["public".to_owned(), "reports".to_owned()])
        );
        assert!(defaults.read && !defaults.write, "only what was changed");
    }

    #[test]
    fn copies_a_projects_mcp_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let mut shop = save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let project = dbrover_store::Project::new(
            dbrover_core::ProjectId::generate(),
            "Shop EU".to_owned(),
            dbrover_store::ProjectSlug::from_name("Shop EU"),
        );
        ProjectFile::new(dir.path().join("projects.toml"))
            .save(&dbrover_store::Projects {
                projects: vec![project.clone()],
                ..dbrover_store::Projects::default()
            })
            .unwrap();
        shop.project = Some(project.id);
        DataSourceFile::new(dir.path().join("data_sources.toml"))
            .save(&[shop])
            .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "Collapse Shop EU");

        harness.get_by_label("Shop EU").click_secondary();
        harness.run();
        harness.get_by_label("Copy MCP configuration").click();
        harness.step();

        let copied = harness
            .output()
            .platform_output
            .commands
            .iter()
            .find_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
            .expect("the configuration is copied");
        let parsed: serde_json::Value = serde_json::from_str(&copied).unwrap();
        let entry = &parsed["mcpServers"]["dbrover-shop-eu"];
        assert_eq!(
            entry["args"],
            serde_json::json!(["mcp", "--project", "shop-eu"])
        );
        let command = std::path::PathBuf::from(entry["command"].as_str().unwrap());
        assert!(command.is_absolute(), "{copied}");
    }

    #[test]
    fn groups_data_sources_into_projects_that_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let shop = save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let warehouse = dbrover_core::DataSource {
            id: DataSourceId::generate(),
            name: "warehouse".to_owned(),
            ..shop.clone()
        };
        DataSourceFile::new(dir.path().join("data_sources.toml"))
            .save(&[shop, warehouse])
            .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "Expand warehouse");
        assert!(
            harness.query_by_label("No project").is_none(),
            "without projects there are no group headers"
        );

        // A new project from a data source's menu takes that data source with it.
        harness.get_by_label("shop").click_secondary();
        harness.run();
        harness.get_by_label("Move to project").hover();
        harness.run();
        harness.get_by_label("New project…").click();
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, "Name")
            .type_text("Analytics");
        harness.run();
        harness.get_by_label("Create").click();
        step_until_visible(&mut harness, "Collapse Analytics");
        step_until(&mut harness, "shop moving into the project", |_| {
            saved_project_of(dir.path(), "shop").is_some()
        });
        harness.get_by_label("Collapse No project");
        assert_eq!(saved_project_of(dir.path(), "warehouse"), None);

        harness.get_by_label("Analytics").click_secondary();
        harness.run();
        harness.get_by_label("Rename…").click();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness
            .get_by_role_and_label(Role::TextInput, "Name")
            .type_text("Data");
        harness.key_press(egui::Key::Enter);
        step_until_visible(&mut harness, "Collapse Data");

        // The filter matches project names: a matching project shows all its data sources.
        replace_filter(&mut harness, "", "dat");
        step_until(&mut harness, "warehouse being filtered out", |harness| {
            harness.query_by_label("Expand warehouse").is_none()
        });
        harness.get_by_label("Expand shop");
        replace_filter(&mut harness, "dat", "ware");
        step_until(&mut harness, "the project being filtered out", |harness| {
            harness.query_by_label("Collapse Data").is_none()
        });
        harness.get_by_label("Expand warehouse");
        replace_filter(&mut harness, "ware", "");
        drop(harness);

        let projects = ProjectFile::new(dir.path().join("projects.toml"))
            .load()
            .unwrap();
        let [project] = projects.projects.as_slice() else {
            panic!("expected one project, got {projects:?}");
        };
        assert_eq!(
            (
                project.name.as_str(),
                project.slug.as_str(),
                project.mcp_enabled
            ),
            ("Data", "analytics", false)
        );
        assert_eq!(saved_project_of(dir.path(), "shop"), Some(project.id));

        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "Collapse Data");
        harness.get_by_label("Data").click_secondary();
        harness.run();
        harness.get_by_label("Delete…").click();
        harness.run();
        harness.get_by_label_contains("Its data source moves to No project");
        harness.get_by_label("Delete").click();
        step_until(&mut harness, "the project going away", |harness| {
            harness.query_by_label("Collapse Data").is_none()
        });
        harness.get_by_label("Expand shop");
        step_until(&mut harness, "shop leaving the project", |_| {
            saved_project_of(dir.path(), "shop").is_none()
        });
    }

    /// A PostgreSQL data source behind the `ssh` compose service, signing in with the test
    /// key, and the database password; `None` unless the SSH test servers are set.
    fn postgres_behind_ssh() -> Option<(dbrover_core::DataSource, String)> {
        let ssh = std::env::var("DBROVER_TEST_SSH_URL").ok()?;
        let postgres = std::env::var("DBROVER_TEST_SSH_POSTGRES_URL").ok()?;
        let (ssh_host, ssh_port) = ssh.rsplit_once('@')?.1.split_once(':')?;
        let (login, rest) = postgres.strip_prefix("postgres://")?.split_once('@')?;
        let (user, password) = login.split_once(':')?;
        let (address, database) = rest.split_once('/')?;
        let (host, port) = address.split_once(':')?;
        let key_file =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/ssh/client_ed25519");
        let data_source = dbrover_core::DataSource {
            id: DataSourceId::generate(),
            name: "behind the bastion".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: dbrover_core::ConnectionConfig::Postgres(dbrover_core::PostgresConfig {
                server: dbrover_core::ServerConfig {
                    host: host.to_owned(),
                    port: port.parse::<u16>().ok()?.into(),
                    database: database.to_owned(),
                    user: user.to_owned(),
                    tls: dbrover_core::TlsMode::Prefer,
                    ssh: Some(dbrover_core::SshConfig {
                        host: ssh_host.to_owned(),
                        port: ssh_port.parse::<u16>().ok()?.into(),
                        user: "dbrover".to_owned(),
                        login: dbrover_core::SshLogin::KeyFile { key_file },
                    }),
                    password_command: None,
                },
            }),
        };
        Some((data_source, password.to_owned()))
    }

    #[test]
    fn asks_to_trust_an_unknown_ssh_server_then_connects_through_it() {
        let Some((data_source, password)) = postgres_behind_ssh() else {
            eprintln!("skipped: set DBROVER_TEST_SSH_URL and DBROVER_TEST_SSH_POSTGRES_URL");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        DataSourceFile::new(dir.path().join("data_sources.toml"))
            .save(std::slice::from_ref(&data_source))
            .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);

        step_until_visible(&mut harness, "Expand behind the bastion");
        harness.get_by_label("Expand behind the bastion").click();
        step_until_visible(&mut harness, "Trust the SSH server");
        harness.get_by_label_contains("ssh-ed25519 host key");
        assert!(
            harness
                .query_all_by(|node| node
                    .value()
                    .is_some_and(|value| value.starts_with("SHA256:")))
                .next()
                .is_some(),
            "the fingerprint is shown"
        );
        // The modal is measured on its first frame and placed on the next, so a click aimed
        // at the first frame's button misses. A connection's spinner turns from here on, so
        // every wait is a fixed number of frames or a condition, never `run()`, which waits for
        // the UI to stop repainting and gives up while the spinner turns.
        harness.run_steps(2);
        harness.get_by_label("Trust and connect").click();
        harness.run_steps(2);

        // Trusted, it connects again, and then asks for the database's password.
        step_until_visible(&mut harness, "Connect to behind the bastion");
        harness.run_steps(2);
        harness
            .get_by_role_and_label(Role::PasswordInput, "Password")
            .type_text(&password);
        harness.run_steps(2);
        harness.get_by_label("Connect").click();
        step_until(&mut harness, "connecting through the tunnel", |harness| {
            harness
                .state()
                .state
                .data_source(data_source.id)
                .is_some_and(|entry| {
                    matches!(&entry.session, dbrover_engine::SessionState::Connected(_))
                })
        });
        let known_hosts = std::fs::read_to_string(dir.path().join("known_hosts")).unwrap();
        assert!(known_hosts.contains("ssh-ed25519"), "{known_hosts}");
    }

    #[test]
    fn asks_for_the_password_a_data_source_has_none_saved_for_then_connects() {
        let dir = tempfile::tempdir().unwrap();
        // A server that hangs up at once: the second attempt fails as soon as it is made. A
        // port nothing listens on can hang until the connect timeout on CI runners.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || listener.incoming().for_each(drop));
        let data_source = dbrover_core::DataSource {
            id: DataSourceId::generate(),
            name: "shared".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: dbrover_core::ConnectionConfig::Postgres(dbrover_core::PostgresConfig {
                server: dbrover_core::ServerConfig {
                    host: "127.0.0.1".to_owned(),
                    port: port.into(),
                    database: "app".to_owned(),
                    user: "juan".to_owned(),
                    tls: dbrover_core::TlsMode::Disable,
                    ssh: None,
                    password_command: None,
                },
            }),
        };
        DataSourceFile::new(dir.path().join("data_sources.toml"))
            .save(std::slice::from_ref(&data_source))
            .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);

        step_until_visible(&mut harness, "Expand shared");
        harness.get_by_label("Expand shared").click();
        step_until_visible(&mut harness, "Connect to shared");
        harness.get_by_label_contains("Enter the password: none is saved.");
        let save = harness.get_by_label("Save to keychain");
        assert_eq!(
            egui_kittest::kittest::NodeT::accesskit_node(&save).toggled(),
            Some(egui::accesskit::Toggled::True)
        );
        harness
            .get_by_role_and_label(Role::PasswordInput, "Password")
            .type_text("typed once");
        harness.run();
        harness.get_by_label("Connect").click();

        step_until(&mut harness, "the prompt closing", |harness| {
            harness
                .query_by_label_contains("Connect to shared")
                .is_none()
        });
        step_until(&mut harness, "connecting with the password", |harness| {
            harness
                .state()
                .state
                .data_source(data_source.id)
                .is_some_and(|entry| {
                    matches!(&entry.session, dbrover_engine::SessionState::Failed(error)
                        if error.starts_with("could not connect"))
                })
        });
    }

    #[test]
    fn exports_a_project_with_its_data_sources_and_no_password() {
        let dir = tempfile::tempdir().unwrap();
        let shop = save_shop(dir.path(), "CREATE TABLE t (id INTEGER)");
        let export_path = dir.path().join("team.toml");
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        let picked = export_path.clone();
        harness.state_mut().pick_data_sources_export = Box::new(move || Some(picked.clone()));
        step_until_visible(&mut harness, "Expand shop");

        harness.get_by_label("File").click();
        harness.run();
        harness.get_by_label("Export data sources…").click();
        harness.run();
        harness
            .get_by_role_and_label(Role::CheckBox, "shop")
            .click();
        harness.run();
        harness.get_by_label_contains("1 data source chosen");
        harness.get_by_label("Export…").click();

        step_until_visible(&mut harness, "Exported 1 data source to");
        let text = std::fs::read_to_string(&export_path).unwrap();
        assert!(text.contains(&shop.id.to_string()), "{text}");
        assert!(text.contains(r#"path = "shop.db""#), "{text}");
    }

    #[test]
    fn previews_an_import_then_imports_what_was_chosen() {
        let dir = tempfile::tempdir().unwrap();
        let shop = save_shop(dir.path(), "CREATE TABLE t (id INTEGER)");
        let analytics = dbrover_store::Project::new(
            dbrover_core::ProjectId::generate(),
            "Analytics".to_owned(),
            dbrover_store::ProjectSlug::from_name("Analytics"),
        );
        let changed_shop = dbrover_core::DataSource {
            color: Some(dbrover_core::SourceColor::Green),
            ..shop.clone()
        };
        let warehouse = dbrover_core::DataSource {
            id: DataSourceId::generate(),
            name: "warehouse".to_owned(),
            project: Some(analytics.id),
            config: dbrover_core::ConnectionConfig::Sqlite(dbrover_core::SqliteConfig {
                path: dir.path().join("elsewhere/warehouse.db"),
                read_only: true,
            }),
            ..shop.clone()
        };
        let export_path = dir.path().join("team.toml");
        dbrover_store::write_export(
            &export_path,
            &[changed_shop, warehouse],
            std::slice::from_ref(&analytics),
            None,
        )
        .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        let picked = export_path.clone();
        harness.state_mut().pick_data_sources_import = Box::new(move || Some(picked.clone()));
        step_until_visible(&mut harness, "Expand shop");

        harness.get_by_label("File").click();
        harness.run();
        harness.get_by_label("Import data sources…").click();
        step_until_visible(&mut harness, "Imports 1 project and 2 data sources.");
        harness.get_by_label_contains("Colour: none → Green");
        harness.get_by_label_contains("not found on this machine");
        for (entry, choice) in [
            ("Analytics", "Import"),
            ("shop", "Update"),
            ("warehouse", "Import"),
        ] {
            let label = format!("Import choice for {entry}");
            let combo = harness.get_by_label(&label);
            assert_eq!(combo.value().as_deref(), Some(choice), "{entry}");
        }
        harness
            .get_by_label("Import choice for shop")
            .click_accesskit();
        harness.run();
        harness.get_by_label("Skip").click_accesskit();
        harness.run();
        harness.get_by_label_contains("Imports 1 project and 1 data source.");
        harness.get_by_label("Import").click();

        step_until_visible(&mut harness, "Imported 1 project and 1 data source.");
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        assert_eq!(saved.len(), 2, "{saved:#?}");
        assert_eq!(saved[0], shop);
        assert_eq!(saved[1].name, "warehouse");
        assert_eq!(saved[1].project, Some(analytics.id));
    }

    #[test]
    fn browses_a_sqlite_catalog_down_to_columns() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(
            dir.path(),
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT NOT NULL)",
        );
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);

        for (expand, appears) in [
            ("Expand shop", "Expand main"),
            ("Expand main", "Expand tables"),
            ("Expand tables", "Expand orders"),
            ("Expand orders", "Expand columns"),
            ("Expand columns", "customer"),
        ] {
            step_until_visible(&mut harness, expand);
            harness.get_by_label(expand).click();
            step_until_visible(&mut harness, appears);
        }
        harness.get_by_label("text, not null");
        step_until_visible(&mut harness, "1 connected");
    }

    /// N0a's explorer that pages, on SQLite: a schema of more tables than a page shows the
    /// first page and **Load more…**, which loads the rest, and the explorer's filter is passed
    /// to the server for that level once typing pauses.
    #[test]
    fn pages_a_large_schema_and_filters_it_on_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let tables: String = (1..=1_100)
            .map(|n| format!("CREATE TABLE t_{n:04} (id INTEGER);"))
            .collect();
        save_shop(dir.path(), &format!("BEGIN; {tables} COMMIT;"));
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        expand_shop_tables(&mut harness, "t_0001");
        harness.get_by_label("Expand t_1000");
        assert!(harness.query_by_label("Expand t_1001").is_none());
        harness.get_by_label("Load more…");

        // t_1050 isn't loaded yet, so only the server can find it.
        replace_filter(&mut harness, "", "T_105");
        step_until_visible(&mut harness, "Expand t_1050");
        let shown = harness.query_all_by_label_contains("Expand t_").count();
        assert_eq!(shown, 10, "t_1050 to t_1059");
        assert!(harness.query_by_label("Load more…").is_none());

        replace_filter(&mut harness, "T_105", "");
        step_until_visible(&mut harness, "Expand t_0001");
        harness.get_by_label("Load more…").scroll_to_me();
        harness.run();
        harness.get_by_label("Load more…").click();
        step_until_visible(&mut harness, "Expand t_1100");
        assert!(harness.query_by_label("Load more…").is_none());
    }

    /// Expands the `shop` data source down to its tables folder, and waits until `table` shows.
    fn expand_shop_tables(harness: &mut Harness<'_, DbRoverApp>, table: &str) {
        for (expand, appears) in [
            ("Expand shop", "Expand main"),
            ("Expand main", "Expand tables"),
        ] {
            step_until_visible(harness, expand);
            harness.get_by_label(expand).click();
            step_until_visible(harness, appears);
        }
        harness.get_by_label("Expand tables").click();
        step_until_visible(harness, &format!("Expand {table}"));
    }

    fn sqlite_tables(dir: &Path) -> Vec<String> {
        let connection = rusqlite::Connection::open(dir.join("shop.db")).unwrap();
        let mut statement = connection
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn drops_a_table_from_the_explorer_and_refreshes_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(
            dir.path(),
            "CREATE TABLE customers (id INTEGER PRIMARY KEY);
             CREATE TABLE orders (id INTEGER PRIMARY KEY);",
        );
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        expand_shop_tables(&mut harness, "orders");

        harness.get_by_label("orders").click_secondary();
        harness.run_steps(2);
        harness.get_by_label("Drop…").click();
        harness.run_steps(2);
        harness.get_by_label(
            "Drops the table main.orders, with every row in it and its indexes, keys and \
             triggers.",
        );
        assert!(
            harness
                .query_all_by_value("DROP TABLE main.orders;\n")
                .next()
                .is_some()
        );
        harness.get_by_label("Execute").click();
        step_until(&mut harness, "orders leaving the tree", |harness| {
            harness.query_by_label("Expand orders").is_none()
                && harness.query_by_label("Expand customers").is_some()
        });
        assert_eq!(sqlite_tables(dir.path()), ["customers"]);
        assert!(
            harness.query_by_label("Execute").is_none(),
            "the dialog closed"
        );
    }

    /// Types into a text field found by its accessible label, waiting a frame for focus.
    fn fill(harness: &mut Harness<'_, DbRoverApp>, label: &str, text: &str) {
        harness
            .get_by_role_and_label(Role::TextInput, label)
            .focus();
        harness.run_steps(2);
        harness
            .get_by_role_and_label(Role::TextInput, label)
            .type_text(text);
        harness.run_steps(2);
    }

    /// Opens a menu button and ticks `item` in it. The button and its items may both wait on
    /// the catalog, which loads in the background (a referenced table's columns show
    /// "Loading its columns…" in the button's place), so both are waited for: clicking either
    /// at once lost that race when the app tests ran together. The item is found by role, so
    /// another label with the same text, such as a column of the same name, can't match it.
    fn pick(harness: &mut Harness<'_, DbRoverApp>, menu: &str, item: &str) {
        step_until(harness, &format!("the {menu:?} menu"), |harness| {
            harness.query_by_label(menu).is_some()
        });
        harness.get_by_label(menu).click();
        harness.run_steps(2);
        step_until(
            harness,
            &format!("{item:?} in the {menu:?} menu"),
            |harness| {
                harness
                    .query_by_role_and_label(Role::CheckBox, item)
                    .is_some()
            },
        );
        harness.get_by_role_and_label(Role::CheckBox, item).click();
        harness.run_steps(2);
        harness.key_press(egui::Key::Escape);
        harness.run_steps(2);
    }

    #[test]
    fn creates_a_table_with_a_foreign_key_through_the_dialog() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(
            dir.path(),
            "CREATE TABLE customers (id INTEGER PRIMARY KEY);",
        );
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        expand_shop_tables(&mut harness, "customers");

        harness.get_by_label("tables").click_secondary();
        harness.run_steps(2);
        harness.get_by_label("New").hover();
        harness.run_steps(2);
        harness.get_by_label("Table…").click();
        harness.run_steps(2);
        fill(&mut harness, "Name", "orders");
        harness.get_by_label("Add column").click();
        harness.run_steps(2);
        fill(&mut harness, "Column 2 name", "customer");
        fill(&mut harness, "Column 2 type", "integer");

        harness.get_by_label("Foreign keys").click();
        harness.run_steps(2);
        harness.get_by_label("Add foreign key").click();
        harness.run_steps(2);
        pick(&mut harness, "Foreign key 1 columns", "customer");
        harness.get_by_label("Foreign key 1 references").click();
        harness.run_steps(2);
        harness.get_by_label("main.customers").click();
        pick(&mut harness, "Foreign key 1 referenced columns", "id");

        let expected = "CREATE TABLE main.orders (
    id integer NOT NULL,
    customer integer,
    PRIMARY KEY (id),
    FOREIGN KEY (customer) REFERENCES customers (id)
);\n";
        assert!(
            harness.query_all_by_value(expected).next().is_some(),
            "the review panel shows the statement"
        );
        harness.get_by_label("Execute").click();
        step_until_visible(&mut harness, "Expand orders");
        assert!(
            harness.query_by_label("Execute").is_none(),
            "the dialog closed"
        );

        let connection = rusqlite::Connection::open(dir.path().join("shop.db")).unwrap();
        let target: String = connection
            .query_row(
                "SELECT \"table\" FROM pragma_foreign_key_list('orders')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(target, "customers");
    }

    /// Right-clicks the tree node labelled `node` and picks `item` from its menu.
    fn node_menu(harness: &mut Harness<'_, DbRoverApp>, node: &str, item: &str) {
        harness.get_by_label(node).click_secondary();
        harness.run_steps(2);
        harness.get_by_label(item).click();
        harness.run_steps(2);
    }

    #[test]
    fn indexes_a_table_drops_the_index_and_renames_the_table() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(
            dir.path(),
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);",
        );
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        expand_shop_tables(&mut harness, "orders");

        node_menu(&mut harness, "orders", "New index…");
        pick(&mut harness, "Index columns", "customer");
        assert!(
            harness
                .query_all_by_value("CREATE INDEX main.orders_customer_idx ON orders (customer);\n")
                .next()
                .is_some()
        );
        harness.get_by_label("Execute").click();
        step_until_visible(&mut harness, "orders_customer_idx");

        node_menu(&mut harness, "orders_customer_idx", "Drop index…");
        harness.get_by_label_contains("Drops the index main.orders_customer_idx on main.orders.");
        harness.get_by_label("Execute").click();
        step_until(&mut harness, "the index leaving the tree", |harness| {
            harness.query_by_label("orders_customer_idx").is_none()
                && harness.query_by_label("Execute").is_none()
        });

        node_menu(&mut harness, "orders", "Rename…");
        harness
            .get_by_role_and_label(Role::TextInput, "New name")
            .focus();
        harness.run_steps(2);
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness
            .get_by_role_and_label(Role::TextInput, "New name")
            .type_text("old_orders");
        harness.run_steps(2);
        assert!(
            harness
                .query_all_by_value("ALTER TABLE main.orders RENAME TO old_orders;\n")
                .next()
                .is_some()
        );
        harness.get_by_label("Execute").click();
        step_until_visible(&mut harness, "Expand old_orders");
        assert_eq!(sqlite_tables(dir.path()), ["old_orders"]);
    }

    #[test]
    fn copies_a_tables_ddl() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        expand_shop_tables(&mut harness, "orders");

        harness.get_by_label("orders").click_secondary();
        harness.run_steps(2);
        harness.get_by_label("Copy DDL").click();
        step_until(&mut harness, "the DDL being copied", |harness| {
            copied(harness).is_some()
        });
        assert_eq!(
            copied(&harness).as_deref(),
            Some("CREATE TABLE orders (id INTEGER PRIMARY KEY);")
        );
    }

    fn sqlite_view(dir: &Path, name: &str) -> Option<String> {
        let connection = rusqlite::Connection::open(dir.join("shop.db")).unwrap();
        connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'view' AND name = ?1",
                [name],
                |row| row.get(0),
            )
            .ok()
    }

    /// Replaces the text of the multi-line field labelled `label`.
    fn replace_text(harness: &mut Harness<'_, DbRoverApp>, label: &str, text: &str) {
        harness
            .get_by_role_and_label(Role::MultilineTextInput, label)
            .focus();
        harness.run_steps(2);
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness
            .get_by_role_and_label(Role::MultilineTextInput, label)
            .type_text(text);
        harness.run_steps(2);
    }

    #[test]
    fn creates_edits_and_drops_a_view_through_the_dialogs() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(
            dir.path(),
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, total REAL);",
        );
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        for (expand, appears) in [
            ("Expand shop", "Expand main"),
            ("Expand main", "Expand views"),
        ] {
            step_until_visible(&mut harness, expand);
            harness.get_by_label(expand).click();
            step_until_visible(&mut harness, appears);
        }

        harness.get_by_label("views").click_secondary();
        harness.run_steps(2);
        harness.get_by_label("New").hover();
        harness.run_steps(2);
        harness.get_by_label("View…").click();
        harness.run_steps(2);
        fill(&mut harness, "Name", "big_orders");
        replace_text(
            &mut harness,
            "Query",
            "SELECT id FROM orders WHERE total > 100",
        );
        assert!(
            harness
                .query_all_by_value(
                    "CREATE VIEW main.big_orders AS\nSELECT id FROM orders WHERE total > 100;\n"
                )
                .next()
                .is_some(),
            "the review panel shows the statement"
        );
        harness.get_by_label("Execute").click();
        step_until_visible(&mut harness, "Expand big_orders");

        node_menu(&mut harness, "big_orders", "Edit…");
        step_until(&mut harness, "the view's query loading", |harness| {
            harness
                .query_all_by_value("SELECT id FROM orders WHERE total > 100")
                .next()
                .is_some()
        });
        replace_text(
            &mut harness,
            "Query",
            "SELECT id, total FROM orders WHERE total > 500",
        );
        harness.get_by_label_contains("Drops the view main.big_orders and creates it again");
        assert!(
            harness
                .query_all_by_value(
                    "DROP VIEW main.big_orders;\n\nCREATE VIEW main.big_orders AS\nSELECT id, \
                     total FROM orders WHERE total > 500;\n"
                )
                .next()
                .is_some(),
            "SQLite drops and creates the view"
        );
        harness.get_by_label("Execute").click();
        step_until(&mut harness, "the edit applying", |harness| {
            harness.query_by_label("Execute").is_none()
        });
        assert!(
            sqlite_view(dir.path(), "big_orders").is_some_and(|sql| sql.contains("total > 500"))
        );
        // The views folder reloads after the edit.
        step_until_visible(&mut harness, "Expand big_orders");

        node_menu(&mut harness, "big_orders", "Drop…");
        harness.get_by_label_contains("Drops the view main.big_orders.");
        harness.get_by_label("Execute").click();
        step_until(&mut harness, "the view leaving the tree", |harness| {
            harness.query_by_label("Expand big_orders").is_none()
                && harness.query_by_label("Execute").is_none()
        });
        assert_eq!(sqlite_view(dir.path(), "big_orders"), None);
    }

    #[test]
    fn creates_and_drops_a_schema_through_the_dialogs() {
        let dir = tempfile::tempdir().unwrap();
        let warehouse = dbrover_core::DataSource {
            id: DataSourceId::generate(),
            name: "warehouse".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: dbrover_core::ConnectionConfig::DuckDb(dbrover_core::DuckDbConfig {
                database: dbrover_core::DuckDbDatabase::InMemory,
            }),
        };
        DataSourceFile::new(dir.path().join("data_sources.toml"))
            .save(std::slice::from_ref(&warehouse))
            .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "Expand warehouse");
        harness.get_by_label("Expand warehouse").click();
        step_until_visible(&mut harness, "Expand memory");

        harness.get_by_label("memory").click_secondary();
        harness.run_steps(2);
        harness.get_by_label("New").hover();
        harness.run_steps(2);
        harness.get_by_label("Schema…").click();
        harness.run_steps(2);
        fill(&mut harness, "Name", "sales");
        assert!(
            harness
                .query_all_by_value("CREATE SCHEMA sales;\n")
                .next()
                .is_some()
        );
        harness.get_by_label("Execute").click();
        step_until_visible(&mut harness, "Expand sales");

        node_menu(&mut harness, "sales", "Drop schema…");
        harness
            .get_by_label("Also drop everything in it (CASCADE)")
            .click();
        harness.run_steps(2);
        harness.get_by_label("Drops the schema sales and everything in it.");
        assert!(
            harness
                .query_all_by_value("DROP SCHEMA sales CASCADE;\n")
                .next()
                .is_some()
        );
        harness.get_by_label("Execute").click();
        step_until(&mut harness, "the schema leaving the tree", |harness| {
            harness.query_by_label("Expand sales").is_none()
                && harness.query_by_label("Execute").is_none()
        });
    }

    #[test]
    fn opens_table_data_in_a_console_and_loads_more_rows() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(
            dir.path(),
            "CREATE TABLE orders (id INTEGER PRIMARY KEY);
             WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 600)
             INSERT INTO orders SELECT n FROM s;",
        );
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);

        open_orders(&mut harness, "500 rows, more available");
        harness.get_by_label("Load more rows").click();
        step_until_visible(&mut harness, "600 rows ·");
    }

    #[test]
    fn restores_console_tabs_from_their_files_and_saves_edits() {
        let dir = tempfile::tempdir().unwrap();
        let shop = save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let files = ConsoleFiles::new(dir.path().join("consoles"));
        let saved = ConsoleKey {
            data_source: shop.id,
            name: ConsoleName::numbered(1),
        };
        files.save(&saved, "select 41;").unwrap();
        let deleted = ConsoleKey {
            data_source: shop.id,
            name: ConsoleName::numbered(2),
        };
        let (_layer, feed) = log_console::pipeline();
        let config = engine_config(dir.path());
        // As if the saved layout held both tabs, though one file has since been deleted.
        let mut harness = harness_at_default_size().build_eframe(|cc| {
            let mut app = DbRoverApp::new(cc, feed, config).unwrap();
            workspace::open_console_tab(&mut app.dock, saved.clone());
            workspace::open_console_tab(&mut app.dock, deleted.clone());
            app
        });

        let editor = || {
            |node: &egui_kittest::kittest::AccessKitNode<'_>| {
                node.role() == Role::MultilineTextInput
                    && node
                        .value()
                        .is_some_and(|text| text.starts_with("select 41;"))
            }
        };
        step_until(&mut harness, "the console reopening", |harness| {
            harness.query_by(editor()).is_some()
        });
        assert_eq!(
            workspace::console_tabs(&harness.state().dock),
            std::slice::from_ref(&saved)
        );

        harness.get_by(editor()).focus();
        harness.run();
        harness.get_by(editor()).type_text(" select 42;");
        step_until(&mut harness, "the edit being saved", |_| {
            std::fs::read_to_string(files.path(&saved))
                .unwrap()
                .contains("select 42;")
        });
    }

    #[test]
    fn inserts_a_statement_from_the_history_into_the_console() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        open_orders(&mut harness, "0 rows");

        harness.get_by_label("History").click();
        step_until(&mut harness, "the history loading", |harness| {
            harness.query_by_label("Insert").is_some()
        });
        harness.get_by_label("Insert").click();
        harness.run();

        let query = "SELECT * FROM main.orders;\n";
        harness.get_by_value(&query.repeat(2));
    }

    #[test]
    fn marks_and_filters_statements_from_mcp_clients() {
        let dir = tempfile::tempdir().unwrap();
        let shop = save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let from_assistant = "SELECT 'from an assistant'";
        dbrover_store::HistoryFile::new(dir.path().join("history.sqlite"))
            .open()
            .unwrap()
            .record(&dbrover_store::HistoryEntry {
                data_source: shop.id,
                executed_at: jiff::Timestamp::UNIX_EPOCH,
                sql: from_assistant.to_owned(),
                duration: None,
                rows: Some(1),
                outcome: dbrover_store::HistoryOutcome::Ok,
                source: dbrover_store::HistorySource::Mcp {
                    client: Some("claude-ai".to_owned()),
                },
            })
            .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        open_orders(&mut harness, "0 rows");

        harness.get_by_label("History").click();
        step_until(&mut harness, "the history loading", |harness| {
            harness.query_all_by_label("Insert").count() == 2
        });
        harness.get_by_label("MCP");
        harness
            .get_by(|node| {
                node.role() == Role::ComboBox && node.value().as_deref() == Some("All sources")
            })
            .click();
        harness.run();
        harness.get_by_label("MCP clients").click();
        step_until(&mut harness, "the MCP statements alone", |harness| {
            harness.query_all_by_label("Insert").count() == 1
        });
        harness.get_by_label(from_assistant);
    }

    fn count_orders(dir: &Path) -> i64 {
        rusqlite::Connection::open(dir.join("shop.db"))
            .unwrap()
            .query_row("SELECT count(*) FROM orders", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn manual_commit_mode_holds_changes_until_rolled_back_and_warns_before_closing() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "Expand shop");
        harness.get_by_label("shop").click_secondary();
        harness.run();
        // The Welcome tab has a card of the same name; the context menu comes first.
        harness
            .get_all_by_label("New query console")
            .next()
            .unwrap()
            .click();
        step_until_editor(&mut harness);
        harness
            .get_by(|node| {
                node.role() == Role::ComboBox && node.value().as_deref() == Some("Auto-commit")
            })
            .click();
        harness.run();
        harness.get_by_label("Manual commit").click();
        harness.run();
        let editor = harness.get_by_role(Role::MultilineTextInput);
        editor.focus();
        harness.run();
        harness
            .get_by_role(Role::MultilineTextInput)
            .type_text("INSERT INTO orders VALUES (1)");
        harness.run();

        let run_and_wait_for_transaction = |harness: &mut Harness<'_, DbRoverApp>| {
            harness.get_by_label("Run").click();
            step_until_visible(harness, "Transaction open");
        };
        run_and_wait_for_transaction(&mut harness);
        harness.get_by_label("Rollback").click();
        step_until_visible(&mut harness, "Rolled back");
        assert_eq!(count_orders(dir.path()), 0);

        run_and_wait_for_transaction(&mut harness);
        let key = workspace::console_tabs(&harness.state().dock)[0].clone();
        let ctx = harness.ctx.clone();
        harness
            .state_mut()
            .close_connections(Closing::Console(key), &ctx);
        step_until_visible(&mut harness, "Roll back open transactions?");
        // The modal is measured on its first frame and placed on the next; a click aimed at
        // the first frame's button lands outside it and dismisses the modal. Exactly two
        // frames, not `run()`: a spinner still turning elsewhere (the slower Windows runner
        // got there first) keeps the UI repainting, and `run()` would give up waiting for it
        // to settle.
        harness.run_steps(2);
        assert_eq!(workspace::console_tabs(&harness.state().dock).len(), 1);
        harness.get_by_label("Roll back and close").click();

        step_until(&mut harness, "the console tab closing", |harness| {
            workspace::console_tabs(&harness.state().dock).is_empty()
        });
        step_until(&mut harness, "the connection closing", |_| {
            count_orders(dir.path()) == 0
        });
    }

    /// Opens the `orders` table of a saved `shop` data source in a console, which runs
    /// `SELECT * FROM main.orders`, and waits for its rows.
    /// Frames for a modal to be sized, placed and faded in.
    const MODAL_STEPS: usize = 8;

    /// Opens the grid's export dialog, picks `rows` ("Loaded rows" or "All rows") and, when
    /// given, `format` from the format list, and leaves the dialog open for the options.
    fn open_export_dialog(harness: &mut Harness<'_, DbRoverApp>, rows: &str, format: Option<&str>) {
        harness.get_by_label("Export…").click();
        // The modal sizes and fades in over its first frames. A fixed number of them rather
        // than `run`: a console still loading rows keeps repainting, more frames than `run`
        // allows on a slow machine.
        harness.run_steps(MODAL_STEPS);
        harness.get_by_label_contains(rows).click();
        harness.run_steps(2);
        if let Some(format) = format {
            harness.get_by_label(format).click();
            harness.run_steps(2);
        }
    }

    /// Closes the Settings tab, as its × does, so the console beside it is in front again.
    /// egui_dock's close buttons have no accessible name to click them by.
    fn close_settings_tab(harness: &mut Harness<'_, DbRoverApp>) {
        let dock = &mut harness.state_mut().dock;
        let path = dock.find_tab(&Tab::Settings).expect("the Settings tab");
        dock.remove_tab(path);
        harness.run_steps(2);
    }

    /// Whether the export dialog's choice (a format tile, a separator) named `label` is the one
    /// chosen.
    fn is_chosen(harness: &Harness<'_, DbRoverApp>, label: &str) -> bool {
        use egui_kittest::kittest::NodeT;

        harness.get_by_label(label).accesskit_node().toggled()
            == Some(egui::accesskit::Toggled::True)
    }

    /// Clicks the export dialog's Export…, the last on screen: the toolbar has one too. One
    /// step, not `run`: an export of every row keeps the app repainting with its progress, more
    /// frames than `run` allows on a slow machine; callers wait for its outcome.
    fn confirm_export(harness: &mut Harness<'_, DbRoverApp>) {
        harness
            .get_all_by_label("Export…")
            .last()
            .expect("the dialog's Export…")
            .click();
        harness.step();
    }

    fn open_orders(harness: &mut Harness<'_, DbRoverApp>, rows_shown: &str) {
        for expand in ["Expand shop", "Expand main", "Expand tables"] {
            step_until_visible(harness, expand);
            harness.get_by_label(expand).click();
        }
        step_until_visible(harness, "Expand orders");
        harness.get_by_label("orders").click_secondary();
        harness.run();
        harness.get_by_label("Open data").click();
        step_until_visible(harness, rows_shown);
    }

    #[test]
    fn explains_the_statement_at_the_cursor() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        open_orders(&mut harness, "0 rows");

        harness.get_by_label("Explain").click();

        // SQLite's query plan for a full table scan, which is no table's rows to edit.
        step_until_visible(&mut harness, "SCAN main.orders");
        harness.get_by_label("Read-only");
    }

    #[test]
    fn sorts_views_and_exports_the_loaded_rows() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(
            dir.path(),
            r#"CREATE TABLE orders (id INTEGER PRIMARY KEY, note TEXT);
               INSERT INTO orders VALUES (1, 'plain'), (2, '{"gift":true}'), (3, NULL);"#,
        );
        let export_path = dir.path().join("orders.csv");
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        let picked = export_path.clone();
        harness.state_mut().pick_export_path = Box::new(move |_, _| Some(picked.clone()));
        open_orders(&mut harness, "3 rows");

        harness.get_by_label("id").click();
        harness.run();
        harness.get_by_label("id ▲").click();
        harness.run();
        harness.get_by_label("id ▼");
        harness.get_by_label_contains("Sorted and filtered here, not by the database: 3 of 3");

        harness.get_by_label(r#"{"gift":true}"#).click();
        harness.run();
        harness.key_press(egui::Key::Space);
        harness.run();
        harness.get_by_label("Value of note");
        harness.get_by_label("gift");
        harness.get_by_label("Text").click();
        harness.run();
        harness.get_by_value("{\n  \"gift\": true\n}");

        open_export_dialog(&mut harness, "Loaded rows", None);
        confirm_export(&mut harness);
        step_until_visible(&mut harness, "Exported 3 rows");
        assert_eq!(
            std::fs::read_to_string(&export_path).unwrap(),
            "id,note\r\n3,\r\n2,\"{\"\"gift\"\":true}\"\r\n1,plain\r\n"
        );
    }

    #[test]
    fn runs_with_the_shortcut_keymap_toml_binds_and_reports_its_problems() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        std::fs::write(
            dir.path().join("keymap.toml"),
            "[bindings]\n\"console.run\" = \"f5\"\n\"console.walk\" = \"f6\"\n",
        )
        .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "keymap.toml: 1 problem");
        open_console(&mut harness);
        harness
            .get_by_role(Role::MultilineTextInput)
            .type_text("SELECT 42 AS answer");
        // F5 runs what the console holds, so wait until the typed text has reached it: one frame
        // wasn't always enough on a busy CI runner, and F5 then ran an empty console.
        step_until(&mut harness, "the query to be typed", |harness| {
            editor_text(harness) == "SELECT 42 AS answer"
        });

        harness.key_press(egui::Key::F5);

        step_until_visible(&mut harness, "1 rows");
        harness.get_by_label("keymap.toml: 1 problem").click();
        harness.run();
        harness.get_by_label_contains("\"console.walk\" is not an action");
    }

    #[test]
    fn completes_a_column_from_the_catalog_it_loads_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(
            dir.path(),
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT)",
        );
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        open_console(&mut harness);

        harness
            .get_by_role(Role::MultilineTextInput)
            .type_text("SELECT * FROM orders WHERE cus");
        step_until(&mut harness, "the column to be suggested", |harness| {
            harness.query_by_label("customer").is_some()
        });
        harness.key_press(egui::Key::Enter);
        harness.run();

        harness.get_by(|node| {
            node.role() == Role::MultilineTextInput
                && node.value().as_deref() == Some("SELECT * FROM orders WHERE customer")
        });
        assert!(
            !harness.state().state.data_sources.entries()[0]
                .catalog
                .is_expanded(&dbrover_core::CatalogPath::root()),
            "completion loads the catalog without expanding the explorer"
        );
    }

    /// The PostgreSQL test server `DBROVER_TEST_POSTGRES_URL` names, as data sources on it.
    struct PostgresTestServer {
        host: String,
        port: u16,
        user: String,
        password: String,
        /// The database the URL names.
        database: String,
    }

    impl PostgresTestServer {
        /// `None`, with a note, when the variable is unset, so the test skips.
        fn from_env() -> Option<Self> {
            let Ok(url) = std::env::var("DBROVER_TEST_POSTGRES_URL") else {
                eprintln!("skipped: DBROVER_TEST_POSTGRES_URL is not set");
                return None;
            };
            let (login, rest) = url.strip_prefix("postgres://")?.split_once('@')?;
            let (user, password) = login.split_once(':')?;
            let (address, database) = rest.split_once('/')?;
            let (host, port) = address.split_once(':')?;
            Some(Self {
                host: host.to_owned(),
                port: port.parse().ok()?,
                user: user.to_owned(),
                password: password.to_owned(),
                database: database.to_owned(),
            })
        }

        fn data_source(&self, database: &str, name: &str) -> dbrover_core::DataSource {
            dbrover_core::DataSource {
                id: DataSourceId::generate(),
                name: name.to_owned(),
                color: None,
                safety: dbrover_core::SafetyLevel::Normal,
                project: None,
                mcp_login: None,
                config: dbrover_core::ConnectionConfig::Postgres(dbrover_core::PostgresConfig {
                    server: dbrover_core::ServerConfig {
                        host: self.host.clone(),
                        port: self.port.into(),
                        database: database.to_owned(),
                        user: self.user.clone(),
                        tls: dbrover_core::TlsMode::Disable,
                        ssh: None,
                        password_command: None,
                    },
                }),
            }
        }

        /// The password, kept for `data_source`, as the app's secret store would.
        fn secrets(
            &self,
            data_source: &dbrover_core::DataSource,
        ) -> crate::test_support::MemorySecrets {
            crate::test_support::MemorySecrets::with_password(
                data_source.id,
                Password::new(self.password.as_str()),
            )
        }

        /// A connection of the test's own to `database`, to set up and clean up behind the
        /// app's back.
        fn open(
            &self,
            runtime: &tokio::runtime::Runtime,
            database: &str,
        ) -> Box<dyn dbrover_core::Connection> {
            let admin = self.data_source(database, "admin");
            let secrets: Arc<dyn SecretStore> = Arc::new(self.secrets(&admin));
            let tunnels = dbrover_engine::Tunnels::new("known_hosts".into());
            let commands = dbrover_engine::PasswordCommands::new(
                dbrover_store::TrustedCommands::default(),
                Arc::new(dbrover_engine::ProcessRunner),
            );
            runtime
                .block_on(dbrover_engine::open_data_source(
                    &admin, &secrets, &tunnels, &commands,
                ))
                .unwrap()
        }
    }

    /// The first value of `sql`'s first row, on `connection`.
    fn first_value(
        runtime: &tokio::runtime::Runtime,
        connection: &mut Box<dyn dbrover_core::Connection>,
        sql: &str,
    ) -> Option<String> {
        runtime.block_on(async {
            let mut cursor = connection.execute(sql).await.unwrap();
            let rows = cursor.next_page(1).await.unwrap();
            cursor.finish().await.unwrap();
            rows.into_iter()
                .next()
                .and_then(|row| row.into_iter().next().flatten())
        })
    }

    /// The text the app last put on the clipboard, if any.
    fn copied(harness: &Harness<'_, DbRoverApp>) -> Option<String> {
        harness
            .output()
            .platform_output
            .commands
            .iter()
            .find_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
    }

    /// A PostgreSQL data source starts with the server's roles, each saying what it may do,
    /// with PostgreSQL's own `pg_` roles among the system objects; Copy DDL on one copies its
    /// `CREATE ROLE`.
    #[test]
    fn lists_postgres_roles_and_copies_one_as_ddl() {
        let Some(server) = PostgresTestServer::from_env() else {
            return;
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut admin = server.open(&runtime, "postgres");
        let role = format!("dbrover_e3_tree_{}", std::process::id());
        first_value(&runtime, &mut admin, &format!("DROP ROLE IF EXISTS {role}"));
        first_value(
            &runtime,
            &mut admin,
            &format!("CREATE ROLE {role} LOGIN CONNECTION LIMIT 2"),
        );
        let source = server.data_source(&server.database, "pg");
        let dir = tempfile::tempdir().unwrap();
        DataSourceFile::new(dir.path().join("data_sources.toml"))
            .save(std::slice::from_ref(&source))
            .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = crate::test_support::start_app(
            harness_at_default_size(),
            dir.path(),
            feed,
            server.secrets(&source),
        );
        step_until_visible(&mut harness, "Expand pg");
        harness.get_by_label("Expand pg").click();
        step_until_visible(&mut harness, "Expand roles");
        harness.get_by_label("Expand roles").click();
        step_until_visible(&mut harness, &role);
        assert!(
            harness
                .query_all_by_label("login, superuser")
                .next()
                .is_some(),
            "the test server's own role says what it may do"
        );
        assert!(
            harness.query_by_label("pg_monitor").is_none(),
            "PostgreSQL's own roles are hidden with the other system objects"
        );

        harness.get_by_label(&role).click_secondary();
        harness.run_steps(2);
        harness.get_by_label("Copy DDL").click();
        step_until(&mut harness, "the DDL being copied", |harness| {
            copied(harness).is_some()
        });
        assert_eq!(
            copied(&harness),
            Some(format!(
                "CREATE ROLE {role} WITH LOGIN CONNECTION LIMIT 2;\n"
            ))
        );

        drop(harness);
        first_value(&runtime, &mut admin, &format!("DROP ROLE {role}"));
    }

    /// Every byte of the history store, its write-ahead log included.
    fn history_bytes(dir: &Path) -> Vec<u8> {
        let mut bytes = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let is_history = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("history.sqlite"));
            if is_history && let Ok(contents) = std::fs::read(&path) {
                bytes.extend(contents);
            }
        }
        bytes
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    }

    /// E3's exit through the dialogs: New user with a password and a membership, Edit, Rename,
    /// and Drop with the table it owns; the password is redacted in the review panel and
    /// appears nowhere in the history store.
    #[test]
    fn creates_edits_renames_and_drops_a_postgres_user_through_the_dialogs() {
        let Some(server) = PostgresTestServer::from_env() else {
            return;
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        // In the data source's own database, where the user's table is counted.
        let mut admin = server.open(&runtime, &server.database);
        let group = format!("dbrover_e3_ui_readers_{}", std::process::id());
        let user = format!("dbrover_e3_ui_user_{}", std::process::id());
        let renamed = format!("{user}_renamed");
        let table = format!("dbrover_e3_ui_owned_{}", std::process::id());
        let password = format!("correct-horse-{}", std::process::id());
        first_value(
            &runtime,
            &mut admin,
            &format!("DROP TABLE IF EXISTS {table}"),
        );
        for role in [&user, &renamed, &group] {
            first_value(&runtime, &mut admin, &format!("DROP ROLE IF EXISTS {role}"));
        }
        first_value(&runtime, &mut admin, &format!("CREATE ROLE {group}"));
        let source = server.data_source(&server.database, "pg");
        let dir = tempfile::tempdir().unwrap();
        DataSourceFile::new(dir.path().join("data_sources.toml"))
            .save(std::slice::from_ref(&source))
            .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = crate::test_support::start_app(
            harness_at_default_size(),
            dir.path(),
            feed,
            server.secrets(&source),
        );
        step_until_visible(&mut harness, "Expand pg");
        harness.get_by_label("Expand pg").click();
        step_until_visible(&mut harness, "Expand roles");
        harness.get_by_label("Expand roles").click();
        step_until_visible(&mut harness, &group);

        harness.get_by_label("pg").click_secondary();
        harness.run_steps(2);
        harness.get_by_label("New").hover();
        harness.run_steps(2);
        harness.get_by_label("User…").click();
        harness.run_steps(2);
        fill(&mut harness, "Name", &user);
        for label in ["Password", "Confirm password"] {
            harness
                .get_by_role_and_label(Role::PasswordInput, label)
                .focus();
            harness.run_steps(2);
            harness
                .get_by_role_and_label(Role::PasswordInput, label)
                .type_text(&password);
            harness.run_steps(2);
        }
        // The group is in the tree too, so its tick box is told apart by its role.
        harness.get_by_label("Roles it belongs to").click();
        harness.run_steps(2);
        harness
            .get_by_role_and_label(Role::CheckBox, &group)
            .click();
        harness.run_steps(2);
        harness.key_press(egui::Key::Escape);
        harness.run_steps(2);
        let redacted =
            format!("CREATE ROLE {user} WITH LOGIN IN ROLE {group} PASSWORD '<redacted>';\n");
        assert!(
            harness.query_all_by_value(&redacted).next().is_some(),
            "the review panel redacts the password"
        );
        harness.get_by_label("Execute").click();
        step_until(&mut harness, "the user in the tree", |harness| {
            harness.query_by_label("Execute").is_none() && harness.query_by_label(&user).is_some()
        });

        node_menu(&mut harness, &user, "Edit…");
        step_until_visible(&mut harness, "Create databases");
        harness.get_by_label("Create databases").click();
        harness.run_steps(2);
        assert!(
            harness
                .query_all_by_value(&format!("ALTER ROLE {user} WITH CREATEDB;\n"))
                .next()
                .is_some()
        );
        harness.get_by_label("Execute").click();
        step_until(&mut harness, "the edit applying", |harness| {
            harness.query_by_label("Execute").is_none()
        });
        let described = first_value(
            &runtime,
            &mut admin,
            &format!(
                "SELECT r.rolcanlogin::text || r.rolcreatedb::text || count(m.roleid)::text \
                 FROM pg_roles r LEFT JOIN pg_auth_members m ON m.member = r.oid \
                 WHERE r.rolname = '{user}' GROUP BY r.rolcanlogin, r.rolcreatedb"
            ),
        );
        assert_eq!(described.as_deref(), Some("truetrue1"));

        // The roles folder reloads after the edit.
        step_until(&mut harness, "the user back in the tree", |harness| {
            harness.query_by_label(&user).is_some()
        });
        node_menu(&mut harness, &user, "Rename…");
        harness
            .get_by_role_and_label(Role::TextInput, "New name")
            .focus();
        harness.run_steps(2);
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness
            .get_by_role_and_label(Role::TextInput, "New name")
            .type_text(&renamed);
        harness.run_steps(2);
        harness.get_by_label("Execute").click();
        step_until(&mut harness, "the renamed user in the tree", |harness| {
            harness.query_by_label("Execute").is_none()
                && harness.query_by_label(&renamed).is_some()
        });

        for sql in [
            format!("CREATE TABLE {table} (id integer)"),
            format!("ALTER TABLE {table} OWNER TO {renamed}"),
        ] {
            first_value(&runtime, &mut admin, &sql);
        }
        node_menu(&mut harness, &renamed, "Drop…");
        // Owning something, it starts out giving it away.
        step_until_visible(
            &mut harness,
            "REASSIGN OWNED gives the 1 object it owns in this database",
        );
        harness.get_by_label("Drop it").click();
        harness.run_steps(2);
        harness.get_by_label(
            "DROP OWNED drops the 1 object it owns in this database, with their data.",
        );
        assert!(
            harness
                .query_all_by_value(&format!(
                    "DROP OWNED BY {renamed};\n\nDROP ROLE {renamed};\n"
                ))
                .next()
                .is_some()
        );
        harness.get_by_label("Execute").click();
        step_until(&mut harness, "the user leaving the tree", |harness| {
            harness.query_by_label("Execute").is_none()
                && harness.query_by_label(&renamed).is_none()
        });
        let left = first_value(
            &runtime,
            &mut admin,
            &format!(
                "SELECT count(*)::text FROM pg_class WHERE relname = '{table}' \
                 UNION ALL SELECT count(*)::text FROM pg_roles WHERE rolname = '{renamed}'"
            ),
        );
        assert_eq!(left.as_deref(), Some("0"), "the table went with the user");

        drop(harness);
        let history = history_bytes(dir.path());
        assert!(
            contains(&history, &format!("ALTER ROLE {user} WITH CREATEDB")),
            "the history was written"
        );
        assert!(
            !contains(&history, &password),
            "the password is in the history"
        );
        assert!(
            !contains(&history, "SCRAM-SHA-256"),
            "a verifier is in the history"
        );
        first_value(
            &runtime,
            &mut admin,
            &format!("DROP TABLE IF EXISTS {table}"),
        );
        for role in [&user, &renamed, &group] {
            first_value(&runtime, &mut admin, &format!("DROP ROLE IF EXISTS {role}"));
        }
    }

    /// Ends a console's session server-side, as a restart would, then reconnects it from its
    /// toolbar. Works in a database of its own, so ending its sessions touches no other test.
    #[test]
    fn a_console_whose_connection_was_lost_says_so_and_reconnects_when_asked() {
        let Some(server) = PostgresTestServer::from_env() else {
            return;
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut admin = server.open(&runtime, "postgres");
        let database = format!("dbrover_reconnect_{}", std::process::id());
        first_value(
            &runtime,
            &mut admin,
            &format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"),
        );
        first_value(&runtime, &mut admin, &format!("CREATE DATABASE {database}"));
        let scratch = server.data_source(&database, "scratch");
        let dir = tempfile::tempdir().unwrap();
        DataSourceFile::new(dir.path().join("data_sources.toml"))
            .save(std::slice::from_ref(&scratch))
            .unwrap();
        let (_layer, feed) = log_console::pipeline();
        let mut harness = crate::test_support::start_app(
            harness_at_default_size(),
            dir.path(),
            feed,
            server.secrets(&scratch),
        );
        step_until_visible(&mut harness, "Expand scratch");
        // Opening a console loads the catalog for completion, so a connection's spinner may
        // be turning: frames, not `run()`, which gives up while the UI keeps repainting.
        harness.get_by_label("scratch").click_secondary();
        harness.run_steps(2);
        harness
            .get_all_by_label("New query console")
            .next()
            .unwrap()
            .click();
        step_until_editor(&mut harness);
        harness.get_by_role(Role::MultilineTextInput).focus();
        harness.run_steps(2);
        harness
            .get_by_role(Role::MultilineTextInput)
            .type_text("SELECT 1");
        harness.run_steps(2);
        harness.get_by_label("Run").click();
        step_until_visible(&mut harness, "Done in");

        first_value(
            &runtime,
            &mut admin,
            &format!(
                "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
                 WHERE datname = '{database}'"
            ),
        );
        let remaining =
            format!("SELECT count(*) FROM pg_stat_activity WHERE datname = '{database}'");
        let deadline = std::time::Instant::now() + WAIT_TIMEOUT;
        while first_value(&runtime, &mut admin, &remaining).as_deref() != Some("0") {
            assert!(
                std::time::Instant::now() < deadline,
                "the session never ended"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        harness.get_by_label("Run").click();
        step_until_visible(&mut harness, "session settings are gone");
        harness.get_by_label("Reconnect").click();
        step_until(&mut harness, "the console reconnecting", |harness| {
            harness
                .query_by_label_contains("session settings are gone")
                .is_none()
                && harness.state().open_consoles().any(|console| {
                    matches!(
                        console.session(),
                        dbrover_engine::SessionState::Connected(_)
                    )
                })
        });
        harness.get_by_label("Run").click();
        step_until(&mut harness, "the query running again", |harness| {
            harness
                .state()
                .open_consoles()
                .any(|console| console.last_run_completed())
        });

        drop(harness);
        first_value(
            &runtime,
            &mut admin,
            &format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"),
        );
    }

    /// Opens a console on the shop's data source and focuses its editor.
    fn open_console(harness: &mut Harness<'_, DbRoverApp>) {
        step_until_visible(harness, "Expand shop");
        harness.get_by_label("shop").click_secondary();
        harness.run();
        // The Welcome tab has a card of the same name; the context menu comes first.
        harness
            .get_all_by_label("New query console")
            .next()
            .unwrap()
            .click();
        step_until_editor(harness);
        harness.get_by_role(Role::MultilineTextInput).focus();
        harness.run();
    }

    /// Waits for the editor of a console just opened: one frame wasn't always enough on a busy
    /// macOS runner, and the editor wasn't there yet to focus.
    fn step_until_editor(harness: &mut Harness<'_, DbRoverApp>) {
        step_until(harness, "the console's editor appearing", |harness| {
            harness.query_by_role(Role::MultilineTextInput).is_some()
        });
    }

    fn editor_text(harness: &Harness<'_, DbRoverApp>) -> String {
        harness
            .get_by_role(Role::MultilineTextInput)
            .value()
            .unwrap_or_default()
    }

    #[test]
    fn formats_a_statement_undoes_it_in_one_step_and_follows_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        open_console(&mut harness);
        let typed = "select id,customer from orders where id=1";
        harness
            .get_by_role(Role::MultilineTextInput)
            .type_text(typed);
        // Every wait below is on a condition or a fixed two frames, never `run()`: the console's
        // connection spinner can keep the app repainting, and `run()` then fails on its frame
        // limit (it did on a busy CI runner).
        step_until(&mut harness, "the query to be typed", |harness| {
            editor_text(harness) == typed
        });

        harness.key_press_modifiers(
            egui::Modifiers::COMMAND | egui::Modifiers::ALT,
            egui::Key::L,
        );
        step_until(&mut harness, "the statement to be formatted", |harness| {
            editor_text(harness) == "SELECT\n    id,\n    customer\nFROM orders\nWHERE id = 1"
        });
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        step_until(
            &mut harness,
            "one undo to restore the typed text",
            |harness| editor_text(harness) == typed,
        );

        harness.get_by_label("View").click();
        harness.run_steps(2);
        harness.get_by_label_contains("Settings…").click();
        harness.run_steps(2);
        harness.get_by_label("Editor").click();
        harness.run_steps(2);
        harness.get_by_label("lower").click();
        harness.run_steps(2);
        close_settings_tab(&mut harness);
        harness.get_by_label("Code").click();
        harness.run_steps(2);
        harness.get_by_label_contains("Format all").click();
        step_until(
            &mut harness,
            "the console to be formatted in lower case",
            |harness| {
                editor_text(harness) == "select\n    id,\n    customer\nfrom orders\nwhere id = 1"
            },
        );
        step_until(&mut harness, "the settings file", |_| {
            std::fs::read_to_string(dir.path().join("settings.toml"))
                .is_ok_and(|text| text.contains("keyword_case = \"lower\""))
        });
    }

    #[test]
    fn underlines_problems_while_typing_and_explains_them_on_hover() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(dir.path(), "CREATE TABLE orders (id INTEGER PRIMARY KEY)");
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        open_console(&mut harness);

        harness
            .get_by_role(Role::MultilineTextInput)
            .type_text("select * from orders where id = null;\nselect id from orders where");
        // Checked once typing pauses.
        step_until_visible(&mut harness, "1 error, 1 warning");
        let squiggles: Vec<egui::Rect> = harness
            .state()
            .consoles
            .open_consoles()
            .flat_map(|console| console.squiggle_rects().collect::<Vec<_>>())
            .collect();
        assert_eq!(squiggles.len(), 2, "one squiggle per problem");

        harness.hover_at(squiggles[0].center());
        step_until_visible(&mut harness, "= NULL is never true; use IS NULL");
        harness.get_by_label_contains("null-comparison");
        harness.hover_at(squiggles[1].center());
        step_until_visible(&mut harness, "Syntax error: expected an expression");
    }

    #[test]
    fn completes_tables_after_from_and_an_aliases_columns_after_its_dot() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(
            dir.path(),
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
             CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT);",
        );
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        open_console(&mut harness);
        let editor_is = |harness: &Harness<'_, DbRoverApp>, text: &str| {
            harness
                .query_by(|node| {
                    node.role() == Role::MultilineTextInput && node.value().as_deref() == Some(text)
                })
                .is_some()
        };

        harness
            .get_by_role(Role::MultilineTextInput)
            .type_text("SELECT * FROM cus");
        step_until(&mut harness, "the table to be suggested", |harness| {
            harness.query_by_label("customers").is_some()
        });
        assert!(
            harness.query_by_label("customer").is_none(),
            "after FROM, tables are offered, not columns"
        );
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert!(editor_is(&harness, "SELECT * FROM customers"));

        harness
            .get_by_role(Role::MultilineTextInput)
            .type_text(" c WHERE c.na");
        step_until(
            &mut harness,
            "the alias's column to be suggested",
            |harness| harness.query_by_label("name").is_some(),
        );
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert!(editor_is(
            &harness,
            "SELECT * FROM customers c WHERE c.name"
        ));
    }

    #[test]
    fn tells_when_a_newer_release_is_out_and_when_it_is_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let (_layer, feed) = log_console::pipeline();
        let config = engine_config_with_releases(dir.path(), FixedReleases::tagged("v99.0.0"));
        let mut harness =
            harness_at_default_size().build_eframe(|cc| DbRoverApp::new(cc, feed, config).unwrap());

        // Checked at start-up: the status bar says so, and its link opens the dialog.
        step_until_visible(&mut harness, "DbRover 99.0.0 is available");
        harness.get_by_label("DbRover 99.0.0 is available").click();
        step_until_visible(&mut harness, "DbRover 99.0.0 is available (you have");
        harness.get_by_label("Open release page");

        // The fake installer "installs" at once; the dialog and the status bar then ask for a
        // restart, which this test doesn't take, since it would close the harness's window.
        // A modal that has just opened is laid out again on its next frame; a click before then
        // can land outside it, which closes it.
        harness.run_steps(2);
        harness.get_by_label("Install and restart").click();
        step_until_visible(&mut harness, "DbRover 99.0.0 is installed.");
        harness.get_by_label("Restart now");
        harness.get_by_label("Restart to finish updating");
        drop(harness);

        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "Engine running");
        harness.get_by_label("Help").click();
        harness.run_steps(2);
        harness.get_by_label("Check for updates…").click();
        step_until_visible(&mut harness, "You're up to date");
        assert!(harness.query_by_label_contains("is available").is_none());
    }

    /// Edits the `ada` cell of the open orders to `Ada Lovelace`, leaving it to be submitted.
    fn rename_ada(harness: &mut Harness<'_, DbRoverApp>) {
        let cell_editor = |harness: &Harness<'_, DbRoverApp>| {
            harness
                .query_by(|node| {
                    node.role() == Role::TextInput && node.value().as_deref() == Some("ada")
                })
                .is_some()
        };

        harness.get_by_label("ada").click();
        // One frame lands the click. The engine is still describing the table, and each of its
        // answers wakes the app, so `run` here could exceed its steps; the loop below waits.
        harness.step();
        // F2 edits once the table has been described; until then it does nothing.
        let deadline = std::time::Instant::now() + WAIT_TIMEOUT;
        while !cell_editor(harness) {
            assert!(
                std::time::Instant::now() < deadline,
                "the cell editor never opened"
            );
            harness.key_press(egui::Key::F2);
            harness.step();
            std::thread::sleep(Duration::from_millis(10));
        }
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness
            .get_by(|node| node.role() == Role::TextInput && node.value().as_deref() == Some("ada"))
            .type_text("Ada Lovelace");
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.get_by_label("1 changed cell");
        assert!(
            harness
                .query_by(|node| {
                    node.role() == Role::TextInput
                        && node.value().as_deref() == Some("Ada Lovelace")
                })
                .is_none(),
            "Enter closes the editor and keeps it closed"
        );
    }

    /// Saves `shop` over a database of two orders, at `safety`.
    fn save_orders_at(dir: &Path, safety: dbrover_core::SafetyLevel) {
        let shop = save_shop(
            dir,
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
             INSERT INTO orders VALUES (1, 'ada'), (2, 'bob');",
        );
        DataSourceFile::new(dir.join("data_sources.toml"))
            .save(&[dbrover_core::DataSource { safety, ..shop }])
            .unwrap();
    }

    fn customer_of_order_1(dir: &Path) -> String {
        rusqlite::Connection::open(dir.join("shop.db"))
            .unwrap()
            .query_row("SELECT customer FROM orders WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn a_careful_data_source_asks_before_a_grid_edit_naming_itself() {
        let dir = tempfile::tempdir().unwrap();
        save_orders_at(dir.path(), dbrover_core::SafetyLevel::Careful);
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        open_orders(&mut harness, "2 rows");
        harness.get_by_label_contains("Careful");
        harness.get_by(|node| {
            node.role() == Role::ComboBox && node.value().as_deref() == Some("Manual commit")
        });

        rename_ada(&mut harness);
        harness.get_by_label("Submit").click();
        step_until_visible(&mut harness, "Apply edits to \u{201c}shop\u{201d}?");
        harness
            .get_by_label_contains("\u{201c}shop\u{201d} is careful: this updates 1 edited row.");
        assert_eq!(
            customer_of_order_1(dir.path()),
            "ada",
            "nothing runs before the answer"
        );
        // The dialog settles where it is centred before a pointer can find its buttons.
        harness.run();
        harness.get_by_label("Apply").click();

        step_until(&mut harness, "the edit being saved", |harness| {
            harness.query_by_label("1 changed cell").is_none()
                && harness.query_by_label("Saving…").is_none()
        });
        // Manual commit mode opened a transaction with the first statement, so the edit waits
        // in it for Commit.
        harness.get_by_label("● Transaction open");
        assert_eq!(customer_of_order_1(dir.path()), "ada");
        harness.get_by_label("Commit").click();
        step_until_visible(&mut harness, "Committed");
        assert_eq!(customer_of_order_1(dir.path()), "Ada Lovelace");
    }

    #[test]
    fn a_read_only_data_source_says_why_it_refused_a_statement() {
        let dir = tempfile::tempdir().unwrap();
        save_orders_at(dir.path(), dbrover_core::SafetyLevel::ReadOnly);
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        open_console(&mut harness);
        harness.get_by_label_contains("Read-only");

        harness
            .get_by_role(Role::MultilineTextInput)
            .type_text("UPDATE orders SET customer = 'x' WHERE id = 1");
        harness.run();
        harness.get_by_label("Run all").click();

        step_until_visible(
            &mut harness,
            "Refused: this statement changes data, and \u{201c}shop\u{201d} is read-only.",
        );
        assert_eq!(customer_of_order_1(dir.path()), "ada");
    }

    #[test]
    fn red_suggests_careful_in_the_data_source_dialog() {
        let dir = tempfile::tempdir().unwrap();
        let shop = save_shop(dir.path(), "CREATE TABLE t (id INTEGER)");
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        step_until_visible(&mut harness, "Expand shop");
        harness.get_by_label("shop").click_secondary();
        harness.run();
        harness.get_by_label("Edit…").click();
        harness.run();

        harness.get_by_label("Red").click();
        harness.run();
        harness.get_by_label("Red usually marks production.");
        harness.get_by_label("Make it careful").click();
        harness.run();
        assert!(
            harness
                .query_by_label("Red usually marks production.")
                .is_none()
        );
        harness.get_by_label("Save").click();

        step_until(&mut harness, "the data source being saved", |_| {
            DataSourceFile::new(dir.path().join("data_sources.toml"))
                .load()
                .is_ok_and(|saved| {
                    saved.first().map(|ds| ds.safety) == Some(dbrover_core::SafetyLevel::Careful)
                })
        });
        let saved = DataSourceFile::new(dir.path().join("data_sources.toml"))
            .load()
            .unwrap();
        assert_eq!(saved[0].id, shop.id);
        assert_eq!(saved[0].color, Some(dbrover_core::SourceColor::Red));
    }

    #[test]
    fn edits_a_cell_and_submits_it_as_an_update() {
        let dir = tempfile::tempdir().unwrap();
        save_shop(
            dir.path(),
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT);
             INSERT INTO orders VALUES (1, 'ada'), (2, 'bob');",
        );
        let (_layer, feed) = log_console::pipeline();
        let mut harness = start_app(dir.path(), feed);
        open_orders(&mut harness, "2 rows");
        rename_ada(&mut harness);
        harness.get_by_label("Submit").click();

        // Submit swaps "1 changed cell" for "Saving…" at once; only when that goes too has the
        // engine written the UPDATE. Waiting on the first label alone raced the database read
        // below, and lost on the slower Linux and Windows CI runners.
        step_until(&mut harness, "the edit being saved", |harness| {
            harness.query_by_label("1 changed cell").is_none()
                && harness.query_by_label("Saving…").is_none()
        });
        harness.get_by_label("Ada Lovelace");
        let saved: String = rusqlite::Connection::open(dir.path().join("shop.db"))
            .unwrap()
            .query_row("SELECT customer FROM orders WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(saved, "Ada Lovelace");
    }
}
