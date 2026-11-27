//! Saved dumps and their schedules (W17h, `docs/dump-restore.md`): the engine keeps
//! `dumps.toml` and the data folder's `dump_schedules.toml`, checks every minute while
//! DbRover is open which schedules are due, and runs a saved dump as a dump of the Dump dialog
//! runs, then records it and removes the files past its `keep`. `dbrover dump run` runs one
//! the same way, without the app.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dbrover_core::{DataSource, DumpEngine, ServerInfo, describe_error};
use dbrover_store::{
    DUMP_SCHEDULES_FILE_NAME, SAVED_DUMPS_FILE_NAME, SavedDump, SavedDumpsFile, ScheduleState,
    ScheduleStatesFile,
};
use jiff::{Timestamp, Zoned};
use tokio::sync::oneshot;

use crate::Event;
use crate::dump_programs::ProgramPaths;
use crate::dumps::{self, DumpDone, DumpFailure, DumpRun, DumpRunId};
use crate::open::{Connector, TypedSecrets, open};
use crate::passwords::Prompting;
use crate::program_run::{ProgramServices, remove_any};

/// How often the engine looks for a due schedule, and how long after start it first looks,
/// so a run missed while DbRover was closed starts a minute after it opens rather than in the
/// middle of its start-up. A schedule is minutes at its finest.
pub(crate) const SCHEDULE_CHECK: Duration = Duration::from_secs(60);

/// The saved dumps and what this machine knows of their schedules, shared by the engine and
/// the runs it starts.
pub(crate) type SharedSavedDumps = Arc<Mutex<SavedDumps>>;

pub(crate) struct SavedDumps {
    file: SavedDumpsFile,
    states_file: ScheduleStatesFile,
    dumps: Vec<SavedDump>,
    states: Vec<ScheduleState>,
}

impl SavedDumps {
    /// `dumps.toml` in the settings folder and `dump_schedules.toml` in the data folder. A file
    /// that can't be read leaves no saved dumps, or no schedule on, and says why in the log.
    pub(crate) fn load(settings_folder: &Path, data_folder: &Path) -> Self {
        let file = SavedDumpsFile::new(settings_folder.join(SAVED_DUMPS_FILE_NAME));
        let states_file = ScheduleStatesFile::new(data_folder.join(DUMP_SCHEDULES_FILE_NAME));
        let dumps = file.load().unwrap_or_else(|error| {
            tracing::error!("{}", describe_error(&error));
            Vec::new()
        });
        let states = states_file.load().unwrap_or_else(|error| {
            tracing::error!("{}; no dump schedule runs", describe_error(&error));
            Vec::new()
        });
        Self {
            file,
            states_file,
            dumps,
            states,
        }
    }

    pub(crate) fn event(&self) -> Event {
        Event::SavedDumpsLoaded {
            dumps: self.dumps.clone(),
            states: self.states.clone(),
        }
    }

    pub(crate) fn find(&self, id_or_name: &str) -> Option<&SavedDump> {
        self.dumps
            .iter()
            .find(|dump| dump.id == id_or_name)
            .or_else(|| self.dumps.iter().find(|dump| dump.name == id_or_name))
    }

    /// Saves `dump`, in place of one with its id.
    pub(crate) fn save(&mut self, dump: SavedDump) -> Result<(), String> {
        match self.dumps.iter_mut().find(|saved| saved.id == dump.id) {
            Some(saved) => *saved = dump,
            None => self.dumps.push(dump),
        }
        self.file
            .save(&self.dumps)
            .map_err(|error| describe_error(&error))
    }

    pub(crate) fn delete(&mut self, id: &str) -> Result<(), String> {
        self.dumps.retain(|dump| dump.id != id);
        self.states.retain(|state| state.id != id);
        self.file
            .save(&self.dumps)
            .map_err(|error| describe_error(&error))?;
        self.save_states()
    }

