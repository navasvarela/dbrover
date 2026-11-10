//! Reports in the app (`docs/reporting.md`, RP1f, RP2e and RP4): the files in `reports/`, read
//! for the explorer's **Reports**; each report's tab, with its parameters, output folder, last
//! run and the runs the history keeps; and the one run that may be going, in its worker, kept
//! in the history once it ends.
//!
//! The app reads and plans a report itself to show what is wrong and to check values as they
//! are typed, but never runs one: the worker reads the file again and runs it, so what runs is
//! what is saved.

pub mod compare;
mod compare_view;
mod explain_view;
pub mod explanation;
mod file;
mod funnel;
mod highlight;
pub mod hints;
pub mod insights;
mod launcher;
pub mod live;
mod problems;
pub mod profile;
mod profile_view;
mod rename_dialog;
mod runs_table;
mod tab;

use std::collections::BTreeMap;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use dbrover_core::{BlobId, DataSource};
use dbrover_engine::{Command, Event, ReportSaveFailure, ReportText, WatchedFile};
use dbrover_export::CustomFormat;
use dbrover_report::{
    Batch, Busy, CombineCaps, CombineLimits, ExplainRequest, Explanation, LimitDefaults,
    LoadedColumn, OutputLimits, Plan, Preview, Report, RunEvent, RunFailure, RunRequest,
    SourceCaps, SourceLimits, SourcePhases, SourceStart, Step, TimedEvent, TypedValues,
    WrittenFile, read_report,
};
use dbrover_sql::FormatOptions;
use dbrover_store::{
    CombineMetrics, DEFAULT_RUNS_KEPT, KeptColumn, OutputMetrics, ReportFiles, ReportName,
    ReportRun, ReportSettings, RunError, RunOrigin, RunOutcome, RunProfile, SourceMetrics,
    StepMetrics,
};
use jiff::Timestamp;

pub use file::{CheckInputs, OnDisk, ReportFile, SaveError, Saved};
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
    /// Plans the report without running it (RS2d).
    Explain(ReportName),
    Cancel,
    ChooseFolder(ReportName),
    /// Opens a file or folder with the system's own app.
    OpenPath(PathBuf),
    /// Writes the report's file from its tab's text.
    Save(ReportName),
    /// Formats the report's text in its tab.
    Format(ReportName),
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
    /// The report being explained, in a worker of its own, beside any run.
    explaining: Option<Explaining>,
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
    /// Which of the runs listed **Profile** shows, counted from the newest.
    pub profile_run: usize,
    /// The combine step's plan **Profile** draws, kept between frames for what is folded and
    /// selected in it.
    pub profile_plan: Option<ProfilePlan>,
    /// The two runs **Compare** shows.
    pub compare_runs: ComparedRuns,
    /// What **Explain** found, the last time it was asked.
    pub explanation: Option<ExplainState>,
}

/// Where **Explain** is.
pub enum ExplainState {
    Explaining,
    Explained(Box<Explanation>),
    /// The worker couldn't explain the report: its file, a parameter, or the worker itself.
    Failed(String),
}

/// A report being explained by a worker.
struct Explaining {
    report: ReportName,
    run: Box<dyn RunningReport>,
}

/// A kept run's combine plan, as **Profile** draws it.
pub struct ProfilePlan {
    /// When the run started, which tells it from the others.
    pub started_at: Timestamp,
    pub view: crate::console::plan::PlanView,
}

/// The two runs **Compare** shows, each by its place among the runs listed, counted from the
/// newest: the one before the newest, and the newest, until others are picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComparedRuns {
    pub before: usize,
    pub after: usize,
}

impl Default for ComparedRuns {
    fn default() -> Self {
        Self {
            before: 1,
            after: 0,
        }
    }
}

