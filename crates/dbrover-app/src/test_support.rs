//! Helpers for tests that drive the whole app: a real engine, a data source file in a
//! temporary directory, and passwords kept in memory instead of the OS keychain.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dbrover_core::{DataSourceId, Password};
use dbrover_engine::{
    EngineConfig, InstallError, InstallSupport, InstalledUpdate, Invocation, ProgramFailure,
    ProgramOutput, ProgramRunner, Release, ReleaseError, ReleaseFeed, Restart, UpdateInstaller,
};
use dbrover_store::{
    ConsoleFiles, CustomFiles, DataSourceFile, DiagramFiles, HistoryFile, KeymapFile, ProjectFile,
    Secret, SecretError, SecretStore, SettingsFile, TrustedCommandsFile,
};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};

use crate::app::DbRoverApp;
use crate::log_console::LogFeed;

/// How long to wait for engine events before failing. Generous, because the first connection
/// to a database can be slow on a busy CI machine.
const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// How many frames `Harness::run` may take to settle, in place of egui_kittest's 4.
///
/// `run` steps until a frame asks for no immediate repaint. Besides the UI's own repaints,
/// every engine event asks for one (`EguiRepaint`), and keeps the next frame asking too, so
/// `run` lasts up to two frames for each event that lands while it steps. A frame takes about
/// a millisecond, so whether an engine reply lands inside `run` is down to scheduling: on a
/// busy CI runner the replies to a click, or the tail of the startup loads, took `run` past 4
/// frames, and the test failed with "Harness::run exceeded max_steps (4)".
///
/// Counted in events, an action's replies are a handful, and the startup loads a dozen, so 64
/// frames outlast them however they are spread out. It costs a passing test nothing: `run`
/// returns at the first settled frame. A UI that never settles, such as one showing a spinner,
/// still fails within a fraction of a second.
pub const MAX_STEPS: u64 = 64;

/// Passwords in memory, so tests never touch the real keychain.
#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<(DataSourceId, Secret), Password>>);

impl MemorySecrets {
    pub fn with_password(id: DataSourceId, password: Password) -> Self {
        Self(Mutex::new(HashMap::from([(
            (id, Secret::Database),
            password,
        )])))
    }

    pub fn saved(&self, id: DataSourceId, secret: Secret) -> Option<Password> {
        self.0.lock().unwrap().get(&(id, secret)).cloned()
    }
}

impl SecretStore for MemorySecrets {
    fn get(&self, id: DataSourceId, secret: Secret) -> Result<Option<Password>, SecretError> {
        Ok(self.saved(id, secret))
    }

    fn set(&self, id: DataSourceId, secret: Secret, value: &Password) -> Result<(), SecretError> {
        self.0.lock().unwrap().insert((id, secret), value.clone());
        Ok(())
    }

    fn delete(&self, id: DataSourceId, secret: Secret) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(&(id, secret));
        Ok(())
    }
}

/// A release feed that never touches the network: it reports this release, or none.
pub struct FixedReleases(pub Option<Release>);

impl FixedReleases {
    /// The running version as the newest release, so the app finds itself up to date.
    pub fn current() -> Self {
        Self::tagged(&format!("v{}", crate::version::VERSION))
    }

    pub fn tagged(tag: &str) -> Self {
        Self(Some(Release {
            tag: tag.to_owned(),
            page: format!("https://example.com/releases/{tag}"),
        }))
    }
}

impl ReleaseFeed for FixedReleases {
    fn latest(&self) -> Result<Release, ReleaseError> {
        self.0.clone().ok_or(ReleaseError::NoReleases)
    }
}

/// An installer that never downloads or replaces anything: it can install, reports a
/// download in two steps, and says it installed `version`.
pub struct FakeInstaller {
    pub version: &'static str,
}

impl Default for FakeInstaller {
    fn default() -> Self {
        Self { version: "99.0.0" }
    }
}

impl UpdateInstaller for FakeInstaller {
    fn support(&self) -> InstallSupport {
        InstallSupport::Supported
    }

    fn install(
        &self,
        progress: &dyn Fn(u64, Option<u64>),
    ) -> Result<InstalledUpdate, InstallError> {
        progress(0, Some(100));
        progress(100, Some(100));
        Ok(InstalledUpdate {
            version: self.version.to_owned(),
            restart: Restart::Relaunch,
        })
    }
}

/// Plays the part of command actions' and export programs, so UI tests start no processes; the
/// engine's own tests run real ones. The program's name says what it does:
/// - `echo` writes its stdin back, or for an export, into the file;
/// - `print` writes its arguments, one a line;
/// - `fail` exits with status 3, after writing "it went wrong" to stderr;
/// - `hang` runs until it is stopped.
pub struct ScriptedPrograms;

#[async_trait::async_trait]
impl ProgramRunner for ScriptedPrograms {
    async fn run(&self, invocation: Invocation) -> Result<ProgramOutput, ProgramFailure> {
        let wrote = |stdout: String| {
            Ok(ProgramOutput {
                stdout,
                ..ProgramOutput::default()
            })
        };
        match invocation.program.as_str() {
            "echo" => wrote(invocation.stdin),
            "print" => wrote(invocation.arguments.join("\n")),
            "fail" => Err(ProgramFailure::Exited {
                program: invocation.program,
                status: "exit status: 3".to_owned(),
                stderr: "it went wrong".to_owned(),
            }),
            "hang" => std::future::pending().await,
            other => Err(ProgramFailure::CouldNotStart(format!(
                "{other} is not a scripted program"
            ))),
        }
    }

