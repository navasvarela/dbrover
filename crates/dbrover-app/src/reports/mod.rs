//! Reports in the app (`docs/reporting.md`, RP1f, RP2e and RP4): the files in `reports/`, read
//! for the explorer's **Reports**; each report's tab, with its parameters, output folder, last
//! run and the runs the history keeps; and the one run that may be going, in its worker, kept
//! in the history once it ends.
//!
//! The app reads and plans a report itself to show what is wrong and to check values as they
//! are typed, but never runs one: the worker reads the file again and runs it, so what runs is
//! what is saved.

mod file;
mod highlight;
mod insights;
mod launcher;
mod problems;
mod rename_dialog;
mod runs_table;
mod tab;

use std::collections::BTreeMap;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use dbrover_core::DataSource;
use dbrover_engine::{Command, Event, ReportSaveFailure, ReportText};
use dbrover_export::CustomFormat;
use dbrover_report::{
    Batch, CombineLimits, LimitDefaults, LoadedColumn, OutputLimits, Plan, Preview, Report,
    RunEvent, RunFailure, RunRequest, SourceLimits, SourceStart, Step, TypedValues, WrittenFile,
    read_report,
};
use dbrover_store::{
    CombineMetrics, DEFAULT_RUNS_KEPT, OutputMetrics, ReportFiles, ReportName, ReportRun,
    ReportSettings, RunError, RunOrigin, RunOutcome, SourceMetrics, StepMetrics,
};
use jiff::Timestamp;

pub use file::{CheckInputs, ReportFile, SaveError, Saved};
pub use launcher::{PARENT_ENV, ProcessLauncher, ReportLauncher, RunningReport};
pub use rename_dialog::{RenameAction, RenameReportDialog};
pub use tab::{ReportView, show};

/// What a new report starts as: a commented trial balance on the first data source, written
/// as the user will write their own.
const NEW_REPORT: &str = include_str!("new_report.toml");

/// How many of a report's runs its tab asks the history for: as many as fit side by side.
pub const RUNS_LISTED: usize = 5;

/// The limits a report runs under where its file leaves one out: Settings › Reports' where it
/// sets one, else DbRover's own.
pub fn limit_defaults(settings: &ReportSettings) -> LimitDefaults {
    let constants = LimitDefaults::default();
    LimitDefaults {
        source: SourceLimits {
            max_rows: settings
                .max_rows
                .map_or(constants.source.max_rows, |rows| rows.get()),
            max_bytes: settings.max_bytes.unwrap_or(constants.source.max_bytes),
            timeout: settings.source_timeout.unwrap_or(constants.source.timeout),
        },
        combine: CombineLimits {
            timeout: settings
                .combine_timeout
                .unwrap_or(constants.combine.timeout),
            memory_limit: settings
                .memory_limit
                .unwrap_or(constants.combine.memory_limit),
            threads: settings
                .threads
                .and_then(|threads| usize::try_from(threads.get()).ok())
                .and_then(NonZeroUsize::new)
                .or(constants.combine.threads),
            max_temp_size: settings
                .max_temp_size
                .unwrap_or(constants.combine.max_temp_size),
        },
        output: OutputLimits {
            max_size: settings.max_size.unwrap_or(constants.output.max_size),
        },
        run_timeout: settings.run_timeout.unwrap_or(constants.run_timeout),
    }
}

/// How many runs of each report the history keeps.
pub fn runs_kept(settings: &ReportSettings) -> NonZeroU32 {
    settings.runs_kept.unwrap_or(DEFAULT_RUNS_KEPT)
}

/// What the user asked of the Reports, in the explorer or a report's tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportAction {
    Open(ReportName),
    New,
    /// Lists `reports/` again, and reads every report's file.
    Reload,
    OpenFolder,
    Run(ReportName),
    Cancel,
    ChooseFolder(ReportName),
    /// Opens a file or folder with the system's own app.
    OpenPath(PathBuf),
    /// Writes the report's file from its tab's text.
    Save(ReportName),
    /// Deletes every run of the report the history keeps.
    ClearRuns(ReportName),
    /// Asks for a new name for the report's file, which its runs follow.
    Rename(ReportName),
    /// Opens Settings › Reports, where the limits' defaults are.
    OpenSettings,
}

