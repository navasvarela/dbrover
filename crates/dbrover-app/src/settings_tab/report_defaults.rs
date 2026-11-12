//! Settings › Reports: the defaults of every report's limits, and the runs kept
//! (`docs/reporting.md`, "Settings › Reports"). Each is a choice of a few values around
//! DbRover's own, as the query history's settings are: a change saves at once, so a typed
//! value would save every keystroke on the way.

use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::time::Duration;

use dbrover_core::ByteSize;
use dbrover_report::{LimitDefaults, grouped};
use dbrover_store::{DEFAULT_RUNS_KEPT, ReportSettings};
use egui::Ui;

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;

/// The menus' choices that can't be zero, checked as the program is compiled.
const ROWS: [NonZeroU64; 4] = [
    non_zero_u64(100_000),
    non_zero_u64(1_000_000),
    non_zero_u64(10_000_000),
    non_zero_u64(100_000_000),
];
const THREADS: [NonZeroU32; 5] = [
    non_zero_u32(1),
    non_zero_u32(2),
    non_zero_u32(4),
    non_zero_u32(8),
    non_zero_u32(16),
];
const RUNS: [NonZeroU32; 5] = [
    non_zero_u32(5),
    non_zero_u32(10),
    non_zero_u32(20),
    non_zero_u32(50),
    non_zero_u32(100),
];

/// A default Settings › Reports sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportDefault {
    MaxRows,
    MaxBytes,
    SourceTimeout,
    MemoryLimit,
    MaxTempSize,
    Threads,
    CombineTimeout,
    MaxSize,
    RunTimeout,
    StallTimeout,
    KeepAwake,
    MaxDisk,
    WorkFolder,
    ResumableAbove,
    KeptRunsTotal,
    RunsKept,
}

impl ReportDefault {
    pub const ALL: [Self; 16] = [
        Self::MaxRows,
        Self::MaxBytes,
        Self::SourceTimeout,
        Self::MemoryLimit,
        Self::MaxTempSize,
        Self::Threads,
        Self::CombineTimeout,
        Self::MaxSize,
        Self::RunTimeout,
        Self::StallTimeout,
        Self::KeepAwake,
        Self::MaxDisk,
        Self::WorkFolder,
        Self::ResumableAbove,
        Self::KeptRunsTotal,
        Self::RunsKept,
    ];

