//! The worker's limits from the operating system (`docs/reporting.md`, "Guardrails in RP1",
//! layer 4): a lower priority everywhere, and on Linux a cap on its memory and its processor
//! time. Each is best effort: a limit the system refuses is logged, and the run goes on under
//! the others.
//!
//! Windows gets its lower priority from the app, which starts the worker below normal
//! (`dbrover_engine::WorkerRun`). macOS doesn't enforce memory limits on a process reliably,
//! so there, as on Windows, the source's and outputs' caps and the run's DuckDB's
//! `memory_limit` bound a run.

use std::time::Duration;

/// The priority the worker runs at on Linux and macOS: `nice 10`, halfway to the lowest, so
/// the app and whatever else the user runs stay responsive while a report reads.
#[cfg(unix)]
const NICENESS: i32 = 10;

/// Asks Linux to kill the worker when the process that started it dies, so a command or an app
/// killed outright (SIGKILL, a CI runner's hard stop) doesn't leave its worker running. A parent
/// that died before the request was made gets no signal sent, so the worker checks that its
/// parent is still the one `parent` names, and exits if not. Elsewhere, the worker stops once
/// its events can't be written (`report_worker::run`).
pub fn die_with_parent(parent: Option<&str>) {
    #[cfg(target_os = "linux")]
    {
        use rustix::process::{Signal, getppid, set_parent_process_death_signal};
        if let Err(error) = set_parent_process_death_signal(Some(Signal::KILL)) {
            tracing::warn!("could not ask to die with the worker's parent: {error}");
            return;
        }
        let expected = parent.and_then(|parent| parent.parse::<i32>().ok());
        let actual = getppid().map(|pid| pid.as_raw_nonzero().get());
        if let Some(expected) = expected
            && actual != Some(expected)
        {
            tracing::warn!("the process that started the worker is gone, so it stops");
            std::process::exit(super::FAILED);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = parent;
}

pub fn lower_priority() {
    #[cfg(unix)]
    if let Err(error) = rustix::process::setpriority_process(None, NICENESS) {
        tracing::warn!("could not lower the worker's priority: {error}");
    }
}

/// Caps the memory the worker can write to: `limit` bytes, or half the machine's.
///
/// `RLIMIT_DATA` rather than `RLIMIT_AS`: the address space counts what tokio's thread stacks
/// and the allocator reserve without using, which is far more than a run writes to. Past the
/// limit an allocation fails, and the worker aborts; the app's side says it ran out of memory.
pub fn limit_memory(limit: Option<u64>) {
    #[cfg(target_os = "linux")]
    {
        use rustix::process::Resource;
        let Some(limit) = limit.or_else(|| half_of_memory().ok()) else {
            tracing::warn!("could not read the machine's memory, so the worker's isn't capped");
            return;
        };
        set_soft_limit(Resource::Data, limit, "memory");
    }
    #[cfg(not(target_os = "linux"))]
    let _ = limit;
}

/// Caps the processor time the worker can use at `limit`, the run's time limit on each of
/// its threads ([`dbrover_report::Plan::processor_time`]): a backstop for a run whose threads
/// keep the processor busy past it, which the app's own timeout also ends.
pub fn limit_processor_time(limit: Duration) {
    #[cfg(target_os = "linux")]
    set_soft_limit(
        rustix::process::Resource::Cpu,
        limit.as_secs().max(1),
        "processor time",
    );
    #[cfg(not(target_os = "linux"))]
    let _ = limit;
}

/// Lowers the soft limit of `resource` to `value`, keeping the hard limit, so nothing the
/// worker inherited is raised.
#[cfg(target_os = "linux")]
fn set_soft_limit(resource: rustix::process::Resource, value: u64, what: &str) {
    use rustix::process::{Rlimit, getrlimit, setrlimit};
    let current = getrlimit(resource);
    let value = match (current.current, current.maximum) {
        (Some(soft), _) if soft < value => soft,
        (_, Some(hard)) if hard < value => hard,
        _ => value,
    };
    let limited = setrlimit(
        resource,
        Rlimit {
            current: Some(value),
            maximum: current.maximum,
        },
    );
    match limited {
        Ok(()) => tracing::info!("the worker's {what} is limited to {value}"),
        Err(error) => tracing::warn!("could not limit the worker's {what}: {error}"),
    }
}

/// Half of `MemTotal` in `/proc/meminfo`, in bytes.
#[cfg(target_os = "linux")]
fn half_of_memory() -> std::io::Result<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo")?;
    meminfo
        .lines()
        .find_map(|line| {
            let kilobytes = line.strip_prefix("MemTotal:")?.trim().strip_suffix("kB")?;
            kilobytes.trim().parse::<u64>().ok()
        })
        .map(|kilobytes| kilobytes * 1024 / 2)
        .ok_or_else(|| std::io::Error::other("/proc/meminfo has no MemTotal"))
}