/// Where reports are kept and how they run: given by `main`, and by tests that use them.
pub struct ReportsSetup {
    pub files: ReportFiles,
    pub launcher: Box<dyn ReportLauncher>,
    /// Where a report's files go until one is chosen for it.
    pub default_folder: PathBuf,
}

/// The reports, their tabs' state, and the run going on.
#[derive(Default)]
pub struct Reports {
    setup: Option<ReportsSetup>,
    listing: Listing,
    states: BTreeMap<ReportName, ReportState>,
    running: Option<Running>,
    /// A report just created, whose tab opens once the listing has it.
    to_open: Option<ReportName>,
    /// Each report's newest runs, as the history last listed them.
    runs: BTreeMap<ReportName, Result<Vec<ReportRun>, String>>,
}

#[derive(Default)]
enum Listing {
    /// Not asked for yet: there is no reports folder, or the engine hasn't answered.
    #[default]
    Unknown,
    Loaded(Vec<Listed>),
    Unavailable(String),
}

/// A report file, as read.
pub struct Listed {
    pub name: ReportName,
    pub path: PathBuf,
    pub read: Result<Report, Vec<String>>,
    /// The file's text, and when it was last modified; `None` if it couldn't be read.
    pub text: Option<(String, Option<SystemTime>)>,
}

impl Listed {
    fn new(found: &ReportText) -> Self {
        let read = match &found.text {
            Ok(text) => read_report(text),
            Err(error) => Err(vec![format!("could not read the file: {error}")]),
        };
        Self {
            name: found.name.clone(),
            path: found.path.clone(),
            read,
            text: found
                .text
                .as_ref()
                .ok()
                .map(|text| (text.clone(), found.modified)),
        }
    }

    /// The file as its tab just wrote it.
    fn written(&mut self, text: String, modified: Option<SystemTime>) {
        self.read = read_report(&text);
        self.text = Some((text, modified));
    }

    /// The name in `[report]`, or the file's when it can't be read.
    pub fn title(&self) -> &str {
        match &self.read {
            Ok(report) => &report.name,
            Err(_) => self.name.as_str(),
        }
    }

    /// The report against the data sources, presets and limits' defaults as they are now.
    pub fn plan(
        &self,
        data_sources: &[DataSource],
        presets: &[CustomFormat],
        defaults: &LimitDefaults,
    ) -> Result<Plan, Vec<String>> {
        let report = self.read.as_ref().map_err(Clone::clone)?;
        Plan::new(report.clone(), data_sources, presets, defaults)
    }
}

/// What a report's tab holds between frames.
#[derive(Default)]
pub struct ReportState {
    pub values: TypedValues,
    /// `None` until one is chosen: the default folder.
    pub folder: Option<PathBuf>,
    pub last_run: Option<RunRecord>,
    /// Whether **Clear history** is asking to be sure.
    pub confirm_clear: bool,
    /// The file as the tab edits it, once the tab has read it.
    pub file: Option<ReportFile>,
    /// **Hide the file**: the tab shows the run's inputs and results only.
    pub file_hidden: bool,
    /// The run panel's tab the user picked last; `None` follows the report's runs.
    pub run_panel: Option<RunPanel>,
}

/// The run panel's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPanel {
    LastRun,
    Preview,
    Runs,
}

/// What the app does after the reports took in an event.
#[derive(Debug, Default)]
pub struct Applied {
    /// A report just created, whose tab to open.
    pub open: Option<ReportName>,
    pub command: Option<Command>,
    /// A report saved by **Save and run**, to run now.
    pub run: Option<ReportName>,
}

/// One run, as its events have told it so far.
pub struct RunRecord {
    pub started: Instant,
    /// When it started, for the history.
    pub started_at: Timestamp,
    /// The parameters' values it was started with.
    pub params: TypedValues,
    /// Each source as the run's start listed them, then the combine step if there is one.
    pub steps: Vec<StepRecord>,
    /// The rows the outputs were given: the source's, or the combine step's.
    pub rows: u64,
    pub preview: Option<Preview>,
    pub files: Vec<WrittenFile>,
    pub end: Option<RunEnd>,
    /// What each step that finished measured, in the order they finished.
    pub measured: Vec<StepMetrics>,
}

