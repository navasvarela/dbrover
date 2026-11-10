//! RS3's exit test (`docs/reports-at-scale.md`, "Milestones" and "RS3's exit test"), run by
//! hand; a debug build takes about 11 minutes at the default size, `--release` less:
//!
//! ```text
//! cargo test -p dbrover-app --test report_at_scale -- --ignored --nocapture
//! ```
//!
//! The real worker, as the app starts it, loads two sources of generated rows (50 million each
//! unless `DBROVER_RS3_ROWS` says), joins and sorts them past the combine step's
//! `memory_limit`, and writes the result as Parquet with DuckDB's `COPY`. The run must stay
//! inside `max_disk`, which the app's watchdog enforces and this test samples, and the worker's
//! peak resident memory within the bound RS3c's queues make computable.
//!
//! Everything is written in a temporary folder (`TMPDIR`), removed when the test ends; at the
//! default size it needs about 10 GB free while it runs.
//!
//! Unix only, as `report_worker.rs`: the home folder is moved by setting `HOME`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase, SafetyLevel,
};
use dbrover_engine::{RunWatch, WorkerRun, disk_budget};
use dbrover_report::{ByteSize, LimitDefaults, QUEUE_PAGES, RunEvent, RunRequest};
use dbrover_store::DataSourceFile;

const BINARY: &str = env!("CARGO_BIN_EXE_dbrover");

/// Rows each source generates, unless `DBROVER_RS3_ROWS` says.
const ROWS: u64 = 50_000_000;

const MEMORY_LIMIT: u64 = 1 << 30;
const MAX_DISK: u64 = 40 << 30;

/// Rows a source's page holds (`PAGE_ROWS` in the engine's `report_source.rs`).
const PAGE_ROWS: u64 = 1_000;

/// How far DuckDB 1.5.6's buffer pool passes `memory_limit` at its peak: it allocates before it
/// writes pages out, while a source loads. Measured on a workspace on a file loading 50 million
/// rows: 0.95 GB under a limit of 512 MiB, 2.0 GiB under 1 GiB, 1.2 GiB under 2 GiB, by its own
/// measure of what it allocated, while its use between statements stays at the limit (`docs/reports-at-scale.md`, "RS3 against today's
/// code").
const DUCKDB_OVERSHOOT: u64 = 2;

/// What a value of a page costs in memory beside its text: its `Option<String>`.
const VALUE_OVERHEAD: u64 = 24;

/// One source's query: a few columns of the kinds reports carry, as RS1g's benchmark has them.
fn source_query(rows: u64, salt: u64) -> String {
    format!(
        "SELECT range AS id, \
                CAST((range * {salt}) % 1000000 / 100 AS DECIMAL(18,2)) AS amount, \
                TIMESTAMP '2026-01-01' + to_seconds(range % 31536000) AS ts, \
                'label ' || (range % 1000) AS label \
         FROM range({rows})"
    )
}

fn report(rows: u64) -> String {
    let source = |id: &str, salt: u64| {
        format!(
            "[[source]]\nid = \"{id}\"\ndata_source = \"generated\"\nquery = \"{}\"\n\
             max_rows = {}\nmax_bytes = \"50 GiB\"\ntimeout = \"2h\"\n",
            source_query(rows, salt),
            rows + 1
        )
    };
    format!(
        "[report]\nname = \"RS3 exit\"\ntimeout = \"4h\"\nmax_disk = \"{}\"\n\n{}\n{}\n\
         [combine]\nquery = \"\"\"\n\
         SELECT a.id, a.amount + b.amount AS total, a.ts, b.label\n\
         FROM a JOIN b USING (id)\n\
         ORDER BY total DESC, a.id\n\"\"\"\n\
         memory_limit = \"{}\"\nthreads = 4\nmax_temp_size = \"30 GiB\"\ntimeout = \"2h\"\n\n\
         [[output]]\nformat = \"parquet\"\nfile = \"exit.parquet\"\nmax_size = \"20 GiB\"\n",
        ByteSize(MAX_DISK),
        source("a", 7),
        source("b", 13),
        ByteSize(MEMORY_LIMIT),
    )
}

