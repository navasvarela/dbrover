//! Reports in the app (`docs/reporting.md`, RP1f and RP2e): the files in `reports/`, read for the
//! explorer's **Reports**; each report's tab, with its parameters, output folder and last run;
//! and the one run that may be going, in its worker.
//!
//! The app reads and plans a report itself to show what is wrong and to check values as they
//! are typed, but never runs one: the worker reads the file again and runs it, so what runs is
//! what is saved.

mod launcher;
mod tab;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use dbrover_core::DataSource;
use dbrover_engine::{Command, Event, ReportText};
use dbrover_export::CustomFormat;
use dbrover_report::{
    LoadedColumn, Plan, Preview, Report, RunEvent, RunFailure, RunRequest, SourceStart, Step,
    TypedValues, WrittenFile, read_report,
};
use dbrover_store::{ReportFiles, ReportName};

pub use launcher::{ProcessLauncher, ReportLauncher, RunningReport};
pub use tab::{ReportView, show};

/// What a new report starts as: a commented trial balance on the first data source, written
/// as the user will write their own.
const NEW_REPORT: &str = include_str!("new_report.toml");

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
        }
    }

    /// The name in `[report]`, or the file's when it can't be read.
    pub fn title(&self) -> &str {
        match &self.read {
            Ok(report) => &report.name,
            Err(_) => self.name.as_str(),
        }
    }

    /// The report against the data sources and presets as they are now.
    pub fn plan(
        &self,
        data_sources: &[DataSource],
        presets: &[CustomFormat],
    ) -> Result<Plan, Vec<String>> {
        let report = self.read.as_ref().map_err(Clone::clone)?;
        Plan::new(report.clone(), data_sources, presets)
    }
}

/// What a report's tab holds between frames.
#[derive(Default)]
pub struct ReportState {
    pub values: TypedValues,
    /// `None` until one is chosen: the default folder.
    pub folder: Option<PathBuf>,
    pub last_run: Option<RunRecord>,
}

/// One run, as its events have told it so far.
pub struct RunRecord {
    pub started: Instant,
    /// Each source as the run's start listed them, then the combine step if there is one.
    pub steps: Vec<StepRecord>,
    /// The rows the outputs were given: the source's, or the combine step's.
    pub rows: u64,
    pub preview: Option<Preview>,
    pub files: Vec<WrittenFile>,
    pub end: Option<RunEnd>,
}

/// A source or the combine step, as far as the run has gone.
pub struct StepRecord {
    pub step: Step,
    /// The data source a source reads; `None` for the combine step.
    pub data_source: Option<String>,
    pub attached: bool,
    pub state: StepState,
    /// A loaded source's table in DuckDB, its columns and their types.
    pub table: Vec<LoadedColumn>,
    pub warnings: Vec<String>,
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
    fn new() -> Self {
        Self {
            started: Instant::now(),
            steps: Vec::new(),
            rows: 0,
            preview: None,
            files: Vec::new(),
            end: None,
        }
    }

    /// Whether a combine step makes the rows the outputs write.
    pub fn combines(&self) -> bool {
        self.steps.iter().any(|step| step.step == Step::Combine)
    }

    fn step_mut(&mut self, step: &Step) -> Option<&mut StepRecord> {
        self.steps.iter_mut().find(|record| record.step == *step)
    }