/// A source or the combine step, as far as the run has gone.
pub struct StepRecord {
    pub step: Step,
    /// The data source a source reads; `None` for the combine step.
    pub data_source: Option<String>,
    pub attached: bool,
    /// The source whose values this one's lookup reads, which it waits for.
    pub waits_for: Option<String>,
    pub state: StepState,
    /// The lookup's batch being read, while the source runs, or how many it ran, once done.
    pub batches: Option<Batch>,
    /// A loaded source's table in DuckDB, its columns and their types.
    pub table: Vec<LoadedColumn>,
    pub warnings: Vec<String>,
    /// For a combine step in PRQL, the SQL it compiled to, which is what ran.
    pub sql: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Waiting,
    /// What it has returned so far.
    Running {
        rows: u64,
        bytes: u64,
        elapsed_ms: u64,
    },
    Done {
        rows: u64,
        bytes: u64,
        elapsed_ms: u64,
    },
    /// An attached DuckDB file, ready for the combine step.
    Attached,
    Failed,
}

pub enum RunEnd {
    Finished { elapsed_ms: u64 },
    Failed(RunFailure),
    Cancelled,
}

impl RunRecord {
    /// A run starting now with `params`.
    pub fn new(params: TypedValues) -> Self {
        Self {
            started: Instant::now(),
            started_at: Timestamp::now(),
            params,
            steps: Vec::new(),
            rows: 0,
            preview: None,
            files: Vec::new(),
            end: None,
            measured: Vec::new(),
        }
    }

    /// The run as the history keeps it, once it has ended, started from `origin`.
    pub fn to_report_run(&self, report: &ReportName, origin: RunOrigin) -> Option<ReportRun> {
        let (elapsed, outcome) = match self.end.as_ref()? {
            RunEnd::Finished { elapsed_ms } => {
                (Duration::from_millis(*elapsed_ms), RunOutcome::Finished)
            }
            RunEnd::Failed(failure) => (
                self.started.elapsed(),
                RunOutcome::Failed(RunError {
                    step: failure.step.to_string(),
                    message: failure.message.clone(),
                }),
            ),
            RunEnd::Cancelled => (self.started.elapsed(), RunOutcome::Cancelled),
        };
        Some(ReportRun {
            report: report.clone(),
            started_at: self.started_at,
            elapsed,
            outcome,
            params: self.params.clone(),
            steps: self.measured.clone(),
            origin,
        })
    }

    /// The data source of the source `id`, as the run's start named it.
    fn data_source_of(&self, id: &str) -> String {
        self.steps
            .iter()
            .find(|record| matches!(&record.step, Step::Source { id: step } if step == id))
            .and_then(|record| record.data_source.clone())
            .unwrap_or_default()
    }

    /// Keeps what a step measured, from the event that says it is done.
    fn measure(&mut self, event: &RunEvent) {
        let measured = match event {
            RunEvent::SourceDone {
                id,
                rows,
                bytes,
                first_row_ms,
                elapsed_ms,
                batches,
                ..
            } => StepMetrics::Source(SourceMetrics {
                id: id.clone(),
                data_source: self.data_source_of(id),
                rows: *rows,
                bytes: *bytes,
                first_row_ms: *first_row_ms,
                elapsed_ms: *elapsed_ms,
                batches: *batches,
            }),
            RunEvent::SourceAttached { id, .. } => StepMetrics::Attached {
                id: id.clone(),
                data_source: self.data_source_of(id),
            },
            RunEvent::CombineDone {
                rows,
                bytes,
                first_row_ms,
                elapsed_ms,
                peak_memory,
                peak_temp_size,
            } => StepMetrics::Combine(CombineMetrics {
                rows: *rows,
                bytes: *bytes,
                first_row_ms: *first_row_ms,
                elapsed_ms: *elapsed_ms,
                peak_memory: *peak_memory,
                peak_temp_size: *peak_temp_size,
            }),
            RunEvent::OutputWritten(file) => StepMetrics::Output(OutputMetrics {
                // The outputs' files are put in place in the report's order.
                number: self.files.len() + 1,
                file: file.path.file_name().map_or_else(
                    || file.path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                ),
                rows: file.rows,
                size: file.size,
                elapsed_ms: file.elapsed_ms,
            }),
            _ => return,
        };
        self.measured.push(measured);
    }