    /// Turns a schedule on or off on this machine. Turned on, its first run is the first
    /// moment it falls on after now.
    pub(crate) fn set_on(&mut self, id: &str, on: bool, now: Timestamp) -> Result<(), String> {
        let state = self.state_mut(id, now);
        if on && !state.on {
            state.since = now;
        }
        state.on = on;
        self.save_states()
    }

    /// The saved dumps whose schedule is due at `now` here.
    pub(crate) fn due(&self, now: &Zoned) -> Vec<SavedDump> {
        self.dumps
            .iter()
            .filter(|dump| {
                self.states
                    .iter()
                    .any(|state| state.id == dump.id && state.is_due(dump, now))
            })
            .cloned()
            .collect()
    }

    /// Records a run of `id` and returns the files past its `keep` to remove.
    pub(crate) fn record(
        &mut self,
        id: &str,
        at: Timestamp,
        result: Result<PathBuf, String>,
    ) -> Vec<PathBuf> {
        let keep = self
            .find(id)
            .map_or(dbrover_store::DEFAULT_KEEP, |dump| dump.keep);
        let old = self.state_mut(id, at).record(at, result, keep);
        if let Err(error) = self.save_states() {
            tracing::error!("{error}");
        }
        old
    }

    fn state_mut(&mut self, id: &str, now: Timestamp) -> &mut ScheduleState {
        let index = match self.states.iter().position(|state| state.id == id) {
            Some(index) => index,
            None => {
                self.states.push(ScheduleState::new(id, now));
                self.states.len() - 1
            }
        };
        &mut self.states[index]
    }

    fn save_states(&self) -> Result<(), String> {
        self.states_file
            .save(&self.states)
            .map_err(|error| describe_error(&error))
    }
}

/// Runs `dump` once, as the Dump dialog's dumps run: its server's version read on a connection
/// of its own first, which a program's choice needs. Records the run and removes the files past
/// its `keep`.
pub(crate) async fn run(
    dump: SavedDump,
    data_source: DataSource,
    run: DumpRunId,
    programs: ProgramPaths,
    services: ProgramServices,
    book: SharedSavedDumps,
    cancelled: oneshot::Receiver<()>,
) -> Result<DumpDone, DumpFailure> {
    services.events.send(Event::SavedDumpStarted {
        run,
        name: dump.name.clone(),
    });
    let result = match server_info(&data_source, &services).await {
        Ok(server) => {
            let request = dump.request(&Zoned::now());
            let run = DumpRun {
                run,
                request,
                server,
                programs,
            };
            dumps::dump_and_report(run, data_source, &services, cancelled).await
        }
        Err(failure) => {
            services.events.send(Event::DumpEnded {
                run,
                result: Err(failure.clone()),
            });
            Err(failure)
        }
    };
    let recorded = result
        .as_ref()
        .map(|done| done.file.clone())
        .map_err(ToString::to_string);
    let old = book.lock().unwrap_or_else(PoisonError::into_inner).record(
        &dump.id,
        Timestamp::now(),
        recorded,
    );
    for file in old {
        match remove_any(&file) {
            Ok(()) => tracing::info!("removed {}, past {}'s keep", file.display(), dump.name),
            Err(error) => tracing::warn!("could not remove {}: {error}", file.display()),
        }
    }
    let event = book.lock().unwrap_or_else(PoisonError::into_inner).event();
    services.events.send(event);
    result
}

/// What the data source's server reports itself as. SQLite and DuckDB need no connection: the
/// engine dumps them itself, whatever their version.
async fn server_info(
    data_source: &DataSource,
    services: &ProgramServices,
) -> Result<ServerInfo, DumpFailure> {
    if matches!(
        DumpEngine::of(&data_source.config, &ServerInfo::new("", "")),
        Some(DumpEngine::Sqlite | DumpEngine::DuckDb)
    ) {
        return Ok(ServerInfo::new(
            data_source.config.driver().display_name(),
            "",
        ));
    }
    let connector = Connector {
        secrets: &services.secrets,
        tunnels: &services.tunnels,
        commands: &services.commands,
        prompting: Prompting::Ask(&services.passwords),
    };
    let connection = open(data_source, &TypedSecrets::default(), connector)
        .await
        .map_err(|error| DumpFailure::Connect(describe_error(&error)))?;
    let server = connection.server_info().clone();
    if let Err(error) = connection.close().await {
        tracing::debug!("closing the connection the dump's version was read on: {error}");
    }
    Ok(server)
}

