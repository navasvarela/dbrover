//! How a report's run starts: a worker process in the app, a stand-in in UI tests.

use dbrover_engine::{RunWatch, WorkerRun};
use dbrover_report::{RunRequest, TimedEvent};
use std::io;
use std::path::PathBuf;
use std::process::Command;

use crate::version::REPORT_WORKER;

/// Starts runs.
pub trait ReportLauncher {
    /// Starts `request`, watched for `watch`'s limits; `wake` is called whenever an event
    /// comes.
    ///
    /// # Errors
    ///
    /// If the run can't be started at all.
    fn start(
        &self,
        request: &RunRequest,
        watch: RunWatch,
        wake: Box<dyn Fn() + Send + Sync>,
    ) -> io::Result<Box<dyn RunningReport>>;
}

/// A run going on.
pub trait RunningReport {
    /// The next event, if one has come; the last one ends the run.
    fn try_next(&mut self) -> Option<TimedEvent>;
    fn cancel(&mut self);
    /// Stops the run as [`RunningReport::cancel`] does, keeping what it finished to resume.
    fn pause(&mut self);
}

/// Runs reports in `dbrover report-worker`, started from the app's own executable with the
/// app's settings folder, so the worker reads the files the app does.
pub struct ProcessLauncher {
    pub executable: PathBuf,
    pub settings_folder: PathBuf,
}

impl ProcessLauncher {
    /// `dbrover report-worker` on the settings folder, ready for [`WorkerRun::start`].
    ///
    /// The worker is told this process's id, so on Linux it can die with it (see
    /// `report_worker::limits::die_with_parent`). Linux sends that signal when the *thread*
    /// that started the worker ends, not the process: both callers start it from their main
    /// thread, the app's UI thread and the command's, which live as long as the run.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .arg(REPORT_WORKER)
            .arg(dbrover_store::CONFIG_DIR_FLAG)
            .arg(&self.settings_folder)
            .env(PARENT_ENV, std::process::id().to_string());
        command
    }
}

/// The variable that tells the worker the id of the process that started it.
pub const PARENT_ENV: &str = "DBROVER_REPORT_PARENT";

impl ReportLauncher for ProcessLauncher {
    fn start(
        &self,
        request: &RunRequest,
        watch: RunWatch,
        wake: Box<dyn Fn() + Send + Sync>,
    ) -> io::Result<Box<dyn RunningReport>> {
        let run = WorkerRun::start(self.command(), request, watch, wake)?;
        Ok(Box::new(Worker(run)))
    }
}

struct Worker(WorkerRun);

impl RunningReport for Worker {
    fn try_next(&mut self) -> Option<TimedEvent> {
        self.0.try_next()
    }

    fn cancel(&mut self) {
        self.0.cancel();
    }

    fn pause(&mut self) {
        self.0.pause();
    }
}