    /// Whether a combine step makes the rows the outputs write.
    pub fn combines(&self) -> bool {
        self.steps.iter().any(|step| step.step == Step::Combine)
    }

    fn step_mut(&mut self, step: &Step) -> Option<&mut StepRecord> {
        self.steps.iter_mut().find(|record| record.step == *step)
    }

    /// Takes in the run's next event.
    pub fn apply(&mut self, event: RunEvent) {
        self.measure(&event);
        match event {
            RunEvent::Started {
                sources,
                combine,
                combine_sql,
                ..
            } => {
                self.steps = sources.into_iter().map(StepRecord::source).collect();
                if combine {
                    self.steps.push(StepRecord {
                        sql: combine_sql,
                        ..StepRecord::new(Step::Combine, None, false)
                    });
                }
            }
            RunEvent::Progress {
                step,
                rows,
                bytes,
                elapsed_ms,
                batch,
            } => {
                if let Some(record) = self.step_mut(&step) {
                    record.state = StepState::Running {
                        rows,
                        bytes,
                        elapsed_ms,
                    };
                    record.batches = batch;
                }
            }
            RunEvent::SourceDone {
                id,
                rows,
                bytes,
                elapsed_ms,
                table,
                warnings,
                batches,
                ..
            } => {
                if !self.combines() {
                    self.rows = rows;
                }
                if let Some(record) = self.step_mut(&Step::Source { id }) {
                    record.state = StepState::Done {
                        rows,
                        bytes,
                        elapsed_ms,
                    };
                    record.batches = batches.map(|of| Batch { number: of, of });
                    record.table = table;
                    record.warnings = warnings;
                }
            }
            RunEvent::SourceAttached { id, .. } => {
                if let Some(record) = self.step_mut(&Step::Source { id }) {
                    record.state = StepState::Attached;
                }
            }
            RunEvent::CombineDone {
                rows,
                bytes,
                elapsed_ms,
                ..
            } => {
                self.rows = rows;
                if let Some(record) = self.step_mut(&Step::Combine) {
                    record.state = StepState::Done {
                        rows,
                        bytes,
                        elapsed_ms,
                    };
                }
            }
            RunEvent::Preview(preview) => self.preview = Some(preview),
            RunEvent::OutputWritten(file) => self.files.push(file),
            RunEvent::Finished { elapsed_ms } => self.end = Some(RunEnd::Finished { elapsed_ms }),
            RunEvent::Failed(failure) => {
                if let Some(record) = self.step_mut(&failure.step) {
                    record.state = StepState::Failed;
                }
                self.end = Some(RunEnd::Failed(failure));
            }
            RunEvent::Cancelled => self.end = Some(RunEnd::Cancelled),
        }
    }
}

impl StepRecord {
    fn new(step: Step, data_source: Option<String>, attached: bool) -> Self {
        Self {
            step,
            data_source,
            attached,
            waits_for: None,
            state: StepState::Waiting,
            batches: None,
            table: Vec::new(),
            warnings: Vec::new(),
            sql: None,
        }
    }

    fn source(source: SourceStart) -> Self {
        Self {
            waits_for: source.waits_for,
            ..Self::new(
                Step::Source { id: source.id },
                Some(source.data_source),
                source.attached,
            )
        }
    }

    /// Whether the step has nothing more to do.
    pub fn is_over(&self) -> bool {
        matches!(
            self.state,
            StepState::Done { .. } | StepState::Attached | StepState::Failed
        )
    }
}

struct Running {
    report: ReportName,
    /// What the run runs, to say who set a limit it stops at that the worker can't place: the
    /// run's own time limit, enforced from the app's side.
    plan: Plan,
    run: Box<dyn RunningReport>,
}

impl Reports {
    pub fn new(setup: ReportsSetup) -> Self {
        Self {
            setup: Some(setup),
            ..Self::default()
        }
    }