/// The run panel's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPanel {
    LastRun,
    Preview,
    Profile,
    Compare,
    Runs,
    /// What **Explain** found.
    Explain,
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
    /// The Git blob id of the report file's text that runs, as the run's start said.
    pub version: Option<BlobId>,
    /// The combine step's limits, as the run's start said.
    combine_limits: Option<CombineCaps>,
    /// What the run measured of itself so far.
    profile: RunProfile,
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
    /// A source's engine, as the run's start named it.
    pub engine: Option<String>,
    /// A source's caps, as the run's start gave them.
    pub limits: Option<SourceCaps>,
    /// While it runs: its rows a second.
    pub pace: live::Pace,
    /// While it runs: what it spent its last second on.
    pub busy: Option<Busy>,
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
            version: None,
            combine_limits: None,
            profile: RunProfile::default(),
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
            version: self.version,
            profile: self.profile.clone(),
            id: None,
        })
    }

    /// The source `id`, as the run's start listed it.
    fn source_record(&self, id: &str) -> Option<&StepRecord> {
        self.steps
            .iter()
            .find(|record| matches!(&record.step, Step::Source { id: step } if step == id))
    }

    /// The data source of the source `id`, as the run's start named it.
    fn data_source_of(&self, id: &str) -> String {
        self.source_record(id)
            .and_then(|record| record.data_source.clone())
            .unwrap_or_default()
    }

    /// When the step whose rows the outputs write started: the combine step, or the report's
    /// one source without it, which has finished by the time a file is written.
    fn feed_started_ms(&self) -> Option<u64> {
        self.measured.iter().rev().find_map(|step| match step {
            StepMetrics::Combine(combine) => combine.started_ms,
            StepMetrics::Source(source) if !self.combines() => source.started_ms,
            _ => None,
        })
    }

    /// Keeps what a step measured, from the event that says it is done, sent `at_ms` into
    /// the run; and what the run measured of itself.
    fn measure(&mut self, event: &RunEvent, at_ms: u64) {
        let started_ms = |elapsed_ms: u64| Some(at_ms.saturating_sub(elapsed_ms));
        let measured = match event {
            RunEvent::Started {
                timeout_ms,
                machine,
                combine_limits,
                ..
            } => {
                self.combine_limits = *combine_limits;
                self.profile.timeout_ms = *timeout_ms;
                self.profile.cores = machine.as_ref().map(|machine| machine.cores);
                self.profile.dbrover = machine.as_ref().map(|machine| machine.dbrover.clone());
                return;
            }
            RunEvent::WorkerUsage { peak_memory } => {
                self.profile.peak_memory = *peak_memory;
                return;
            }
            RunEvent::SourceDone {
                id,
                rows,
                bytes,
                first_row_ms,
                elapsed_ms,
                batches,
                phases,
                table,
                ..
            } => {
                let record = self.source_record(id);
                StepMetrics::Source(SourceMetrics {
                    id: id.clone(),
                    data_source: self.data_source_of(id),
                    rows: *rows,
                    bytes: *bytes,
                    first_row_ms: *first_row_ms,
                    elapsed_ms: *elapsed_ms,
                    batches: *batches,
                    started_ms: started_ms(*elapsed_ms),
                    engine: record.and_then(|record| record.engine.clone()),
                    waits_for: record.and_then(|record| record.waits_for.clone()),
                    phases: phases.map(kept_phases),
                    limits: record.and_then(|record| record.limits).map(|limits| {
                        dbrover_store::SourceCaps {
                            max_rows: limits.max_rows,
                            max_bytes: limits.max_bytes,
                            timeout_ms: limits.timeout_ms,
                        }
                    }),
                    columns: table.iter().map(kept_column).collect(),
                })
            }
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
                query_ms,
                plan,
            } => StepMetrics::Combine(CombineMetrics {
                rows: *rows,
                bytes: *bytes,
                first_row_ms: *first_row_ms,
                elapsed_ms: *elapsed_ms,
                peak_memory: *peak_memory,
                peak_temp_size: *peak_temp_size,
                started_ms: started_ms(*elapsed_ms),
                query_ms: *query_ms,
                limits: self
                    .combine_limits
                    .map(|limits| dbrover_store::CombineCaps {
                        memory_limit: limits.memory_limit,
                        max_temp_size: limits.max_temp_size,
                        timeout_ms: limits.timeout_ms,
                    }),
                plan: plan.clone(),
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
                started_ms: self.feed_started_ms(),
                ended_ms: Some(at_ms),
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
    pub fn apply(&mut self, timed: TimedEvent) {
        let TimedEvent { at_ms, event } = timed;
        self.measure(&event, at_ms);
        match event {
            RunEvent::Started {
                sources,
                combine,
                combine_sql,
                version,
                ..
            } => {
                self.version = version;
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
                busy,
            } => {
                if let Some(record) = self.step_mut(&step) {
                    record.state = StepState::Running {
                        rows,
                        bytes,
                        elapsed_ms,
                    };
                    record.batches = batch;
                    record.pace.see(elapsed_ms, rows, bytes);
                    record.busy = busy;
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
            RunEvent::WorkerUsage { .. } | RunEvent::Explained(_) => {}
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

/// A source's phases as the history keeps them.
fn kept_phases(phases: SourcePhases) -> dbrover_store::SourcePhases {
    let SourcePhases {
        connect_ms,
        server_ms,
        transfer_ms,
        load_ms,
        type_ms,
    } = phases;
    dbrover_store::SourcePhases {
        connect_ms,
        server_ms,
        transfer_ms,
        load_ms,
        type_ms,
    }
}

fn kept_column(column: &LoadedColumn) -> KeptColumn {
    KeptColumn {
        name: column.name.clone(),
        type_name: column.type_name.clone(),
        bytes: column.bytes,
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
            engine: None,
            limits: None,
            pace: live::Pace::default(),
            busy: None,
        }
    }

    fn source(source: SourceStart) -> Self {
        Self {
            waits_for: source.waits_for,
            engine: source.engine,
            limits: source.limits,
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
            Event::SqlFileChanged { path, contents } => {
                for file in self
                    .states
                    .values_mut()
                    .filter_map(|state| state.file.as_mut())
                {
                    if file.path() == path {
                        file.changed_on_disk(contents.as_ref());
                    }
                }
                Applied::default()
            }
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
        let read = self.find(name).and_then(|listed| {
            let (text, modified) = listed.text.clone()?;
            Some(ReportFile::new(listed.path.clone(), text, modified))
        });
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

    /// Formats `name`'s text, the SQL in each source's dialect among `data_sources`.
    pub fn format(
        &mut self,
        name: &ReportName,
        data_sources: &[DataSource],
        options: &FormatOptions,
    ) {
        if let Some(file) = self.file_mut(name) {
            file.format(data_sources, options);
        }
    }

    /// The open tabs' files, to look at for changes other programs made.
    pub fn watched_files(&self) -> Vec<WatchedFile> {
        self.states
            .values()
            .filter_map(|state| state.file.as_ref())
            .map(ReportFile::watched)
            .collect()
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
            explain: None,
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

    /// Explains `name`, planned as `plan`, in a worker with the values typed in its tab, with
    /// the tables its newest finished run loaded, unless one is being explained already.
    pub fn explain(
        &mut self,
        name: &ReportName,
        plan: &Plan,
        defaults: LimitDefaults,
        wake: impl Fn() + Send + Sync + 'static,
    ) {
        if self.explaining.is_some() {
            return;
        }
        let (Some(setup), Some(listed), Some(folder)) = (
            self.setup.as_ref(),
            self.find(name),
            self.output_folder(name),
        ) else {
            return;
        };
        let tables = match self.runs(name) {
            Some(Ok(runs)) => explanation::known_tables(runs),
            _ => Vec::new(),
        };
        let request = RunRequest {
            report: listed.path.clone(),
            values: self
                .state(name)
                .map(|state| state.values.clone())
                .unwrap_or_default(),
            staging: folder.join(staging_name()),
            folder,
            defaults,
            memory_limit: None,
            explain: Some(ExplainRequest { tables }),
        };
        let started = setup.launcher.start(&request, plan.timeout, Box::new(wake));
        let state = match started {
            Ok(run) => {
                self.explaining = Some(Explaining {
                    report: name.clone(),
                    run,
                });
                ExplainState::Explaining
            }
            Err(error) => ExplainState::Failed(format!("could not start the worker: {error}")),
        };
        let report = self.state_mut(name);
        report.explanation = Some(state);
        report.run_panel = Some(RunPanel::Explain);
    }

    /// Whether `name` is being explained now.
    pub fn is_explaining(&self, name: &ReportName) -> bool {
        self.explaining
            .as_ref()
            .is_some_and(|explaining| &explaining.report == name)
    }

    /// Takes in what the explaining worker said since the last frame.
    pub fn poll_explain(&mut self) {
        let Some(explaining) = self.explaining.as_mut() else {
            return;
        };
        let mut found = None;
        let mut ended = false;
        while let Some(timed) = explaining.run.try_next() {
            ended |= timed.event.ends_run();
            match timed.event {
                RunEvent::Explained(explanation) => {
                    found = Some(ExplainState::Explained(explanation));
                }
                RunEvent::Failed(failure) => {
                    found = Some(ExplainState::Failed(failure.to_string()))
                }
                RunEvent::Cancelled => {
                    found = Some(ExplainState::Failed("Cancelled.".to_owned()));
                }
                _ => {}
            }
        }
        let report = explaining.report.clone();
        if ended {
            self.explaining = None;
            found = found.or_else(|| {
                Some(ExplainState::Failed(
                    "the worker ended without explaining the report".to_owned(),
                ))
            });
        }
        if let Some(found) = found {
            self.state_mut(&report).explanation = Some(found);
        }
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
        while let Some(mut timed) = running.run.try_next() {
            if let RunEvent::Failed(failure) = &mut timed.event {
                failure.place_limit(&running.plan);
            }
            events.push(timed);
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

    /// Takes in `event` as sent at the run's start, for a run whose times don't matter.
    fn untimed(record: &mut RunRecord, event: RunEvent) {
        record.apply(event.at(Duration::ZERO));
    }

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
            untimed(
                &mut record,
                RunEvent::Started {
                    report: "Trial balance".to_owned(),
                    sources: vec![SourceStart {
                        id: "balances".to_owned(),
                        data_source: "ledger".to_owned(),
                        attached: false,
                        waits_for: None,
                        engine: None,
                        limits: None,
                    }],
                    combine: false,
                    combine_sql: None,
                    version: None,
                    combine_limits: None,
                    timeout_ms: None,
                    machine: None,
                },
            );
            untimed(
                &mut record,
                RunEvent::SourceDone {
                    id: "balances".to_owned(),
                    rows: 20,
                    bytes: 400,
                    first_row_ms: Some(3),
                    elapsed_ms: 50,
                    table: Vec::new(),
                    warnings: Vec::new(),
                    batches: None,
                    phases: None,
                },
            );
            untimed(
                &mut record,
                RunEvent::OutputWritten(WrittenFile {
                    path: PathBuf::from("/out/tb-2026-10.csv"),
                    rows: 20,
                    size: 512,
                    elapsed_ms: 2,
                    warnings: Vec::new(),
                }),
            );
            assert!(
                record.to_report_run(&report, RunOrigin::App).is_none(),
                "still running"
            );
            untimed(&mut record, end);

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
                        // Sent at the run's start, as `untimed` sends every event.
                        started_ms: Some(0),
                        engine: None,
                        phases: None,
                        limits: None,
                        waits_for: None,
                        columns: Vec::new(),
                    }),
                    StepMetrics::Output(OutputMetrics {
                        number: 1,
                        file: "tb-2026-10.csv".to_owned(),
                        rows: 20,
                        size: 512,
                        elapsed_ms: 2,
                        started_ms: Some(0),
                        ended_ms: Some(0),
                    }),
                ]
            );
        }
    }

    #[test]
    fn keeps_each_steps_place_in_the_run_its_phases_and_its_limits() {
        let caps = SourceCaps {
            max_rows: 1_000,
            max_bytes: 1 << 20,
            timeout_ms: 60_000,
        };
        let combine_caps = CombineCaps {
            memory_limit: 1 << 30,
            max_temp_size: 1 << 32,
            timeout_ms: 600_000,
        };
        let phases = SourcePhases {
            connect_ms: 10,
            server_ms: 700,
            transfer_ms: 200,
            load_ms: 80,
            type_ms: 10,
        };
        let mut record = RunRecord::new(TypedValues::new());
        let events = [
            (
                0,
                RunEvent::Started {
                    report: "Valuation".to_owned(),
                    sources: vec![SourceStart {
                        id: "tx".to_owned(),
                        data_source: "ledger".to_owned(),
                        attached: false,
                        waits_for: None,
                        engine: Some("PostgreSQL".to_owned()),
                        limits: Some(caps),
                    }],
                    combine: true,
                    combine_sql: None,
                    version: None,
                    combine_limits: Some(combine_caps),
                    timeout_ms: Some(1_800_000),
                    machine: Some(dbrover_report::Machine {
                        cores: 10,
                        dbrover: "0.31.0".to_owned(),
                    }),
                },
            ),
            (
                1_020,
                RunEvent::SourceDone {
                    id: "tx".to_owned(),
                    rows: 10,
                    bytes: 100,
                    first_row_ms: Some(710),
                    elapsed_ms: 1_000,
                    table: Vec::new(),
                    warnings: Vec::new(),
                    batches: None,
                    phases: Some(phases),
                },
            ),
            (
                1_500,
                RunEvent::CombineDone {
                    rows: 3,
                    bytes: 30,
                    first_row_ms: Some(400),
                    elapsed_ms: 450,
                    peak_memory: None,
                    peak_temp_size: None,
                    query_ms: Some(420),
                    plan: None,
                },
            ),
            (
                1_510,
                RunEvent::OutputWritten(WrittenFile {
                    path: PathBuf::from("/out/v.csv"),
                    rows: 3,
                    size: 40,
                    elapsed_ms: 30,
                    warnings: Vec::new(),
                }),
            ),
            (
                1_515,
                RunEvent::WorkerUsage {
                    peak_memory: Some(90 << 20),
                },
            ),
            (1_516, RunEvent::Finished { elapsed_ms: 1_516 }),
        ];
        for (at_ms, event) in events {
            record.apply(TimedEvent { at_ms, event });
        }
        let report = ReportName::new("valuation").unwrap();
        let run = record.to_report_run(&report, RunOrigin::App).unwrap();
        let [
            StepMetrics::Source(source),
            StepMetrics::Combine(combine),
            StepMetrics::Output(output),
        ] = run.steps.as_slice()
        else {
            panic!("{:?}", run.steps);
        };
        assert_eq!(source.started_ms, Some(20));
        assert_eq!(source.engine.as_deref(), Some("PostgreSQL"));
        assert_eq!(source.phases, Some(kept_phases(phases)));
        assert_eq!(source.limits.map(|limits| limits.max_rows), Some(1_000));
        assert_eq!(combine.started_ms, Some(1_050));
        assert_eq!(combine.query_ms, Some(420));
        assert_eq!(
            combine.limits.map(|limits| limits.memory_limit),
            Some(1 << 30)
        );
        // A file is started with the rows of the step that feeds it.
        assert_eq!(
            (output.started_ms, output.ended_ms),
            (Some(1_050), Some(1_510))
        );
        assert_eq!(
            run.profile,
            RunProfile {
                peak_memory: Some(90 << 20),
                timeout_ms: Some(1_800_000),
                cores: Some(10),
                dbrover: Some("0.31.0".to_owned()),
            }
        );
    }

    #[test]
    fn keeps_the_version_the_runs_start_named() {
        let version = BlobId::of(b"[report]\n");
        let mut record = RunRecord::new(TypedValues::new());
        untimed(
            &mut record,
            RunEvent::Started {
                report: "Values".to_owned(),
                sources: Vec::new(),
                combine: false,
                combine_sql: None,
                version: Some(version),
                combine_limits: None,
                timeout_ms: None,
                machine: None,
            },
        );
        untimed(&mut record, RunEvent::Finished { elapsed_ms: 1 });

        let report = ReportName::new("values").unwrap();
        let run = record.to_report_run(&report, RunOrigin::Command).unwrap();
        assert_eq!(run.version, Some(version));
    }

    #[test]
    fn follows_each_step_of_a_combined_run() {
        let source = |id: &str, attached| SourceStart {
            id: id.to_owned(),
            data_source: "ledger".to_owned(),
            attached,
            waits_for: None,
            engine: None,
            limits: None,
        };
        let mut record = RunRecord::new(TypedValues::new());
        untimed(
            &mut record,
            RunEvent::Started {
                report: "Valuation".to_owned(),
                sources: vec![source("tx", false), source("prices", true)],
                combine: true,
                combine_sql: None,
                version: None,
                combine_limits: None,
                timeout_ms: None,
                machine: None,
            },
        );
        let states = |record: &RunRecord| -> Vec<StepState> {
            record.steps.iter().map(|step| step.state).collect()
        };
        assert_eq!(states(&record), [StepState::Waiting; 3]);
        assert!(record.combines());

        untimed(
            &mut record,
            RunEvent::SourceAttached {
                id: "prices".to_owned(),
                path: PathBuf::from("/tmp/prices.duckdb"),
            },
        );
        untimed(
            &mut record,
            RunEvent::Progress {
                step: Step::Source {
                    id: "tx".to_owned(),
                },
                rows: 1_000,
                bytes: 20_000,
                elapsed_ms: 1_000,
                batch: None,
                busy: None,
            },
        );
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
        untimed(
            &mut record,
            RunEvent::SourceDone {
                id: "tx".to_owned(),
                rows: 1_500,
                bytes: 30_000,
                first_row_ms: Some(10),
                elapsed_ms: 1_200,
                table: vec![LoadedColumn {
                    name: "quantity".to_owned(),
                    type_name: "DECIMAL(21,18)".to_owned(),
                    declared: false,
                    bytes: None,
                }],
                warnings: vec!["a warning".to_owned()],
                batches: None,
                phases: None,
            },
        );
        assert_eq!(record.rows, 0, "a loaded source's rows aren't the outputs'");
        assert_eq!(record.steps[0].table.len(), 1);
        assert_eq!(record.steps[0].warnings, ["a warning"]);
        untimed(
            &mut record,
            RunEvent::Failed(RunFailure::new(Step::Combine, "no memory")),
        );
        assert_eq!(record.steps[2].state, StepState::Failed);
        assert!(matches!(record.end, Some(RunEnd::Failed(_))));
    }
}
