//! How a report's run starts: a worker process in the app, a stand-in in UI tests.

use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use dbrover_engine::WorkerRun;
use dbrover_report::{RunEvent, RunRequest};

use crate::version::REPORT_WORKER;

/// Starts runs.
pub trait ReportLauncher {
    /// Starts `request`, which may take up to `timeout`; `wake` is called whenever an event
    /// comes.
    ///
    /// # Errors
    ///
    /// If the run can't be started at all.
    fn start(
        &self,
        request: &RunRequest,
        timeout: Duration,
        wake: Box<dyn Fn() + Send + Sync>,
    ) -> io::Result<Box<dyn RunningReport>>;
}

/// A run going on.
pub trait RunningReport {
    /// The next event, if one has come; the last one ends the run.
    fn try_next(&mut self) -> Option<RunEvent>;
    fn cancel(&mut self);
}

/// Runs reports in `dbrover report-worker`, started from the app's own executable with the
/// app's settings folder, so the worker reads the files the app does.
pub struct ProcessLauncher {
    pub executable: PathBuf,
    pub settings_folder: PathBuf,
}

impl ProcessLauncher {
    /// `dbrover report-worker` on the settings folder, ready for [`WorkerRun::start`].
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .arg(REPORT_WORKER)
            .arg(dbrover_store::CONFIG_DIR_FLAG)
            .arg(&self.settings_folder);
        command
    }
}

impl ReportLauncher for ProcessLauncher {
    fn start(
        &self,
        request: &RunRequest,
        timeout: Duration,
        wake: Box<dyn Fn() + Send + Sync>,
    ) -> io::Result<Box<dyn RunningReport>> {
        let run = WorkerRun::start(self.command(), request, timeout, wake)?;
        Ok(Box::new(Worker(run)))
    }
}

struct Worker(WorkerRun);

impl RunningReport for Worker {
    fn try_next(&mut self) -> Option<RunEvent> {
        self.0.try_next()
    }

    fn cancel(&mut self) {
        self.0.cancel();
    }
}