    /// The command that reads `reports/` again, if there is one to read.
    pub fn reload(&self) -> Option<Command> {
        let setup = self.setup.as_ref()?;
        Some(Command::ListReports(setup.files.clone()))
    }

    /// The command that renames `from`'s file to `to` and moves its runs; `None` while it
    /// runs, since the worker reads the file by its name.
    pub fn rename(&self, from: &ReportName, to: &ReportName) -> Option<Command> {
        if self.running() == Some(from) {
            return None;
        }
        let setup = self.setup.as_ref()?;
        Some(Command::RenameReport {
            files: setup.files.clone(),
            from: from.clone(),
            to: to.clone(),
            listed: RUNS_LISTED,
        })
    }

    /// The command that writes a new report, if there is a folder to write it in, starting
    /// on `data_source` when there is one.
    pub fn create(&self, data_source: Option<&str>) -> Option<Command> {
        let setup = self.setup.as_ref()?;
        // As a TOML string, quoted and escaped, whatever the data source's name holds.
        let data_source = toml::Value::String(data_source.unwrap_or("my-database").to_owned());
        let text = NEW_REPORT.replace("\"{data_source}\"", &data_source.to_string());
        Some(Command::CreateReport {
            files: setup.files.clone(),
            name: ReportName::new("report")?,
            text,
        })
    }

    pub fn folder(&self) -> Option<&Path> {
        Some(self.setup.as_ref()?.files.dir())
    }

    /// Takes in what the engine read or wrote.
    pub fn apply(&mut self, event: &Event) -> Applied {
        match event {
            Event::ReportsListed(Ok(found)) => {
                let listed: Vec<Listed> = found.iter().map(Listed::new).collect();
                let created = self
                    .to_open
                    .take_if(|name| listed.iter().any(|listed| &listed.name == name));
                self.follow_files(&listed);
                self.listing = Listing::Loaded(listed);
                Applied {
                    open: created,
                    ..Applied::default()
                }
            }
            Event::ReportsListed(Err(error)) => {
                self.listing = Listing::Unavailable(error.clone());
                Applied::default()
            }
            Event::ReportCreated(Ok(name)) => {
                self.to_open = Some(name.clone());
                Applied {
                    command: self.reload(),
                    ..Applied::default()
                }
            }
            Event::ReportCreated(Err(error)) => {
                tracing::error!("could not create a report: {error}");
                Applied::default()
            }
            Event::ReportRenamed {
                from,
                result: Ok(to),
            } => {
                if let Some(state) = self.states.remove(from) {
                    self.states.insert(to.clone(), state);
                }
                self.runs.remove(from);
                Applied {
                    command: self.reload(),
                    ..Applied::default()
                }
            }
            Event::ReportRunsFound { report, result } => {
                if let Err(error) = result {
                    tracing::warn!("could not read the runs of {report}: {error}");
                }
                self.runs.insert(report.clone(), result.clone());
                Applied::default()
            }
            Event::ReportSaved { name, result } => self.saved(name, result),
            _ => Applied::default(),
        }
    }

    /// Shows each open tab the file as listed, where nothing typed in it would be lost.
    fn follow_files(&mut self, listed: &[Listed]) {
        for found in listed {
            let file = self
                .states
                .get_mut(&found.name)
                .and_then(|state| state.file.as_mut());
            if let (Some(file), Some((text, modified))) = (file, &found.text) {
                file.read_again(text, *modified);
            }
        }
    }

    /// Takes in a save's answer: the listing holds the text written, and **Save and run** runs.
    fn saved(
        &mut self,
        name: &ReportName,
        result: &Result<Option<SystemTime>, ReportSaveFailure>,
    ) -> Applied {
        let Some(file) = self
            .states
            .get_mut(name)
            .and_then(|state| state.file.as_mut())
        else {
            return Applied::default();
        };
        let Saved::Written { text, then_run } = file.saved(result) else {
            return Applied::default();
        };
        if let Listing::Loaded(listed) = &mut self.listing
            && let Some(found) = listed.iter_mut().find(|listed| &listed.name == name)
        {
            let modified = result.as_ref().ok().copied().flatten();
            found.written(text, modified);
        }
        Applied {
            run: then_run.then(|| name.clone()),
            ..Applied::default()
        }
    }