/// What `dbrover dump run` needs of the settings and data folders, without the app.
pub struct HeadlessDump {
    pub data_sources: dbrover_store::DataSourceFile,
    pub settings: dbrover_store::SettingsFile,
    pub trusted_commands: dbrover_store::TrustedCommandsFile,
    pub known_hosts: PathBuf,
    pub history: dbrover_store::HistoryFile,
    pub secrets: Arc<dyn dbrover_store::SecretStore>,
}

/// Runs the saved dump named `name` (its id or its name) once, as its schedule would in the
/// app: recorded in the history and in this machine's schedule state, the files past its
/// `keep` removed. A password the keychain doesn't hold can't be asked for, and fails the run.
///
/// # Errors
///
/// Why the dump didn't run or didn't end well, as a sentence.
pub async fn run_saved_dump(name: &str, headless: HeadlessDump) -> Result<DumpDone, String> {
    let settings_folder = headless
        .settings
        .path()
        .parent()
        .unwrap_or(Path::new("."))
        .to_owned();
    let data_folder = headless
        .trusted_commands
        .path()
        .parent()
        .unwrap_or(Path::new("."))
        .to_owned();
    let book = SavedDumps::load(&settings_folder, &data_folder);
    let dump = book
        .find(name)
        .cloned()
        .ok_or_else(|| format!("There is no saved dump named \u{201c}{name}\u{201d}."))?;
    let data_source = headless
        .data_sources
        .load()
        .map_err(|error| describe_error(&error))?
        .into_iter()
        .find(|data_source| data_source.id == dump.data_source)
        .ok_or_else(|| format!("{}'s data source no longer exists.", dump.name))?;
    let programs = headless
        .settings
        .load()
        .map(|settings| ProgramPaths::from_settings(&settings.dumps))
        .unwrap_or_default();
    let trusted = crate::programs::load_trusted(&headless.trusted_commands).await;
    let (_trust, trusted) = tokio::sync::watch::channel(trusted);
    // Nobody reads the events: the command prints how the run ended itself.
    let (sender, _events) = tokio::sync::mpsc::unbounded_channel();
    let events = crate::events::EventSink::new(sender, NoRepaint);
    let (history, history_task) =
        crate::history::HistoryLog::start(headless.history, events.clone());
    let runner: Arc<dyn crate::ProgramRunner> = Arc::new(crate::ProcessRunner);
    let services = ProgramServices {
        secrets: headless.secrets,
        passwords: crate::passwords::SessionPasswords::default(),
        tunnels: crate::Tunnels::new(headless.known_hosts),
        commands: crate::PasswordCommands::recorded(trusted.clone(), runner),
        events,
        history,
        versions: crate::dump_programs::ProgramVersions::default(),
        trusted,
        run_files: crate::program_run::run_files_folder(&headless.trusted_commands),
    };
    // Never sent: the command runs to the end, and a signal stops the whole process.
    let (_cancel, cancelled) = oneshot::channel();
    let result = run(
        dump,
        data_source,
        DumpRunId(1),
        programs,
        services,
        Arc::new(Mutex::new(book)),
        cancelled,
    )
    .await;
    // The history thread ends once every handle to it has gone, having written the run.
    if let Err(error) = history_task.await {
        tracing::error!("the history thread failed: {error}");
    }
    result.map_err(|failure| failure.to_string())
}

/// No window to repaint.
struct NoRepaint;

impl crate::Repaint for NoRepaint {
    fn request_repaint(&self) {}
}