    fn start_export(
        &self,
        invocation: Invocation,
        out: std::fs::File,
        _: std::time::Duration,
    ) -> std::io::Result<Box<dyn dbrover_export::ExportPipe>> {
        match invocation.program.as_str() {
            "echo" | "fail" => Ok(Box::new(ScriptedPipe {
                out,
                fails: invocation.program == "fail",
            })),
            other => Err(std::io::Error::other(ProgramFailure::CouldNotStart(
                format!("{other} is not a scripted program"),
            ))),
        }
    }
}

/// A scripted export program: `echo` writes its input to the file, `fail` fails at the end.
struct ScriptedPipe {
    out: std::fs::File,
    fails: bool,
}

impl dbrover_export::ExportPipe for ScriptedPipe {
    fn send(&mut self, input: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        self.out.write_all(input)
    }

    fn finish(self: Box<Self>) -> std::io::Result<()> {
        if self.fails {
            return Err(std::io::Error::other(
                "fail failed (exit status: 3): it went wrong",
            ));
        }
        Ok(())
    }
}

/// Starts the app with its data source file in `dir`, and waits for it to finish loading (see
/// [`wait_for_startup`]).
pub fn start_app(
    builder: HarnessBuilder<DbRoverApp>,
    dir: &Path,
    log_feed: LogFeed,
    secrets: MemorySecrets,
) -> Harness<'static, DbRoverApp> {
    let config = EngineConfig {
        data_sources: DataSourceFile::new(dir.join("data_sources.toml")),
        projects: ProjectFile::new(dir.join("projects.toml")),
        secrets: Arc::new(secrets),
        consoles: ConsoleFiles::new(dir.join("consoles")),
        diagrams: DiagramFiles::new(dir.join("diagrams")),
        history: HistoryFile::new(dir.join("history.sqlite")),
        keymap: KeymapFile::new(dir.join("keymap.toml")),
        settings: SettingsFile::new(dir.join("settings.toml")),
        custom_files: CustomFiles::new(dir),
        trusted_commands: TrustedCommandsFile::new(dir.join("trusted_commands.toml")),
        programs: Arc::new(ScriptedPrograms),
        releases: Arc::new(FixedReleases::current()),
        updater: Arc::new(FakeInstaller::default()),
        known_hosts: dir.join("known_hosts"),
        idle_ping_after: dbrover_engine::IDLE_PING_AFTER,
    };
    let mut harness = builder
        .with_max_steps(MAX_STEPS)
        .build_eframe(|cc| DbRoverApp::new(cc, log_feed, config).unwrap());
    wait_for_startup(&mut harness);
    harness
}

/// Steps the app until the engine has reported everything it loads at startup.
///
/// Without this, a test starts clicking as soon as the first thing it waits for shows, such as
/// "Engine running" or a data source, while the rest is still on its way. Some of it changes
/// what the test looks for: a data source's menu has no **Diagrams** item until the saved
/// diagrams arrive. And each late event keeps `Harness::run` stepping (see [`MAX_STEPS`]).
pub fn wait_for_startup(harness: &mut Harness<'_, DbRoverApp>) {
    step_until(harness, "the engine finishing its startup", |harness| {
        harness.state().has_finished_starting()
    });
}

/// Steps the app until `done` holds; engine events arrive asynchronously, so a fixed number of
/// steps would be flaky.
///
/// It steps once before the first check. A click the caller has just queued only lands in the
/// next frame, so until then the tree is the one from before the click, and `done` may hold
/// there and nowhere after. The foreign key dialog test lost that race: picking a referenced
/// table replaces its columns' menu button with "Loading its columns…" until the engine
/// answers, but the stale tree still showed the button, so the test clicked it in the same
/// frame as the table, and the menu stayed shut whenever the engine answered after that frame.
pub fn step_until(
    harness: &mut Harness<'_, DbRoverApp>,
    what: &str,
    done: impl Fn(&Harness<'_, DbRoverApp>) -> bool,
) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    harness.step();
    while !done(harness) {
        assert!(Instant::now() < deadline, "{what} never happened");
        std::thread::sleep(Duration::from_millis(10));
        harness.step();
    }
}

/// Steps the app until at least one widget labelled with something containing `text` appears.
/// It looks for any match, not exactly one: a label can briefly show twice, as a combo box's
/// button and its open popup do, and kittest's single-match queries panic on that.
pub fn step_until_visible(harness: &mut Harness<'_, DbRoverApp>, text: &str) {
    step_until(harness, &format!("{text:?} appearing"), |harness| {
        harness.query_all_by_label_contains(text).next().is_some()
    });
}

/// `Harness::new_ui` with DbRover's fonts, which widgets that draw icons need. egui applies new
/// fonts from the next frame on, so the first frame only installs them.
pub fn ui_harness<'a>(mut app: impl FnMut(&mut egui::Ui) + 'a) -> Harness<'a> {
    let mut fonts_installed = false;
    let mut harness = Harness::new_ui(move |ui| {
        if fonts_installed {
            app(ui);
        } else {
            crate::fonts::install(ui.ctx());
            fonts_installed = true;
        }
    });
    harness.run();
    harness
}