    /// The file `name`'s tab edits, read from the listing the first time it is asked for.
    pub fn file_mut(&mut self, name: &ReportName) -> Option<&mut ReportFile> {
        let read = self
            .find(name)
            .and_then(|listed| listed.text.clone())
            .map(|(text, modified)| ReportFile::new(text, modified));
        let state = self.state_mut(name);
        if state.file.is_none() {
            state.file = read;
        }
        state.file.as_mut()
    }

    /// The command that writes `name`'s file from its tab's text, if there is one to send.
    pub fn save(&mut self, name: &ReportName) -> Option<Command> {
        let files = self.setup.as_ref()?.files.clone();
        self.file_mut(name)?.save(&files, name)
    }

    /// Saves `name`'s file, then runs it once written.
    pub fn save_and_run(&mut self, name: &ReportName) -> Option<Command> {
        let files = self.setup.as_ref()?.files.clone();
        self.file_mut(name)?.save_and_run(&files, name)
    }

    /// Whether `name`'s tab has text its file doesn't.
    pub fn is_unsaved(&self, name: &ReportName) -> bool {
        self.state(name)
            .and_then(|state| state.file.as_ref())
            .is_some_and(ReportFile::is_unsaved)
    }

    /// Each report whose tab has unsaved changes, and its title.
    pub fn unsaved(&self) -> Vec<(ReportName, String)> {
        self.states
            .iter()
            .filter(|(_, state)| state.file.as_ref().is_some_and(ReportFile::is_unsaved))
            .map(|(name, _)| {
                let title = self
                    .find(name)
                    .map_or_else(|| name.to_string(), |listed| listed.title().to_owned());
                (name.clone(), title)
            })
            .collect()
    }

    /// Forgets the text of `name`'s tab, which is closing.
    pub fn close(&mut self, name: &ReportName) {
        if let Some(state) = self.states.get_mut(name) {
            state.file = None;
        }
    }

    /// Whether the explorer shows a Reports section at all.
    pub fn is_set_up(&self) -> bool {
        self.setup.is_some()
    }

    pub fn listed(&self) -> &[Listed] {
        match &self.listing {
            Listing::Loaded(listed) => listed,
            Listing::Unknown | Listing::Unavailable(_) => &[],
        }
    }

    pub fn unavailable(&self) -> Option<&str> {
        match &self.listing {
            Listing::Unavailable(error) => Some(error),
            Listing::Unknown | Listing::Loaded(_) => None,
        }
    }

    pub fn find(&self, name: &ReportName) -> Option<&Listed> {
        self.listed().iter().find(|listed| &listed.name == name)
    }

    pub fn state_mut(&mut self, name: &ReportName) -> &mut ReportState {
        self.states.entry(name.clone()).or_default()
    }

    pub fn state(&self, name: &ReportName) -> Option<&ReportState> {
        self.states.get(name)
    }

    /// The folder a report's files go in: the one chosen for it, or the default.
    pub fn output_folder(&self, name: &ReportName) -> Option<PathBuf> {
        let chosen = self.state(name).and_then(|state| state.folder.clone());
        chosen.or_else(|| Some(self.setup.as_ref()?.default_folder.clone()))
    }

    /// The report running now, if one is.
    pub fn running(&self) -> Option<&ReportName> {
        self.running.as_ref().map(|running| &running.report)
    }

    /// Starts `name`, planned as `plan` under `defaults`, in a worker with the values typed in
    /// its tab, unless a report is running. `wake` asks the UI to look at the run's events.
    pub fn start(
        &mut self,
        name: &ReportName,
        plan: &Plan,
        defaults: LimitDefaults,
        wake: impl Fn() + Send + Sync + 'static,
    ) {
        if self.running.is_some() {
            return;
        }
        let (Some(setup), Some(listed), Some(folder)) = (
            self.setup.as_ref(),
            self.find(name),
            self.output_folder(name),
        ) else {
            return;
        };
        let values = self
            .state(name)
            .map(|state| state.values.clone())
            .unwrap_or_default();
        let request = RunRequest {
            report: listed.path.clone(),
            values: values.clone(),
            staging: folder.join(staging_name()),
            folder,
            defaults,
            memory_limit: None,
        };
        let started = setup.launcher.start(&request, plan.timeout, Box::new(wake));
        let mut record = RunRecord::new(values);
        match started {
            Ok(run) => {
                self.running = Some(Running {
                    report: name.clone(),
                    plan: plan.clone(),
                    run,
                });
            }
            Err(error) => {
                record.end = Some(RunEnd::Failed(RunFailure::new(
                    Step::Worker,
                    format!("could not start the worker: {error}"),
                )));
            }
        }
        let state = self.state_mut(name);
        state.last_run = Some(record);
        // The run panel follows the run, whichever tab was picked before.
        state.run_panel = None;
    }

