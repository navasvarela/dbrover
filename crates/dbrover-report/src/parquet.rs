//! Parquet outputs, written by the combine step's DuckDB with `COPY` straight from the run's
//! database, so no row passes through DbRover's writers (`docs/reports-at-scale.md`, RS3d):
//!
//! 1. the combine query is copied to the first Parquet output's file, under its partial name in
//!    the staging folder, the one folder the locked workspace may write in;
//! 2. each other Parquet output is copied from that file, so the query runs once;
//! 3. the CSV and Excel outputs, the preview and the charts read the rows back from it.
//!
//! Each file's size is held to its output's `max_size`, and it is put in place with the run's
//! other files.

use std::path::Path;
use std::time::{Duration, Instant};

use dbrover_driver_duckdb::{
    ParquetCompression, Workspace, WorkspaceProfile, copy_to_parquet, read_parquet,
};
use dbrover_export::{CompletedFile, ParquetOptions};

use crate::combine::{CombineReader, DuckDbQuery, progress, read_profile};
use crate::live::{ProgressClock, waiting};
use crate::run::{Completed, Count, timed};
use crate::{CombineLimits, LimitHit, Plan, PlannedOutput, RunEvent, RunFailure, RunValues, Step};

/// A Parquet output of a run, its file not written yet.
pub(crate) struct ParquetFile<'a> {
    number: usize,
    name: String,
    output: &'a PlannedOutput,
    compression: ParquetCompression,
    file: CompletedFile,
}

/// The run's Parquet outputs, in the file's order, each to be written in `staging`.
pub(crate) fn parquet_files<'a>(
    plan: &'a Plan,
    values: &RunValues,
    folder: &Path,
    staging: &Path,
) -> Vec<ParquetFile<'a>> {
    plan.outputs
        .iter()
        .zip(&values.files)
        .enumerate()
        .filter_map(|(index, (output, name))| {
            let options = output.format.written_by_duckdb()?;
            Some(ParquetFile {
                number: index + 1,
                name: name.clone(),
                output,
                compression: compression(options),
                file: CompletedFile::written_by_another(&folder.join(name), staging),
            })
        })
        .collect()
}

fn compression(options: ParquetOptions) -> ParquetCompression {
    match options.compression {
        dbrover_export::ParquetCompression::Zstd => ParquetCompression::Zstd,
        dbrover_export::ParquetCompression::Snappy => ParquetCompression::Snappy,
    }
}

impl ParquetFile<'_> {
    fn step(&self) -> Step {
        Step::Output {
            number: self.number,
            file: self.name.clone(),
        }
    }

    /// The file written, holding `rows`, once its size is checked against `max_size`.
    fn completed(self, rows: u64, busy: Duration) -> Result<Completed, RunFailure> {
        let size = self.file.size().unwrap_or_default();
        let max_size = self.output.limits.max_size;
        if size > max_size.0 {
            let value = max_size.to_string();
            return Err(RunFailure::at_limit(
                self.step(),
                format!(
                    "the Parquet file came to {}, more than {value}, its max_size; write fewer \
                     rows or columns, or raise max_size",
                    crate::ByteSize(size)
                ),
                LimitHit {
                    key: "max_size".to_owned(),
                    value,
                    raise_in: format!("output {}", self.number),
                    set_in: None,
                },
            ));
        }
        Ok(Completed {
            step: self.step(),
            rows,
            warnings: Vec::new(),
            busy,
            file: self.file,
        })
    }
}

/// What writing the Parquet outputs did.
pub(crate) struct Copied {
    /// The rows the combine query returned, which every file holds.
    pub(crate) rows: u64,
    /// The combine query's time, copied into the first file.
    pub(crate) query: Duration,
    /// The profile of that `COPY`, which holds the combine query's plan.
    pub(crate) profile: WorkspaceProfile,
    pub(crate) files: Vec<Completed>,
    /// The first file, which the other outputs read the rows back from.
    first: std::path::PathBuf,
}

impl Copied {
    /// The query that reads the rows back for the outputs DbRover writes, the preview and the
    /// charts: every row when something but the preview needs them, else one more than the
    /// preview shows, so it can say there are more.
    pub(crate) fn read_back(&self, every_row: bool) -> String {
        let query = read_parquet(&self.first);
        if every_row {
            query
        } else {
            format!("{query} LIMIT {}", crate::PREVIEW_ROWS + 1)
        }
    }
}

/// What writing Parquet needs of the combine step.
pub(crate) struct ParquetCopy<'a> {
    pub(crate) query: &'a str,
    pub(crate) values: &'a RunValues,
    pub(crate) limits: &'a CombineLimits,
}

impl ParquetCopy<'_> {
    /// Writes `files`, of which there is at least one: the combine query into the first, each
    /// other from it. Says each second that the combine step is busy.
    pub(crate) async fn write(
        &self,
        workspace: &mut Workspace,
        files: Vec<ParquetFile<'_>>,
        events: &mut dyn FnMut(RunEvent),
    ) -> Result<Copied, RunFailure> {
        let mut files = files.into_iter();
        let Some(first) = files.next() else {
            return Err(RunFailure::new(Step::Combine, "no Parquet output to write"));
        };
        let started = Instant::now();
        let copy = copy_to_parquet(self.query, first.file.partial(), first.compression);
        let mut clock = ProgressClock::new();
        let (rows, query) = waiting(Box::pin(self.run(workspace, &copy)), |so_far| {
            let busy = clock.lap(so_far);
            events(progress(
                &Step::Combine,
                &Count::default(),
                started,
                None,
                busy,
            ));
        })
        .await;
        let rows = rows.map_err(|error| error.at(Step::Combine))?;
        let profile = read_profile(workspace).await;
        let source = first.file.partial().to_owned();
        let mut written = vec![first.completed(rows, query)?];
        for other in files {
            let copy = copy_to_parquet(
                &read_parquet(&source),
                other.file.partial(),
                other.compression,
            );
            let (copied, busy) = timed(self.run(workspace, &copy)).await;
            copied.map_err(|error| error.at(other.step()))?;
            written.push(other.completed(rows, busy)?);
        }
        Ok(Copied {
            rows,
            query,
            profile,
            files: written,
            first: source,
        })
    }

    /// Runs a `COPY` under the combine step's timeout: the rows it wrote.
    async fn run(&self, workspace: &mut Workspace, copy: &str) -> Result<u64, crate::SourceError> {
        let reader = CombineReader::start(
            workspace,
            copy,
            self.values,
            DuckDbQuery::Combine,
            self.limits,
        )
        .await?;
        reader.finish().await.map(Option::unwrap_or_default)
    }
}
