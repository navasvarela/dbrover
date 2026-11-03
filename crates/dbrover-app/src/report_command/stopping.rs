//! Ctrl-C, SIGTERM and SIGHUP cancel `dbrover report run`'s run (`docs/reporting.md`,
//! "Running it"): the worker is killed and its staging folder removed by `WorkerRun`, as
//! **Cancel** does in the app, so the output folder is left as it was and the command exits
//! with the code for a run stopped from outside.

use std::sync::{Arc, Mutex, PoisonError};

use dbrover_engine::{RunStopper, StopSignals};

/// Where the run is, as a stop signal finds it.
enum State {
    /// The worker hasn't started yet.
    NotStarted,
    Running(RunStopper),
    /// A stop came before the worker started: it is cancelled as soon as it does.
    StopRequested,
}

/// Listens for the stop signals from the moment it is made until the process exits.
pub struct Stopping {
    state: Arc<Mutex<State>>,
}

impl Stopping {
    /// Starts listening. If the signals can't be listened for, they end the process as they
    /// would without this, and the run's worker is stopped by its own time limit only.
    pub fn listen() -> Self {
        let state = Arc::new(Mutex::new(State::NotStarted));
        let on_stop = Arc::clone(&state);
        let listening =
            StopSignals::listen().and_then(|signals| signals.on_signal(move || stop(&on_stop)));
        if let Err(error) = listening {
            tracing::warn!("Ctrl-C and SIGTERM won't cancel the run: {error}");
        }
        Self { state }
    }

    /// The run's worker has started: a stop cancels it from now on, and one that came before
    /// cancels it now.
    pub fn started(&self, stopper: RunStopper) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(*state, State::StopRequested) {
            stopper.cancel();
        }
        *state = State::Running(stopper);
    }
}

fn stop(state: &Mutex<State>) {
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    match &*state {
        State::Running(stopper) => stopper.cancel(),
        State::NotStarted => *state = State::StopRequested,
        State::StopRequested => {}
    }
}