    pub fn cancel(&mut self) {
        if let Some(running) = &mut self.running {
            running.run.cancel();
        }
    }

    /// Takes in what the running report's worker said since the last frame. Returns the
    /// command that keeps the run in the history, with the newest `kept` of its report, once it
    /// has ended.
    pub fn poll(&mut self, kept: NonZeroU32) -> Option<Command> {
        let running = self.running.as_mut()?;
        let mut events = Vec::new();
        while let Some(mut event) = running.run.try_next() {
            if let RunEvent::Failed(failure) = &mut event {
                failure.place_limit(&running.plan);
            }
            events.push(event);
        }
        let report = running.report.clone();
        let record = self.state_mut(&report).last_run.as_mut()?;
        for event in events {
            record.apply(event);
        }
        let run = record.to_report_run(&report, RunOrigin::App)?;
        self.running = None;
        Some(Command::RecordReportRun {
            run: Box::new(run),
            kept,
            listed: RUNS_LISTED,
        })
    }

    /// `name`'s newest runs as the history last listed them, newest first; `None` until it has.
    pub fn runs(&self, name: &ReportName) -> Option<&Result<Vec<ReportRun>, String>> {
        self.runs.get(name)
    }

    /// The command that asks the history for `name`'s newest runs.
    pub fn list_runs(name: &ReportName) -> Command {
        Command::ListReportRuns {
            report: name.clone(),
            listed: RUNS_LISTED,
        }
    }
}

/// A staging folder's name no other run of this app, or of another DbRover, has: hidden, and
/// named after the process and the time, which is all one run needs to be told apart.
pub fn staging_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!(".dbrover-report-{}-{nanos}", std::process::id())
}

/// Opens a file or folder with the system's own app, as double-clicking it would: a
/// workbook in its spreadsheet app, a folder in the file manager.
///
/// Not `egui`'s `open_url` with a `file://` address: that hands the address to the default
/// browser, which on macOS opened a report's TOML, and its written files, in the browser.
pub fn open_path(path: &Path) {
    let mut command = opener(path);
    match command.spawn() {
        // Waited for on a thread of its own, so the exited opener leaves no zombie behind.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(error) => tracing::warn!("could not open {}: {error}", path.display()),
    }
}

