use std::any::Any;
use std::panic::Location;
use std::path::Path;

use anyhow::Context as _;
use tracing::Level;
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Layer as _, fmt};

use crate::log_console::{self, LogFeed};

/// Daily log files kept before the oldest is deleted.
const MAX_LOG_FILES: usize = 7;

const APP_LOG_PREFIX: &str = "dbrover";
/// Its own files, so a server running beside the app never interleaves lines with it.
const MCP_LOG_PREFIX: &str = "dbrover-mcp";

pub struct Telemetry {
    pub log_feed: LogFeed,
    /// Flushes the file log when dropped, so it must live until the app exits.
    /// `None` when file logging is unavailable; the reason is in the log console.
    pub file_guard: Option<WorkerGuard>,
}

/// Installs the global `tracing` subscriber: everything goes to the log console and, when the
/// logs directory is usable, to a daily rolling file. Records from the `log` crate (used by
/// eframe and winit) are bridged in too.
///
/// Must be called on the thread that will run the UI (see `log_console::pipeline`).
pub fn init(logs_dir: &Path) -> anyhow::Result<Telemetry> {
    let (console_layer, log_feed) = log_console::pipeline();
    let (file_writer, file_guard) = match open_log_files(logs_dir, APP_LOG_PREFIX) {
        Ok((writer, guard)) => (Some(writer), Ok(guard)),
        Err(error) => (None, Err(error)),
    };
    let file_layer = file_writer.map(|writer| {
        fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_filter(targets())
    });

    tracing_subscriber::registry()
        .with(console_layer.with_filter(targets()))
        .with(file_layer)
        .try_init()
        .context("could not install the tracing subscriber")?;
    log_panics();

    let file_guard = file_guard
        .inspect_err(|error| tracing::warn!("file logging disabled: {error:#}"))
        .ok();
    Ok(Telemetry {
        log_feed,
        file_guard,
    })
}

/// Installs the global `tracing` subscriber for `dbrover mcp`, which has no window: everything
/// goes to standard error, which MCP clients keep in their own logs, and to daily
/// `dbrover-mcp` files beside the app's. Never to standard output, which carries the protocol.
///
/// Returns the guard that flushes the file log, which must live until the server stops, or
/// `None` when file logging is unavailable (the reason is logged).
pub fn init_headless(logs_dir: &Path) -> anyhow::Result<Option<WorkerGuard>> {
    let (file_writer, file_guard) = match open_log_files(logs_dir, MCP_LOG_PREFIX) {
        Ok((writer, guard)) => (Some(writer), Ok(guard)),
        Err(error) => (None, Err(error)),
    };
    let file_layer = file_writer.map(|writer| {
        fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_filter(targets())
    });
    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_filter(targets());
    tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_layer)
        .try_init()
        .context("could not install the tracing subscriber")?;
    log_panics();
    Ok(file_guard
        .inspect_err(|error| tracing::warn!("file logging disabled: {error:#}"))
        .ok())
}

/// Logs every panic as an error, then lets the default hook print it as before.
///
/// An app started from Finder or the Dock has nowhere to print to, so without this a crash
/// left no message behind: on 2026-10-23 a crash after a query left only winit's "no handler
/// was set" in the log. The file log is still written, because unwinding from the panic
/// drops `Telemetry::file_guard`, which flushes it.
fn log_panics() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let report = panic_report(
            info.payload(),
            info.location(),
            std::thread::current().name(),
        );
        tracing::error!(target: "dbrover::panic", "{report}");
        default_hook(info);
    }));
}

/// `panicked on thread 'main' at src/grid.rs:12:5: <message>`, as close to Rust's own
/// wording as the parts allow.
/// The text a panic was raised with, which is a `&str` or a `String` unless the code panicked
/// with some other value.
pub fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(a panic without a message)")
}

fn panic_report(
    payload: &(dyn Any + Send),
    location: Option<&Location<'_>>,
    thread: Option<&str>,
) -> String {
    let message = panic_message(payload);
    let thread = thread.unwrap_or("<unnamed>");
    match location {
        Some(location) => format!("panicked on thread '{thread}' at {location}: {message}"),
        None => format!("panicked on thread '{thread}': {message}"),
    }
}

/// DbRover's own crates log from debug up. Dependencies only from warn up: at info they are
/// chatty about things a database tool's user doesn't care about (GL setup, window events).
pub fn targets() -> Targets {
    Targets::new()
        .with_target("dbrover", Level::DEBUG)
        .with_default(Level::WARN)
}

fn open_log_files(logs_dir: &Path, prefix: &str) -> anyhow::Result<(NonBlocking, WorkerGuard)> {
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(prefix)
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(logs_dir)
        .with_context(|| format!("could not open log files in {}", logs_dir.display()))?;
    Ok(tracing_appender::non_blocking(appender))
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use super::*;

    struct ReportCase {
        payload: Box<dyn Any + Send>,
        location: Option<&'static Location<'static>>,
        thread: Option<&'static str>,
        expected: String,
    }

    #[test]
    fn reports_the_message_location_and_thread() {
        let location = Location::caller();
        let cases = [
            ReportCase {
                payload: Box::new("boom"),
                location: Some(location),
                thread: Some("main"),
                expected: format!("panicked on thread 'main' at {location}: boom"),
            },
            ReportCase {
                payload: Box::new(String::from("index 3 out of range")),
                location: Some(location),
                thread: None,
                expected: format!(
                    "panicked on thread '<unnamed>' at {location}: index 3 out of range"
                ),
            },
            ReportCase {
                payload: Box::new(42_u32),
                location: None,
                thread: Some("engine"),
                expected: "panicked on thread 'engine': (a panic without a message)".to_owned(),
            },
        ];
        for case in cases {
            let report = panic_report(case.payload.as_ref(), case.location, case.thread);
            assert_eq!(report, case.expected);
        }
    }

    /// Collects what the subscriber writes, to read it back.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| io::Error::other("poisoned"))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_panic_is_logged_as_an_error_before_unwinding() {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_max_level(Level::ERROR)
            .finish();

        // The hook is process-wide and stays installed. It still runs the default hook, so
        // other tests' panics look the same as before; they log nothing, having no subscriber.
        log_panics();
        let unwound = tracing::subscriber::with_default(subscriber, || {
            std::panic::catch_unwind(|| panic!("the grid lost its order"))
        });

        assert!(unwound.is_err());
        let log = captured
            .0
            .lock()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        assert!(log.contains("ERROR"), "{log}");
        assert!(log.contains("dbrover::panic"), "{log}");
        assert!(log.contains("the grid lost its order"), "{log}");
        assert!(log.contains("telemetry.rs"), "{log}");
    }
}