    /// The caption of the card it is in.
    pub fn section(self) -> &'static str {
        match self {
            Self::MaxRows | Self::MaxBytes | Self::SourceTimeout => "Each source",
            Self::MemoryLimit | Self::MaxTempSize | Self::Threads | Self::CombineTimeout => {
                "The combine step's DuckDB"
            }
            Self::MaxSize
            | Self::RunTimeout
            | Self::StallTimeout
            | Self::KeepAwake
            | Self::MaxDisk
            | Self::WorkFolder
            | Self::ResumableAbove
            | Self::KeptRunsTotal
            | Self::RunsKept => "Outputs and runs",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::MaxRows => "Rows a source may return",
            Self::MaxBytes => "Values a source may return",
            Self::SourceTimeout => "A source's time limit",
            Self::MemoryLimit => "Memory before spilling to disk",
            Self::MaxTempSize => "Spilled to disk at most",
            Self::Threads => "Threads",
            Self::CombineTimeout => "The combine query's time limit",
            Self::MaxSize => "Values an output may write",
            Self::RunTimeout => "A whole run's time limit",
            Self::StallTimeout => "A run without progress for",
            Self::KeepAwake => "Keep the computer awake",
            Self::MaxDisk => "Disk a run may use",
            Self::WorkFolder => "Work folder",
            Self::ResumableAbove => "Keep a stopped run past",
            Self::KeptRunsTotal => "Kept runs may hold",
            Self::RunsKept => "Runs kept per report",
        }
    }

    /// Which key of a report's file overrides it, under the row's title.
    pub fn description(self) -> &'static str {
        match self {
            Self::MaxRows => "A source's max_rows overrides it.",
            Self::MaxBytes => "A source's max_bytes overrides it.",
            Self::SourceTimeout => "A source's timeout overrides it, across all its batches.",
            Self::MemoryLimit => "memory_limit in [combine] overrides it.",
            Self::MaxTempSize => "max_temp_size in [combine] overrides it.",
            Self::Threads => "threads in [combine] overrides it.",
            Self::CombineTimeout => "timeout in [combine] overrides it.",
            Self::MaxSize => "An output's max_size overrides it.",
            Self::RunTimeout => "timeout in [report] overrides it.",
            Self::StallTimeout => {
                "Is stopped as stuck: no row, batch or work in DuckDB. stall_timeout in [report] \
                 overrides it."
            }
            Self::KeepAwake => "While a run goes on past two minutes, on macOS and Linux.",
            Self::MaxDisk => {
                "Its DuckDB's file, what it spills and its files being written; max_disk in \
                 [report] overrides it."
            }
            Self::WorkFolder => {
                "Where each run keeps its DuckDB's file and spills, removed when it ends unless \
                 it stopped and can resume: a roomier disk, say."
            }
            Self::ResumableAbove => {
                "What a failed or stopped run's finished work must have read for its work \
                 folder to be kept, so it can resume; resumable_above in [report] overrides it."
            }
            Self::KeptRunsTotal => {
                "Every kept work folder together; past it the oldest go, and each goes after a \
                 week."
            }
            Self::RunsKept => "Older runs are deleted as each new one is kept.",
        }
    }

    /// The key it is saved under in `settings.toml`'s `[reports]`, which search finds too.
    pub fn key(self) -> &'static str {
        match self {
            Self::MaxRows => "max_rows",
            Self::MaxBytes => "max_bytes",
            Self::SourceTimeout => "source_timeout",
            Self::MemoryLimit => "memory_limit",
            Self::MaxTempSize => "max_temp_size",
            Self::Threads => "threads",
            Self::CombineTimeout => "combine_timeout",
            Self::MaxSize => "max_size",
            Self::RunTimeout => "run_timeout",
            Self::StallTimeout => "stall_timeout",
            Self::KeepAwake => "keep_awake",
            Self::MaxDisk => "max_disk",
            Self::WorkFolder => "work_folder",
            Self::ResumableAbove => "resumable_above",
            Self::KeptRunsTotal => "kept_runs_total",
            Self::RunsKept => "runs_kept",
        }
    }

    /// Draws the menu that changes it in `settings`.
    pub fn control(self, ui: &mut Ui, settings: &mut ReportSettings) {
        let constants = LimitDefaults::default();
        let id = self.key();
        match self {
            Self::MaxRows => choice(
                ui,
                id,
                &mut settings.max_rows,
                &rows(constants.source.max_rows),
                &ROWS,
                |rows_set| rows(rows_set.get()),
            ),
            Self::MaxBytes => {
                size_choice(ui, id, &mut settings.max_bytes, constants.source.max_bytes)
            }
            Self::SourceTimeout => {
                time_choice(
                    ui,
                    id,
                    &mut settings.source_timeout,
                    constants.source.timeout,
                );
            }
            Self::MemoryLimit => choice(
                ui,
                id,
                &mut settings.memory_limit,
                &constants.combine.memory_limit.to_string(),
                &[512 * MIB, GIB, 2 * GIB, 4 * GIB, 8 * GIB, 16 * GIB].map(ByteSize),
                |size| size.to_string(),
            ),
            Self::MaxTempSize => choice(
                ui,
                id,
                &mut settings.max_temp_size,
                &constants.combine.max_temp_size.to_string(),
                &[GIB, 10 * GIB, 50 * GIB, 100 * GIB].map(ByteSize),
                |size| size.to_string(),
            ),
            Self::Threads => choice(
                ui,
                id,
                &mut settings.threads,
                &format!("half the cores, {}", constants.combine.threads()),
                &THREADS,
                |threads| threads.to_string(),
            ),
            Self::CombineTimeout => {
                time_choice(
                    ui,
                    id,
                    &mut settings.combine_timeout,
                    constants.combine.timeout,
                );
            }
            Self::MaxSize => size_choice(ui, id, &mut settings.max_size, constants.output.max_size),
            Self::RunTimeout => choice(
                ui,
                id,
                &mut settings.run_timeout,
                &duration(constants.run_timeout),
                &[10 * MINUTE, 30 * MINUTE, HOUR, 2 * HOUR, 4 * HOUR].map(Duration::from_secs),
                duration,
            ),
            Self::StallTimeout => {
                time_choice(ui, id, &mut settings.stall_timeout, constants.stall_timeout);
            }
            Self::KeepAwake => {
                let mut on = settings.keep_awake.unwrap_or(true);
                if ui.checkbox(&mut on, "During long runs").changed() {
                    // Unset is the default, on.
                    settings.keep_awake = (!on).then_some(false);
                }
            }
            Self::MaxDisk => choice(
                ui,
                id,
                &mut settings.max_disk,
                &constants.max_disk.to_string(),
                &[5 * GIB, 20 * GIB, 50 * GIB, 100 * GIB, 500 * GIB].map(ByteSize),
                |size| size.to_string(),
            ),
            Self::WorkFolder => work_folder(ui, &mut settings.work_folder),
            Self::ResumableAbove => choice(
                ui,
                id,
                &mut settings.resumable_above,
                &constants.resumable_above.to_string(),
                &[100 * MIB, GIB, 10 * GIB].map(ByteSize),
                |size| size.to_string(),
            ),
            Self::KeptRunsTotal => choice(
                ui,
                id,
                &mut settings.kept_runs_total,
                &ByteSize(dbrover_engine::DEFAULT_KEPT_RUNS_TOTAL).to_string(),
                &[10 * GIB, 50 * GIB, 100 * GIB, 500 * GIB].map(ByteSize),
                |size| size.to_string(),
            ),
            Self::RunsKept => choice(
                ui,
                id,
                &mut settings.runs_kept,
                &format!("the newest {DEFAULT_RUNS_KEPT}"),
                &RUNS,
                |kept| format!("the newest {kept}"),
            ),
        }
    }
}