/// The most memory a page of the generated rows takes in the worker: each value's text, at its
/// widest, and its `Option<String>`, and the row's `Vec`.
fn page_memory(rows: u64) -> u64 {
    let id = rows.to_string().len() as u64;
    let amount = "9999.99".len() as u64;
    let ts = "2026-12-31 23:59:59".len() as u64;
    let label = "label 999".len() as u64;
    let row = id + amount + ts + label + 4 * VALUE_OVERHEAD + VALUE_OVERHEAD;
    PAGE_ROWS * row
}

/// A home folder of its own, with the in-memory DuckDB data source `generated`.
struct Home {
    home: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let home = Self {
            home: tempfile::tempdir().unwrap(),
        };
        DataSourceFile::new(home.config().join("data_sources.toml"))
            .save(&[DataSource {
                id: DataSourceId::generate(),
                name: "generated".to_owned(),
                color: None,
                safety: SafetyLevel::Normal,
                project: None,
                mcp_login: None,
                config: ConnectionConfig::DuckDb(DuckDbConfig::new(DuckDbDatabase::InMemory)),
            }])
            .unwrap();
        home
    }

    fn config(&self) -> PathBuf {
        self.home.path().join("settings")
    }

    fn report(&self, name: &str, text: &str) -> PathBuf {
        let path = self.config().join("reports").join(format!("{name}.toml"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn request(&self, report: &Path) -> RunRequest {
        let out = self.home.path().join("out");
        RunRequest {
            report: report.to_owned(),
            values: Default::default(),
            staging: out.join(".run"),
            work: self.work_root().join("run"),
            folder: out,
            defaults: LimitDefaults::default(),
            memory_limit: None,
            explain: None,
        }
    }

    fn work_root(&self) -> PathBuf {
        self.home.path().join("report-runs")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BINARY);
        command
            .arg("report-worker")
            .arg("--config-dir")
            .arg(self.config())
            .env("HOME", self.home.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("DBROVER_CONFIG_DIR");
        command
    }

    /// Runs `report`, watched as the app watches a run, and returns its events, and the most
    /// its folders held at once, sampled twice a second.
    fn run(&self, report: &Path) -> (Vec<RunEvent>, u64) {
        let request = self.request(report);
        std::fs::create_dir_all(self.work_root()).unwrap();
        let watch = RunWatch {
            timeout: Duration::from_secs(4 * 3600),
            stall_timeout: Duration::from_secs(10 * 60),
            disk: Some(disk_budget(&self.work_root(), ByteSize(MAX_DISK))),
            keep_awake: None,
        };
        let sampling = Arc::new(AtomicBool::new(true));
        let peak_disk = Arc::new(AtomicU64::new(0));
        let sampler = {
            let (sampling, peak_disk) = (Arc::clone(&sampling), Arc::clone(&peak_disk));
            let folders = [request.work.clone(), request.staging.clone()];
            std::thread::spawn(move || {
                while sampling.load(Ordering::SeqCst) {
                    let used: u64 = folders.iter().map(|folder| size_of(folder)).sum();
                    peak_disk.fetch_max(used, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
        };
        let run = WorkerRun::start(self.command(), &request, watch, || {}).unwrap();
        let mut events = Vec::new();
        while let Some(timed) = run.wait_next() {
            let seconds = timed.at_ms as f64 / 1000.0;
            match &timed.event {
                RunEvent::Progress { .. } => continue,
                RunEvent::SourceDone {
                    id, rows, phases, ..
                } => println!("{seconds:>8.1} s  source {id}: {rows} rows, {phases:?}"),
                RunEvent::CombineDone {
                    rows,
                    peak_memory,
                    peak_temp_size,
                    query_ms,
                    ..
                } => println!(
                    "{seconds:>8.1} s  combine: {rows} rows in {query_ms:?} ms, DuckDB's peaks \
                     {peak_memory:?} in memory and {peak_temp_size:?} spilled"
                ),
                RunEvent::Preview(_) => {}
                other => println!("{seconds:>8.1} s  {other:?}"),
            }
            events.push(timed.event);
        }
        sampling.store(false, Ordering::SeqCst);
        sampler.join().unwrap();
        assert!(!request.work.exists(), "the run's work folder is removed");
        (events, peak_disk.load(Ordering::SeqCst))
    }
}

/// The bytes of every file under `path`; nothing for one that has gone.
fn size_of(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| size_of(&entry.path()))
                .sum()
        })
        .unwrap_or(0)
}

fn worker_peak(events: &[RunEvent]) -> u64 {
    events
        .iter()
        .find_map(|event| match event {
            RunEvent::WorkerUsage { peak_memory } => *peak_memory,
            _ => None,
        })
        .unwrap()
}

#[test]
#[ignore = "slow: 100 million rows through the worker; run by hand"]
fn loads_a_hundred_million_rows_past_memory_and_writes_parquet_inside_its_bounds() {
    let rows: u64 = std::env::var("DBROVER_RS3_ROWS")
        .ok()
        .and_then(|rows| rows.parse().ok())
        .unwrap_or(ROWS);
    let home = Home::new();

    // The worker's own memory, the same run of a few rows: the process, its runtime and
    // DuckDB's own, which the bound adds to what the run's data may take.
    let baseline_report = home.report("baseline", &report(1_000));
    let (baseline_events, _) = home.run(&baseline_report);
    assert!(
        matches!(baseline_events.last(), Some(RunEvent::Finished { .. })),
        "{baseline_events:#?}"
    );
    let baseline = worker_peak(&baseline_events);

    let started = Instant::now();
    let exit = home.report("exit", &report(rows));
    let (events, peak_disk) = home.run(&exit);
    let elapsed = started.elapsed();
    assert!(
        matches!(events.last(), Some(RunEvent::Finished { .. })),
        "{events:#?}"
    );

    let (combined_rows, peak_temp_size, peak_duckdb) = events
        .iter()
        .find_map(|event| match event {
            RunEvent::CombineDone {
                rows,
                peak_temp_size,
                peak_memory,
                ..
            } => Some((*rows, *peak_temp_size, *peak_memory)),
            _ => None,
        })
        .unwrap();
    assert_eq!(combined_rows, rows, "every row joined and written");
    let spilled = peak_temp_size.unwrap_or(0);
    assert!(
        spilled > 0,
        "the combine step went past memory_limit and spilled"
    );
    let parquet = events
        .iter()
        .find_map(|event| match event {
            RunEvent::OutputWritten(file) => Some(file.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(parquet.rows, rows);

    let sources = 2;
    let queues = sources * (QUEUE_PAGES as u64 + 2) * page_memory(rows);
    // DuckDB's own peak counts what it allocated, a little past twice the limit at most here
    // (2 GiB and 1 MB); the worker's resident peak, which the bound is about, is far less.
    let duckdb = DUCKDB_OVERSHOOT * MEMORY_LIMIT;
    let bound = duckdb + queues + baseline;
    let peak = worker_peak(&events);
    println!(
        "\n{rows} rows a source in {:.0} s; Parquet {} ({}); disk at most {} of {}; \
         DuckDB spilled {} and held {}; worker peak {} of a bound of {} \
         (DuckDB {} + queues {} + baseline {})",
        elapsed.as_secs_f64(),
        ByteSize(parquet.size),
        parquet.path.display(),
        ByteSize(peak_disk),
        ByteSize(MAX_DISK),
        ByteSize(spilled),
        ByteSize(peak_duckdb.unwrap_or(0)),
        ByteSize(peak),
        ByteSize(bound),
        ByteSize(duckdb),
        ByteSize(queues),
        ByteSize(baseline),
    );
    assert!(peak_disk <= MAX_DISK, "inside max_disk");
    assert!(
        peak <= bound,
        "the worker's peak memory is within the bound"
    );
}