/// The platform's command that opens `path` with its app.
fn opener(path: &Path) -> std::process::Command {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let mut command = std::process::Command::new(program);
    command.arg(path);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_a_path_with_the_platforms_opener() {
        let command = opener(Path::new("/Users/ada/My Reports/sales.xlsx"));
        let expected = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        assert_eq!(command.get_program(), expected);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["/Users/ada/My Reports/sales.xlsx"]);
    }

    #[test]
    fn keeps_a_run_however_it_ends() {
        let failure = RunFailure::new(Step::Combine, "no memory");
        let cases = [
            (
                RunEvent::Finished { elapsed_ms: 1_200 },
                RunOutcome::Finished,
                Some(Duration::from_millis(1_200)),
            ),
            (
                RunEvent::Failed(failure),
                RunOutcome::Failed(RunError {
                    step: "the combine step".to_owned(),
                    message: "no memory".to_owned(),
                }),
                None,
            ),
            (RunEvent::Cancelled, RunOutcome::Cancelled, None),
        ];
        let report = ReportName::new("trial-balance").unwrap();
        for (end, outcome, elapsed) in cases {
            let params: TypedValues = [("since".to_owned(), "2026-10-01".to_owned())].into();
            let mut record = RunRecord::new(params.clone());
            record.apply(RunEvent::Started {
                report: "Trial balance".to_owned(),
                sources: vec![SourceStart {
                    id: "balances".to_owned(),
                    data_source: "ledger".to_owned(),
                    attached: false,
                    waits_for: None,
                }],
                combine: false,
                combine_sql: None,
            });
            record.apply(RunEvent::SourceDone {
                id: "balances".to_owned(),
                rows: 20,
                bytes: 400,
                first_row_ms: Some(3),
                elapsed_ms: 50,
                table: Vec::new(),
                warnings: Vec::new(),
                batches: None,
            });
            record.apply(RunEvent::OutputWritten(WrittenFile {
                path: PathBuf::from("/out/tb-2026-10.csv"),
                rows: 20,
                size: 512,
                elapsed_ms: 2,
                warnings: Vec::new(),
            }));
            assert!(
                record.to_report_run(&report, RunOrigin::App).is_none(),
                "still running"
            );
            record.apply(end);

            let run = record.to_report_run(&report, RunOrigin::App).unwrap();
            assert_eq!(run.outcome, outcome);
            if let Some(elapsed) = elapsed {
                assert_eq!(run.elapsed, elapsed);
            }
            assert_eq!(run.params, params);
            assert_eq!(
                run.steps,
                [
                    StepMetrics::Source(SourceMetrics {
                        id: "balances".to_owned(),
                        data_source: "ledger".to_owned(),
                        rows: 20,
                        bytes: 400,
                        first_row_ms: Some(3),
                        elapsed_ms: 50,
                        batches: None,
                    }),
                    StepMetrics::Output(OutputMetrics {
                        number: 1,
                        file: "tb-2026-10.csv".to_owned(),
                        rows: 20,
                        size: 512,
                        elapsed_ms: 2,
                    }),
                ]
            );
        }
    }

    #[test]
    fn follows_each_step_of_a_combined_run() {
        let source = |id: &str, attached| SourceStart {
            id: id.to_owned(),
            data_source: "ledger".to_owned(),
            attached,
            waits_for: None,
        };
        let mut record = RunRecord::new(TypedValues::new());
        record.apply(RunEvent::Started {
            report: "Valuation".to_owned(),
            sources: vec![source("tx", false), source("prices", true)],
            combine: true,
            combine_sql: None,
        });
        let states = |record: &RunRecord| -> Vec<StepState> {
            record.steps.iter().map(|step| step.state).collect()
        };
        assert_eq!(states(&record), [StepState::Waiting; 3]);
        assert!(record.combines());

        record.apply(RunEvent::SourceAttached {
            id: "prices".to_owned(),
            path: PathBuf::from("/tmp/prices.duckdb"),
        });
        record.apply(RunEvent::Progress {
            step: Step::Source {
                id: "tx".to_owned(),
            },
            rows: 1_000,
            bytes: 20_000,
            elapsed_ms: 1_000,
            batch: None,
        });
        assert_eq!(
            states(&record),
            [
                StepState::Running {
                    rows: 1_000,
                    bytes: 20_000,
                    elapsed_ms: 1_000
                },
                StepState::Attached,
                StepState::Waiting
            ]
        );
        record.apply(RunEvent::SourceDone {
            id: "tx".to_owned(),
            rows: 1_500,
            bytes: 30_000,
            first_row_ms: Some(10),
            elapsed_ms: 1_200,
            table: vec![LoadedColumn {
                name: "quantity".to_owned(),
                type_name: "DECIMAL(21,18)".to_owned(),
                declared: false,
            }],
            warnings: vec!["a warning".to_owned()],
            batches: None,
        });
        assert_eq!(record.rows, 0, "a loaded source's rows aren't the outputs'");
        assert_eq!(record.steps[0].table.len(), 1);
        assert_eq!(record.steps[0].warnings, ["a warning"]);
        record.apply(RunEvent::Failed(RunFailure::new(
            Step::Combine,
            "no memory",
        )));
        assert_eq!(record.steps[2].state, StepState::Failed);
        assert!(matches!(record.end, Some(RunEnd::Failed(_))));
    }
}