/// The folder chosen, or that runs use the data folder's, with buttons to choose another and
/// to go back to the default.
fn work_folder(ui: &mut Ui, folder: &mut Option<PathBuf>) {
    ui.horizontal(|ui| {
        let shown = folder.as_deref().map_or_else(
            || "default: report-runs in DbRover's data folder".to_owned(),
            |folder| folder.display().to_string(),
        );
        ui.label(shown);
        if ui.button("Choose…").clicked()
            && let Some(chosen) = crate::file_picker::pick_work_folder(folder.as_deref())
        {
            *folder = Some(chosen);
        }
        if folder.is_some() && ui.button("Use the default").clicked() {
            *folder = None;
        }
    });
}

fn size_choice(ui: &mut Ui, id: &str, size: &mut Option<ByteSize>, default: ByteSize) {
    choice(
        ui,
        id,
        size,
        &default.to_string(),
        &[256 * MIB, GIB, 4 * GIB, 16 * GIB].map(ByteSize),
        |size| size.to_string(),
    );
}

fn time_choice(ui: &mut Ui, id: &str, time: &mut Option<Duration>, default: Duration) {
    choice(
        ui,
        id,
        time,
        &duration(default),
        &[MINUTE, 10 * MINUTE, 30 * MINUTE, HOUR, 2 * HOUR].map(Duration::from_secs),
        duration,
    );
}

/// A menu of DbRover's default and `choices`. A value set by hand in `settings.toml` shows as
/// it is, written by `label`.
fn choice<T: Copy + PartialEq>(
    ui: &mut Ui,
    id: &str,
    value: &mut Option<T>,
    default: &str,
    choices: &[T],
    label: impl Fn(T) -> String,
) {
    let default = format!("default: {default}");
    egui::ComboBox::from_id_salt(("report_default", id))
        .selected_text(value.map_or_else(|| default.clone(), &label))
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, default);
            for &choice in choices {
                ui.selectable_value(value, Some(choice), label(choice));
            }
        });
}

fn rows(rows: u64) -> String {
    format!("{} rows", grouped(rows))
}

/// `10 minutes`, `1 hour`, `90 seconds`: the largest whole unit.
fn duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (count, unit) = if seconds >= HOUR && seconds.is_multiple_of(HOUR) {
        (seconds / HOUR, "hour")
    } else if seconds >= MINUTE && seconds.is_multiple_of(MINUTE) {
        (seconds / MINUTE, "minute")
    } else {
        (seconds, "second")
    };
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

const fn non_zero_u64(value: u64) -> NonZeroU64 {
    match NonZeroU64::new(value) {
        Some(value) => value,
        None => panic!("a menu's choice must not be zero"),
    }
}

const fn non_zero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => panic!("a menu's choice must not be zero"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_duration_in_its_largest_whole_unit() {
        let cases = [
            (60, "1 minute"),
            (600, "10 minutes"),
            (3_600, "1 hour"),
            (7_200, "2 hours"),
            (5_400, "90 minutes"),
            (45, "45 seconds"),
        ];
        for (seconds, text) in cases {
            assert_eq!(duration(Duration::from_secs(seconds)), text);
        }
    }
}