    fn apply(&mut self, event: RunEvent) {
        match event {
            RunEvent::Started {
                sources, combine, ..
            } => {
                self.steps = sources.into_iter().map(StepRecord::source).collect();
                if combine {
                    self.steps.push(StepRecord::new(Step::Combine, None, false));
                }
            }
            RunEvent::Progress {
                step,
                rows,
                bytes,
                elapsed_ms,
            } => {
                if let Some(record) = self.step_mut(&step) {
                    record.state = StepState::Running {
                        rows,
                        bytes,
                        elapsed_ms,
                    };
                }
            }
            RunEvent::SourceDone {
                id,
                rows,
                bytes,
                elapsed_ms,
                table,
                warnings,
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
            state: StepState::Waiting,
            table: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn source(source: SourceStart) -> Self {
        Self::new(
            Step::Source { id: source.id },
            Some(source.data_source),
            source.attached,
        )
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

    /// Takes in what the engine read. Returns a report to open, once one just created is
    /// listed, and the command to list them again after one is created.
    pub fn apply(&mut self, event: &Event) -> (Option<ReportName>, Option<Command>) {
        match event {
            Event::ReportsListed(Ok(found)) => {
                let listed: Vec<Listed> = found.iter().map(Listed::new).collect();
                let created = self
                    .to_open
                    .take_if(|name| listed.iter().any(|listed| &listed.name == name));
                self.listing = Listing::Loaded(listed);
                (created, None)
            }
            Event::ReportsListed(Err(error)) => {
                self.listing = Listing::Unavailable(error.clone());
                (None, None)
            }
            Event::ReportCreated(Ok(name)) => {
                self.to_open = Some(name.clone());
                (None, self.reload())
            }
            Event::ReportCreated(Err(error)) => {
                tracing::error!("could not create a report: {error}");
                (None, None)
            }
            _ => (None, None),
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

    /// Starts `name` in a worker with the values typed in its tab, unless a report is running.
    /// `wake` asks the UI to look at the run's events.
    pub fn start(
        &mut self,
        name: &ReportName,
        plan: &Plan,
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
        let request = RunRequest {
            report: listed.path.clone(),
            values: self
                .state(name)
                .map(|state| state.values.clone())
                .unwrap_or_default(),
            staging: folder.join(staging_name()),
            folder,
            memory_limit: None,
        };
        let started = setup.launcher.start(&request, plan.timeout, Box::new(wake));
        let mut record = RunRecord::new();
        match started {
            Ok(run) => {
                self.running = Some(Running {
                    report: name.clone(),
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
        self.state_mut(name).last_run = Some(record);
    }

    pub fn cancel(&mut self) {
        if let Some(running) = &mut self.running {
            running.run.cancel();
        }
    }

    /// Takes in what the running report's worker said since the last frame.
    pub fn poll(&mut self) {
        let Some(running) = &mut self.running else {
            return;
        };
        let mut events = Vec::new();
        while let Some(event) = running.run.try_next() {
            events.push(event);
        }
        let report = running.report.clone();
        let mut ended = false;
        if let Some(record) = self.state_mut(&report).last_run.as_mut() {
            for event in events {
                record.apply(event);
            }
            ended = record.end.is_some();
        }
        if ended {
            self.running = None;
        }
    }
}

/// A staging folder's name no other run of this app, or of another DbRover, has: hidden, and
/// named after the process and the time, which is all one run needs to be told apart.
fn staging_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!(".dbrover-report-{}-{nanos}", std::process::id())
}

/// Opens a file or folder with the system's own app, as a `file://` address.
pub fn open_path(ctx: &egui::Context, path: &Path) {
    ctx.open_url(egui::OpenUrl::new_tab(file_url(path)));
}

/// `path` as a `file://` address, with what an address can't hold percent-encoded:
/// `/Users/me/My Reports` as `file:///Users/me/My%20Reports`, `C:\Reports` as
/// `file:///C:/Reports`.
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from("file://");
    if !text.starts_with('/') {
        url.push('/');
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                url.push(char::from(byte));
            }
            other => url.push_str(&format!("%{other:02X}")),
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_paths_as_file_addresses() {
        let cases = [
            ("/Users/me/My Reports", "file:///Users/me/My%20Reports"),
            ("/tmp/a#b%c.csv", "file:///tmp/a%23b%25c.csv"),
            ("C:\\Reports\\tb.xlsx", "file:///C:/Reports/tb.xlsx"),
        ];
        for (path, url) in cases {
            assert_eq!(file_url(Path::new(path)), url, "{path}");
        }
    }

    #[test]
    fn follows_each_step_of_a_combined_run() {
        let source = |id: &str, attached| SourceStart {
            id: id.to_owned(),
            data_source: "ledger".to_owned(),
            attached,
        };
        let mut record = RunRecord::new();
        record.apply(RunEvent::Started {
            report: "Valuation".to_owned(),
            sources: vec![source("tx", false), source("prices", true)],
            combine: true,
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
            }],
            warnings: vec!["a warning".to_owned()],
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
