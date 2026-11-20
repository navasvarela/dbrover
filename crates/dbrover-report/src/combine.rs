//! A run with a combine step (`docs/reporting.md`, "RP2 — Several sources"):
//!
//! 1. open the run's DuckDB on a file in the run's work folder, spilling beside it;
//! 2. attach each DuckDB file, and load every other source in rounds, the sources of a round
//!    at the same time, each under its caps, a batch of its lookup's values at a time, typing
//!    its columns once its last row is in;
//! 3. lock the database down, and run the combine query under its timeout, its rows going to
//!    the outputs as a source's do without a combine step;
//! 4. run each chart's own query, if it has one, under the same limits, its rows going to the
//!    chart.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow_schema::Schema;
use dbrover_core::{
    CancelHandle, Connection, CostEstimate, CostGuard, DriverError, HandedOver, Handoff,
    HandoffFiles, HandoffFolder, LakeFormat, ObjectStorage, ReadPace, ResultCursor, Row,
    ScanEstimate, describe_error,
};
use dbrover_driver_duckdb::{
    Allowed, BucketSignIn, DuckDbType, HandoffLoaded, LakeView, LoadColumn, ParquetCompression,
    TableLoader, Workspace, WorkspaceLimit, WorkspaceLimits, WorkspaceProfile, WorkspaceStore,
    WorkspaceTable,
};
use dbrover_export::ExportColumn;
use dbrover_language::Literal;
use dbrover_sql::Dialect;
use futures_util::future::try_join_all;
use tokio::sync::mpsc;

mod ranges;

use crate::chart::ChartRun;
use crate::checkpoints::Checkpoints;
use crate::composed::{Composed, Reused, composed};
use crate::declared::Shaped;
use crate::handoff::HandoffFolders;
use crate::live::{ProgressClock, waiting};
use crate::parquet::{ParquetCopy, ParquetFile, parquet_files};
use crate::run::{Count, Feed, Sink, millis, timed};
use crate::statement::{Statements, lookup_literal};
use crate::typing::{ColumnTyper, DriverTypes};
use crate::{
    AttachedSource, Batch, Busy, Chart, CombineLimits, Combined, HandedOverFiles, KeptColumn,
    LakeSource, LimitHit, LoadedColumn, Logic, Parts, Plan, PlannedLookup, PlannedSource,
    PlannedSplit, QuerySource, ReportSource, Resumed, RunEvent, RunFailure, RunFolders, RunValues,
    SourceError, SourceKept, SourceOpener, SourcePart, SourcePhases, SourceReader, SourceStatement,
    Step,
};

/// Rows asked of the combine query at a time, as a source's are.
const PAGE_ROWS: usize = 1_000;

/// The pages a source's reader may be ahead of its loader: enough that receiving and appending
/// overlap, few enough that a fast network never piles rows up in the worker. With the page
/// being read and the one being appended, a source holds at most two more than this at once,
/// which is what bounds the worker's memory outside DuckDB (`docs/reports-at-scale.md`, RS3c).
pub const QUEUE_PAGES: usize = 4;

/// How long an interrupted query gets to stop before the run gives up on it.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// The run's database file, in its work folder.
const WORKSPACE_FILE: &str = "workspace.duckdb";

/// The folder in the run's work folder that DuckDB spills to.
const SPILL_FOLDER: &str = "spill";

/// A run with a combine step, and what its steps read: the whole run, or one range of a
/// partitioned run.
pub(crate) struct Combining<'a, 'v, O> {
    pub(crate) plan: &'a Plan,
    pub(crate) values: &'v RunValues,
    pub(crate) folders: RunFolders<'a>,
    pub(crate) combined: &'a Combined,
    pub(crate) opener: &'v O,
    /// Where the sources it loads are recorded, and those an earlier run loaded found (RS5e);
    /// `None` for a range of a partitioned run, whose DuckDB is its own.
    pub(crate) checkpoints: Option<&'v Checkpoints>,
    /// The folders this run's handed-over files go in, one per statement, which the run
    /// removes as it ends (RS7f). The same for every range of a partitioned run, so one
    /// cleanup covers them all.
    pub(crate) handoffs: &'v HandoffFolders,
}

impl<'a, O: SourceOpener> Combining<'a, '_, O> {
    /// Runs the combine step into `sink`: see the module. The run's DuckDB is made for it and
    /// removed after it, so each range of a partitioned run has a database of its own, which
    /// holds one range's tables and spill. An unpartitioned run that fails leaves it, for its
    /// work folder to keep if the run can resume, and a run resuming one opens it as it was
    /// left (RS5e).
    pub(crate) async fn run(
        &self,
        sink: &mut Sink<'a>,
        events: &mut dyn FnMut(RunEvent),
    ) -> Result<(), RunFailure> {
        let limits = self.limits();
        let work = self.folders.work;
        let file = work.join(WORKSPACE_FILE);
        let resumes = self.checkpoints.is_some_and(Checkpoints::has_sources) && file.exists();
        let store = if resumes {
            WorkspaceStore::Kept(file)
        } else {
            // What a range a stopped run was in the middle of left, which a run resuming it
            // reads again from the start (RS5d), or a run that loaded nothing worth keeping.
            remove_workspace(work);
            WorkspaceStore::File(file)
        };
        let mut workspace = Workspace::open(
            &store,
            &WorkspaceLimits {
                memory_limit: limits.memory_limit.0,
                threads: limits.threads(),
                temp_directory: work.join(SPILL_FOLDER),
                max_temp_size: limits.max_temp_size.0,
            },
        )
        .await
        .map_err(|error| RunFailure::new(Step::Combine, describe_error(&error)))?;
        let outcome = self.combine(&mut workspace, sink, events).await;
        workspace.close().await;
        if outcome.is_ok() || self.checkpoints.is_none() {
            remove_workspace(work);
        }
        outcome
    }

    fn limits(&self) -> &CombineLimits {
        &self.combined.combine.limits
    }

    async fn combine(
        &self,
        workspace: &mut Workspace,
        sink: &mut Sink<'a>,
        events: &mut dyn FnMut(RunEvent),
    ) -> Result<(), RunFailure> {
        // The report being run and every report it uses, each loading into its own schema.
        let (reports, reused) =
            composed(self.plan, self.values, self.combined, self.folders.cache)?;
        // A result taken from the cache is read in before the lock, so the folder it is in
        // needs no allowance (RS9f).
        for taken in &reused {
            self.take_result(taken, workspace, events).await?;
        }
        let mut lakes = Vec::new();
        for report in &reports {
            if let Some(schema) = report.schema() {
                workspace
                    .create_schema(&schema)
                    .await
                    .map_err(|error| RunFailure::new(Step::Combine, describe_error(&error)))?;
            }
            self.load_report(report, workspace, &mut lakes, events)
                .await?;
        }
        let folders = self.folders;
        let parquet = parquet_files(self.plan, self.values, folders);
        // DuckDB writes only in the folders it writes the Parquet files in.
        let mut writable: Vec<&Path> = parquet.iter().filter_map(ParquetFile::folder).collect();
        // Where a result kept for a report used as a source is written (RS9f).
        if let Some(cache) = self.folders.cache.filter(|_| self.keeps_results(&reports)) {
            writable.push(cache.folder);
        }
        writable.sort();
        writable.dedup();
        for folder in &writable {
            make_folder(folder)?;
        }
        if !lakes.is_empty() {
            // What each lake's scan asks the bucket for, which the profile keeps (RS7e); the
            // settings freeze at the lock, so it starts here.
            workspace
                .log_bucket_requests()
                .await
                .map_err(|error| RunFailure::new(Step::Combine, describe_error(&error)))?;
        }
        let prefixes: Vec<&str> = lakes.iter().map(|lake| lake.view.prefix()).collect();
        workspace
            .lock(&Allowed {
                folders: &writable,
                lakes: &prefixes,
            })
            .await
            .map_err(|error| RunFailure::new(Step::Combine, describe_error(&error)))?;
        // Loading the other sources can take longer than a temporary credential lasts, and a
        // locked workspace still takes a secret, so each lake's are renewed now, as late as
        // they can be: a secret is a session's, so no other connection can replace them once
        // the combine query has the workspace's.
        for lake in &mut lakes {
            workspace
                .sign_in_to_bucket(&mut lake.sign_in)
                .await
                .map_err(|error| {
                    RunFailure::new(
                        Step::Combine,
                        format!(
                            "could not sign in to the bucket of the lake {:?} again: {}",
                            lake.id,
                            describe_error(&error)
                        ),
                    )
                })?;
        }

        // Each report used as a source makes its result now, innermost first, so the report
        // that reads it finds it as a table when its own combine runs (RS9c).
        for report in reports.iter().rev() {
            let Some(into) = &report.into else {
                continue;
            };
            self.make_result(report, into, workspace, events).await?;
        }
        let started = Instant::now();
        let combine_query = self.combined.combine.query.sql();
        let copied = if parquet.is_empty() {
            None
        } else {
            let copy = ParquetCopy {
                query: combine_query,
                values: self.values,
                limits: self.limits(),
            };
            Some(copy.write(workspace, parquet, events).await?)
        };
        let query = copied.as_ref().map_or_else(
            || combine_query.to_owned(),
            |copied| copied.read_back(self.streams_every_row()),
        );
        let mut clock = ProgressClock::new();
        let (reader, starting) = waiting(
            Box::pin(CombineReader::start(
                workspace,
                &query,
                self.values,
                DuckDbQuery::Combine,
                self.limits(),
            )),
            |so_far| {
                events(progress(
                    &Step::Combine,
                    &Count::default(),
                    started,
                    clock.lap(so_far),
                ));
            },
        )
        .await;
        let mut reader = reader.map_err(|error| error.at(Step::Combine))?;
        let streamed = sink
            .stream(&mut reader, &Feed::Combine, self.values, started, events)
            .await;
        reader.close().await;
        let mut read = streamed?;
        let read_ms = millis(starting + read.waited_first + read.waited_rest);
        let (profile, query_ms) = match copied {
            None => (read_profile(workspace).await, read_ms),
            Some(copied) => {
                // The preview may have read only its rows of the file.
                read.rows = copied.rows;
                sink.add_copied(copied.files);
                (copied.profile, millis(copied.query))
            }
        };
        sink.count(&read);
        let peaks = profile.peaks;
        events(RunEvent::CombineDone {
            rows: read.rows,
            bytes: read.bytes,
            first_row_ms: read.first_row.map(millis),
            elapsed_ms: millis(started.elapsed()),
            peak_memory: peaks.memory,
            peak_temp_size: peaks.temp_size,
            query_ms: Some(query_ms),
            plan: profile.plan,
        });
        self.query_charts(workspace, sink).await?;
        self.lakes_scanned(workspace, &lakes, events).await;
        Ok(())
    }

    /// What each lake's scan read from its bucket, once the combine query and the charts have
    /// run: the requests DuckDB made and the bytes their answers carried (RS7e).
    ///
    /// A log that can't be read leaves the numbers out of the profile rather than failing a
    /// run that has its rows.
    async fn lakes_scanned(
        &self,
        workspace: &mut Workspace,
        lakes: &[ReadLake],
        events: &mut dyn FnMut(RunEvent),
    ) {
        for lake in lakes {
            match workspace.bucket_requests(&lake.key_prefix).await {
                Ok(traffic) => events(RunEvent::LakeScanned {
                    id: lake.id.clone(),
                    requests: traffic.requests,
                    bytes: traffic.bytes,
                }),
                Err(error) => tracing::warn!(
                    "could not read what the lake {:?} asked its bucket for: {}",
                    lake.id,
                    describe_error(&error)
                ),
            }
        }
    }

    /// Whether anything but the preview reads every row of the result: an output DbRover
    /// writes, or a chart drawn from the result rather than a query of its own.
    fn streams_every_row(&self) -> bool {
        let plan = self.plan;
        plan.outputs
            .iter()
            .any(|output| output.format.written_by_duckdb().is_none())
            || plan.charts.iter().any(|chart| chart.query.is_none())
    }

    /// Each chart with a query of its own, which runs in the locked-down workspace now the
    /// combine step is done, a chart at a time in the file's order, under the combine step's
    /// limits (`docs/charts-as-code.md`, CH3e), its rows going to the chart.
    async fn query_charts(
        &self,
        workspace: &mut Workspace,
        sink: &mut Sink<'a>,
    ) -> Result<(), RunFailure> {
        let plan = self.plan;
        let files = &self.values.charts;
        for (index, (chart, file)) in plan.charts.iter().zip(files).enumerate() {
            if let Some(query) = &chart.query {
                let read = self.chart_rows(workspace, index + 1, chart, file, query);
                sink.add_chart(read.await?);
            }
        }
        Ok(())
    }

    /// Runs a chart's `query` and gives its rows to the chart.
    async fn chart_rows(
        &self,
        workspace: &mut Workspace,
        number: usize,
        chart: &'a Chart,
        file: &str,
        query: &str,
    ) -> Result<ChartRun<'a>, RunFailure> {
        let step = Step::Chart {
            number,
            file: file.to_owned(),
        };
        let query_of = DuckDbQuery::Chart;
        let mut reader =
            CombineReader::start(workspace, query, self.values, query_of, self.limits())
                .await
                .map_err(|error| error.at(step.clone()))?;
        let mut run = ChartRun::new(number, chart, file, reader.columns());
        let read = loop {
            match reader.next_page().await {
                Ok(Some(page)) => {
                    if let Err(failure) = run.add(&page) {
                        break Err(failure);
                    }
                }
                Ok(None) => break Ok(()),
                Err(error) => break Err(error.at(step.clone())),
            }
        };
        reader.close().await;
        read.map(|()| run)
    }

    /// Attaches, reads in place and loads every source of `report` into its schema: the
    /// DuckDB files first, then each lake's view, then the sources with a query in the rounds
    /// their lookups put them in (RS9c; one report is `docs/reporting.md`'s RP2).
    ///
    /// A report source whose own report has no combine step has no schema of its own: its one
    /// source loads straight into the table the report that names it reads, since its rows are
    /// its result.
    async fn load_report(
        &self,
        report: &Composed<'_>,
        workspace: &mut Workspace,
        lakes: &mut Vec<ReadLake>,
        events: &mut dyn FnMut(RunEvent),
    ) -> Result<(), RunFailure> {
        for source in &report.combined.sources {
            match source {
                PlannedSource::Attached(attached) => {
                    // An attached file is a catalog, not a table, so it keeps its id across
                    // the whole run; planning refused two files under one id.
                    attach(workspace, attached).await?;
                    events(RunEvent::SourceAttached {
                        id: attached.id.clone(),
                        path: attached.path.clone(),
                    });
                }
                PlannedSource::Lake(lake) => {
                    let read = self
                        .read_lake(workspace, lake, report.table(&lake.id))
                        .await?;
                    events(RunEvent::LakeRead {
                        id: report.step_id(&lake.id),
                        url: read.view.reads_from().to_owned(),
                        format: lake.format.name().to_owned(),
                    });
                    lakes.push(read);
                }
                PlannedSource::Query(_) | PlannedSource::Report(_) => {}
            }
        }
        let workspace = &*workspace;
        let mut loaded: Vec<LoadedTable> = self.resume_sources(report, events)?;
        for round in report.combined.rounds() {
            let events = RefCell::new(&mut *events);
            let loads = round
                .into_iter()
                .filter(|source| {
                    !loaded
                        .iter()
                        .any(|table| table.table.name() == source.id.as_str())
                })
                .map(|source| {
                    self.load_anew(
                        Loaded {
                            source,
                            report,
                            table: report.table(&source.id),
                        },
                        workspace,
                        &loaded,
                        &events,
                    )
                });
            let tables = try_join_all(loads).await?;
            loaded.extend(tables);
        }
        // A report of one source without a combine: its rows go into the table the report
        // that names it reads, under that source's id.
        for source in &report.combined.sources {
            let Some(inner) = source.report() else {
                continue;
            };
            let Logic::Direct(query) = &inner.plan.logic else {
                continue;
            };
            let values = inner.values(&report.values).map_err(|problems| {
                RunFailure::new(
                    Step::Source {
                        id: report.step_id(&inner.id),
                    },
                    problems.join("\n"),
                )
            })?;
            let mut path = report.path.clone();
            path.push(inner.id.clone());
            let only = Composed {
                path,
                plan: &inner.plan,
                combined: report.combined,
                values,
                into: None,
                source: Some(inner),
            };
            let events = RefCell::new(&mut *events);
            self.load_anew(
                Loaded {
                    source: query,
                    report: &only,
                    table: report.table(&inner.id),
                },
                workspace,
                &[],
                &events,
            )
            .await?;
        }
        Ok(())
    }

    /// Whether any report of the run keeps its result for a later one (RS9f), which is what
    /// the cache's folder has to be writable for.
    fn keeps_results(&self, reports: &[Composed<'_>]) -> bool {
        reports
            .iter()
            .any(|report| report.source.is_some_and(|source| source.reuse.is_some()))
    }

    /// Reads a result kept for a report used as a source into the table the report that reads
    /// it looks for, instead of running that report at all (RS9f). Before the lock, so the
    /// file needs no allowance; a file DuckDB can't read fails the run rather than running
    /// the report again, since a result the run can't trust is not one to work around
    /// silently.
    async fn take_result(
        &self,
        taken: &Reused,
        workspace: &mut Workspace,
        events: &mut dyn FnMut(RunEvent),
    ) -> Result<(), RunFailure> {
        let step = Step::Source {
            id: taken.step_id.clone(),
        };
        let started = Instant::now();
        let read = dbrover_driver_duckdb::read_parquet(&taken.file);
        workspace
            .create_table_as(&taken.table, &read, &[])
            .await
            .map_err(|error| {
                RunFailure::new(
                    step.clone(),
                    format!(
                        "could not read the result kept for it in {}: {}; run the report from \
                         scratch to make it again",
                        taken.file.display(),
                        describe_error(&error)
                    ),
                )
            })?;
        let rows = workspace
            .count_rows(&taken.table)
            .await
            .map_err(|error| RunFailure::new(step.clone(), describe_error(&error)))?;
        events(RunEvent::SourceDone {
            id: taken.step_id.clone(),
            rows,
            bytes: taken.file.metadata().map(|file| file.len()).unwrap_or(0),
            first_row_ms: None,
            elapsed_ms: millis(started.elapsed()),
            table: Vec::new(),
            warnings: Vec::new(),
            batches: None,
            parts: None,
            phases: None,
            scanned_bytes: None,
            handed_over: None,
            reused_ms: Some(millis(taken.age)),
        });
        Ok(())
    }

    /// Keeps the result of a report used as a source as Parquet, for a later run of any
    /// report that reads it with `reuse` (RS9f). A result that can't be written is a warning,
    /// not a failure: the rows are right, and the next run makes it again.
    async fn keep_result(
        &self,
        workspace: &mut Workspace,
        source: &ReportSource,
        into: &WorkspaceTable,
        values: &RunValues,
    ) {
        let Some(cache) = self.folders.cache else {
            return;
        };
        let Some(file) = source.kept_result(cache.folder, values) else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(cache.folder) {
            tracing::warn!(
                "could not make {}, so the result of {:?} wasn't kept: {error}",
                cache.folder.display(),
                source.report
            );
            return;
        }
        // Written beside its name and renamed into place, so a run killed in the middle of
        // writing never leaves half a result for the next run to read.
        let partial = file.with_extension("parquet.partial");
        let kept = self
            .copy_result(workspace, into, &partial)
            .await
            .and_then(|()| std::fs::rename(&partial, &file).map_err(|error| error.to_string()));
        if let Err(problem) = kept {
            let _ = std::fs::remove_file(&partial);
            tracing::warn!(
                "the result of {:?} wasn't kept in {}: {problem}",
                source.report,
                file.display()
            );
        }
    }

    /// Copies `into`'s rows to `file` as Parquet, with the combine step's DuckDB, under its
    /// timeout. The folder is one the lock allows, as the staging folder is (RS3d).
    async fn copy_result(
        &self,
        workspace: &mut Workspace,
        into: &WorkspaceTable,
        file: &Path,
    ) -> Result<(), String> {
        let query = format!("SELECT * FROM {}", into.qualified());
        let copy = dbrover_driver_duckdb::copy_to_parquet(&query, file, ParquetCompression::Zstd);
        let reader = CombineReader::start(
            workspace,
            &copy,
            self.values,
            DuckDbQuery::Combine,
            self.limits(),
        )
        .await
        .map_err(|error| error.message.clone())?;
        reader
            .finish()
            .await
            .map(drop)
            .map_err(|error| error.message.clone())
    }

    /// Makes the result of a report used as a source, in the table the report that reads it
    /// looks for, from its own combine query over its own tables (RS9c).
    ///
    /// Its `search_path` names its schema, so the query reads its sources as its file writes
    /// them, and is put back afterwards, so the report that reads it finds its own. The
    /// statement runs under the outer report's combine limits, which bound the one DuckDB they
    /// share; the inner report's own `[combine]` limits aren't read.
    async fn make_result(
        &self,
        report: &Composed<'_>,
        into: &WorkspaceTable,
        workspace: &mut Workspace,
        events: &mut dyn FnMut(RunEvent),
    ) -> Result<(), RunFailure> {
        let step = Step::Source {
            id: report.path.join(" › "),
        };
        let failed = |error: &DriverError| RunFailure::new(step.clone(), describe_error(error));
        let schema = report.schema();
        workspace
            .set_search_path(schema.as_deref())
            .await
            .map_err(|error| failed(&error))?;
        let started = Instant::now();
        let query = format!(
            "CREATE TABLE {} AS {}",
            into.qualified(),
            report.combined.combine.query.sql()
        );
        let made = async {
            let reader = CombineReader::start(
                workspace,
                &query,
                &report.values,
                DuckDbQuery::Combine,
                self.limits(),
            )
            .await?;
            reader.finish().await
        }
        .await;
        // Put it back whether the statement ran or not: the report that reads this one, and
        // the outer combine, read their own tables.
        workspace
            .set_search_path(None)
            .await
            .map_err(|error| failed(&error))?;
        let rows = made.map_err(|error| error.at(step.clone()))?;
        if let Some(source) = report.source {
            self.keep_result(workspace, source, into, &report.values)
                .await;
        }
        events(RunEvent::SourceDone {
            id: report.path.join(" › "),
            rows: rows.unwrap_or_default(),
            bytes: 0,
            first_row_ms: None,
            elapsed_ms: millis(started.elapsed()),
            table: Vec::new(),
            warnings: Vec::new(),
            batches: None,
            parts: None,
            phases: None,
            scanned_bytes: None,
            handed_over: None,
            reused_ms: None,
        });
        Ok(())
    }

    /// The tables of the sources an earlier run loaded into the workspace this one resumes,
    /// each said done with what it read then; nothing for a run that resumes none.
    fn resume_sources(
        &self,
        report: &Composed<'_>,
        events: &mut dyn FnMut(RunEvent),
    ) -> Result<Vec<LoadedTable>, RunFailure> {
        let Some(checkpoints) = self.checkpoints.filter(|kept| kept.has_sources()) else {
            return Ok(Vec::new());
        };
        let kept: Vec<(SourceKept, &str)> = report
            .combined
            .sources
            .iter()
            .filter_map(|source| match source {
                PlannedSource::Query(source) => checkpoints
                    .source(&report.step_id(&source.id))
                    .map(|kept| (kept, source.id.as_str())),
                // None of them loads rows, so none is a checkpoint a resumed run skips.
                PlannedSource::Attached(_) | PlannedSource::Lake(_) | PlannedSource::Report(_) => {
                    None
                }
            })
            .collect();
        let tables: Vec<LoadedTable> = kept
            .iter()
            .map(|(source, id)| LoadedTable::from_kept(source, report.table(id)))
            .collect();
        events(RunEvent::Resumed(Resumed {
            ranges: Vec::new(),
            sources: kept.iter().map(|(source, _)| source.id.clone()).collect(),
            of: 0,
            rows: 0,
            bytes: 0,
            files: Vec::new(),
        }));
        for (source, _) in &kept {
            events(kept_source_done(source));
        }
        Ok(tables)
    }

    /// Loads `source` as [`Combining::load`] does, after dropping what an earlier run left of
    /// its table, and records it once loaded.
    async fn load_anew(
        &self,
        source: Loaded<'_, '_>,
        workspace: &Workspace,
        loaded: &[LoadedTable],
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<LoadedTable, RunFailure> {
        let step = || Step::Source {
            id: source.step_id(),
        };
        if self.checkpoints.is_some_and(Checkpoints::has_sources) {
            workspace
                .drop_table(&source.table)
                .await
                .map_err(|error| RunFailure::new(step(), describe_error(&error)))?;
        }
        let (table, read) = self.load(source, workspace, loaded, events).await?;
        if let Some(checkpoints) = self.checkpoints {
            checkpoints.source_done(table.kept(read));
        }
        Ok(table)
    }

    /// Reads `source` into its table under its caps, a batch of its lookup's values at a
    /// time if it has one, then types its columns; returns the table and what it read.
    async fn load(
        &self,
        source: Loaded<'_, '_>,
        workspace: &Workspace,
        loaded: &[LoadedTable],
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(LoadedTable, Count), RunFailure> {
        let opened = Instant::now();
        let into = source.table.clone();
        let step_id = source.step_id();
        let report = source.report;
        let source = source.source;
        let mut loading = Loading {
            source,
            into,
            step_id,
            limits: self.limits(),
            opened,
            table: None,
            read: Count::default(),
            spent: Spent::default(),
            clock: ProgressClock::new(),
            scanned: None,
            at: At::Whole,
            handed_over: None,
        };
        let statements =
            Statements::loaded(report.plan, &report.values, source, Some(self.handoffs));
        let deadline = opened + source.limits.timeout;
        let sent = match &source.split {
            PlannedSplit::Whole => {
                let statement = statements.whole(opened);
                loading
                    .fill(self.opener, &statement, workspace, events)
                    .await?;
                Sent::Whole
            }
            PlannedSplit::Lookup(lookup) => {
                let (values, reading) = timed(lookup_values(lookup, workspace, loaded)).await;
                loading.spent.load += reading;
                let values = values.map_err(|error| error.at(loading.step()))?;
                let batches: Vec<&[Literal]> = values.chunks(lookup.batch_size.get()).collect();
                let of = batches.len() as u64;
                for (index, batch) in batches.into_iter().enumerate() {
                    let statement = statements.batch(batch, deadline);
                    loading.at = At::Batch(Batch {
                        number: index as u64 + 1,
                        of,
                    });
                    loading
                        .fill(self.opener, &statement, workspace, events)
                        .await?;
                }
                if of == 0 {
                    loading
                        .load_nothing(&statements, deadline, self.opener, workspace, events)
                        .await?;
                }
                Sent::Batches(of)
            }
            PlannedSplit::Partition(partition) => {
                let ranges = report.values.ranges.get(&source.id).ok_or_else(|| {
                    RunFailure::new(loading.step(), "the run's values have no ranges for it")
                })?;
                let reading = ranges::Ranges {
                    statements: &statements,
                    ranges,
                    partition,
                };
                reading
                    .load(&mut loading, self.opener, workspace, events)
                    .await?;
                Sent::Parts(ranges.ranges.len() as u64)
            }
        };
        loading.finish(sent, events).await
    }
}

/// Removes the run's DuckDB's file and spill from `work`, once the database is closed, so a
/// partitioned run's next range starts with an empty one and the run's disk holds one range's.
/// What can't be removed is left for the work folder's own removal when the run ends.
fn remove_workspace(work: &Path) {
    let file = work.join(WORKSPACE_FILE);
    let mut wal = file.clone().into_os_string();
    wal.push(".wal");
    for leftover in [file, PathBuf::from(wal)] {
        if let Err(error) = std::fs::remove_file(&leftover)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!("could not remove {}: {error}", leftover.display());
        }
    }
    let _ = std::fs::remove_dir_all(work.join(SPILL_FOLDER));
}

/// The event that says a source an earlier run loaded is done: its numbers as that run read
/// them, and no time of this run's.
fn kept_source_done(source: &SourceKept) -> RunEvent {
    RunEvent::SourceDone {
        id: source.id.clone(),
        rows: source.rows,
        bytes: source.read,
        first_row_ms: None,
        elapsed_ms: 0,
        table: source
            .columns
            .iter()
            .map(|column| LoadedColumn {
                name: column.name.clone(),
                type_name: column.duckdb_type.clone(),
                declared: false,
                bytes: None,
            })
            .collect(),
        warnings: Vec::new(),
        batches: None,
        parts: None,
        phases: None,
        scanned_bytes: None,
        handed_over: None,
        reused_ms: None,
    }
}

/// Makes `folder`, which DuckDB is to write a file in.
fn make_folder(folder: &Path) -> Result<(), RunFailure> {
    std::fs::create_dir_all(folder).map_err(|error| {
        RunFailure::new(
            Step::Combine,
            format!("could not make the folder {}: {error}", folder.display()),
        )
    })
}

/// A loaded source's table, for the lookups of the rounds after it: its name, and each
/// column's name and type.
struct LoadedTable {
    /// Where it is: in the schema of the report that reads it (RS9c).
    table: WorkspaceTable,
    /// What the run calls its step: the path, so two reports' sources of one id are two
    /// checkpoints.
    step_id: String,
    /// Each column with the type DuckDB holds it as, by name: the type the run's loading
    /// chose, or, for a source handed over through a bucket, the files' own, which can be a
    /// type nothing in a report's loading makes (RS7f).
    columns: Vec<KeptColumn>,
}

impl LoadedTable {
    /// The table of a source an earlier run loaded, as the manifest records it.
    fn from_kept(kept: &SourceKept, table: WorkspaceTable) -> Self {
        Self {
            table,
            step_id: kept.id.clone(),
            columns: kept.columns.clone(),
        }
    }

    /// What the manifest records of the table, once its source read `read`.
    fn kept(&self, read: Count) -> SourceKept {
        SourceKept {
            // The step's path, so two reports' sources of one id are two checkpoints (RS9c).
            id: self.step_id.clone(),
            rows: read.rows,
            read: read.bytes,
            columns: self.columns.clone(),
        }
    }
}

/// The distinct values of `lookup`'s column, in the table of the source it reads, as
/// literals of the column's type.
async fn lookup_values(
    lookup: &PlannedLookup,
    workspace: &Workspace,
    loaded: &[LoadedTable],
) -> Result<Vec<Literal>, SourceError> {
    let table = loaded
        .iter()
        .find(|table| table.table.name().eq_ignore_ascii_case(&lookup.from))
        .ok_or_else(|| {
            SourceError::new(format!(
                "lookup: {:?} wasn't loaded before this source",
                lookup.from
            ))
        })?;
    let column = table
        .columns
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case(&lookup.column))
        .ok_or_else(|| {
            let names: Vec<&str> = table
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect();
            SourceError::new(format!(
                "lookup: {:?} has no column {:?}; it has {}",
                lookup.from,
                lookup.column,
                names.join(", ")
            ))
        })?;
    // A column of a type no loading makes is one a handoff's files brought (RS7f), or a kept
    // workspace from a later DbRover: its values can't be written into a statement as
    // literals, which is what a lookup does with them.
    let column_type = column
        .duckdb_type
        .parse::<DuckDbType>()
        .map_err(|problem| {
            SourceError::new(format!(
                "lookup: {:?}'s column {:?} is a {}, which a lookup's values can't be made \
                 from: {problem}. Read the values with a cast in the source's own query",
                lookup.from, column.name, column.duckdb_type
            ))
        })?;
    let values = workspace
        .distinct_values(&table.table, &column.name)
        .await
        .map_err(|error| SourceError::new(describe_error(&error)))?;
    Ok(values
        .into_iter()
        .map(|value| lookup_literal(column_type, value))
        .collect())
}

/// A lake read in place, kept for as long as the combine runs: its view in the workspace, and
/// the bucket's sign-in, whose secret is renewed before the combine query starts.
struct ReadLake {
    id: String,
    view: LakeView,
    /// The lake's prefix without its bucket, which its bucket's requests are matched by.
    key_prefix: String,
    sign_in: BucketSignIn,
}

impl<O: SourceOpener> Combining<'_, '_, O> {
    /// Signs in to a lake's bucket, loads what DuckDB needs to read it, and makes the view the
    /// combine query reads it through (RS7). The prefix the lock must allow for it is
    /// [`LakeView::prefix`].
    async fn read_lake(
        &self,
        workspace: &mut Workspace,
        lake: &LakeSource,
        into: WorkspaceTable,
    ) -> Result<ReadLake, RunFailure> {
        let failed = |problem: String| {
            RunFailure::new(
                Step::Source {
                    id: lake.id.clone(),
                },
                format!("could not read the lake {:?}: {problem}", lake.id),
            )
        };
        // Only a bucket signing in with an access key has a password to find; every other
        // sign-in is the machine's own (C2).
        let password = if lake.storage.uses_password() {
            self.opener
                .password(&lake.data_source)
                .await
                .map_err(|error| failed(error.message))?
        } else {
            None
        };
        // DuckDB holds no secret for a provider until that provider's own extension is
        // loaded, so the extensions come first of all.
        workspace
            .load_lake_extensions(&lake.storage, lake.format)
            .await
            .map_err(|error| failed(describe_error(&error)))?;
        let mut sign_in = BucketSignIn::new(lake.storage.clone(), password);
        workspace
            .sign_in_to_bucket(&mut sign_in)
            .await
            .map_err(|error| {
                failed(format!(
                    "could not sign in to {}: {}",
                    lake.storage,
                    describe_error(&error)
                ))
            })?;
        // An Iceberg table is read from a metadata file, which DuckDB won't guess at: the
        // newest one is found by listing the table's metadata folder, as the explorer does.
        let read_from = match lake.format {
            LakeFormat::Iceberg => sign_in
                .iceberg_metadata(&lake.path)
                .await
                .map_err(|error| failed(describe_error(&error)))?,
            LakeFormat::Parquet | LakeFormat::Csv | LakeFormat::Json | LakeFormat::Delta => {
                lake.path.url().to_owned()
            }
        };
        let view = LakeView::new(
            into,
            &lake.storage,
            &lake.path,
            lake.format,
            &read_from,
            lake.hive_partitioning,
        );
        check_lake_cost(workspace, lake).await?;
        workspace
            .read_lake(&view)
            .await
            .map_err(|error| failed(describe_error(&error)))?;
        Ok(ReadLake {
            id: lake.id.clone(),
            key_prefix: lake.key_prefix(),
            view,
            sign_in,
        })
    }
}

/// Refuses a lake whose files hold more than the data source's cost guard allows: nobody can
/// be asked during a run, as for a source's statement (RS7d).
///
/// The bytes come from the files' Parquet footers, so they bound the read from above: DuckDB
/// reads less where its filters and columns let it skip part of the lake. A lake DuckDB has no
/// footers to read (a CSV or JSON one) isn't judged, and a log line says so.
async fn check_lake_cost(workspace: &mut Workspace, lake: &LakeSource) -> Result<(), RunFailure> {
    let Some(guard) = lake
        .data_source
        .config
        .cost_guard()
        .filter(CostGuard::is_on)
    else {
        return Ok(());
    };
    let bytes = workspace
        .lake_bytes(&lake.path, lake.format)
        .await
        .map_err(|error| {
            RunFailure::new(
                Step::Source {
                    id: lake.id.clone(),
                },
                format!(
                    "could not read the lake {:?} to judge what it holds: {}",
                    lake.id,
                    describe_error(&error)
                ),
            )
        })?;
    let Some(bytes) = bytes else {
        tracing::info!(
            "the {} lake {:?} has no footers to read, so its size isn't judged",
            lake.format,
            lake.id
        );
        return Ok(());
    };
    let estimate = CostEstimate::Bytes(ScanEstimate::at_most(bytes));
    if guard.holds(&estimate, ReadPace::Whole) {
        return Err(RunFailure::new(
            Step::Source {
                id: lake.id.clone(),
            },
            format!(
                "the lake {:?} holds {bytes}, and {:?} refuses a lake above {}: filter it in \
                 the [combine] query by a partition the lake is laid out by, name a narrower \
                 path, or raise the data source's limit",
                lake.id,
                lake.data_source.name,
                guard
                    .ask_above_bytes
                    .map_or_else(|| "its limit".to_owned(), |limit| limit.to_string())
            ),
        ));
    }
    Ok(())
}

async fn attach(workspace: &Workspace, source: &AttachedSource) -> Result<(), RunFailure> {
    workspace.attach(&source.id, &source.path).await.map_err(|error| {
        let message = describe_error(&error);
        // DuckDB locks a file one way at a time between processes, and the app may have it
        // open for writing.
        let hint = if message.contains("lock") {
            ". DuckDB opens a file for writing in one process at a time: if DbRover has it open \
             for writing, make the data source read-only, or disconnect it"
        } else {
            ""
        };
        RunFailure::new(
            Step::Source {
                id: source.id.clone(),
            },
            format!(
                "could not attach {} ({:?}): {message}{hint}",
                source.path.display(),
                source.data_source.name
            ),
        )
    })
}

/// A source's table being loaded, as text, the types its values say, and the bytes each
/// column's values took.
struct Table {
    loader: TableLoader,
    typer: ColumnTyper,
    column_bytes: Vec<u64>,
}

impl Table {
    /// Sees `part`'s text values for its columns' types, counts its bytes, and appends it.
    async fn append(&mut self, part: SourcePart) -> Result<(), DriverError> {
        let added = part.column_bytes(self.column_bytes.len());
        for (bytes, more) in self.column_bytes.iter_mut().zip(added) {
            *bytes += more;
        }
        match part {
            SourcePart::Rows(page) => {
                self.typer.see(&page);
                self.loader.append(page).await
            }
            SourcePart::Batch(batch) => {
                self.typer.see_batch(&batch);
                self.loader.append_batch(batch).await
            }
        }
    }
}

/// The time a source being loaded spent outside waiting for its rows, which its [`Count`]
/// holds.
#[derive(Default)]
struct Spent {
    connect: Duration,
    /// Each statement's start, up to its cursor; its wait for the first page is in `Count`.
    start: Duration,
    /// Held by DuckDB: waiting for room in the queue, and for the last pages to be appended,
    /// and reading a lookup's values. Appending itself overlaps receiving, so it isn't counted.
    load: Duration,
    typing: Duration,
}

/// A source being loaded into its table.
struct Loading<'a> {
    source: &'a QuerySource,
    /// The table it fills: in the schema of the report that reads it (RS9c).
    into: WorkspaceTable,
    /// What the run's progress calls it: `valued › tx` for a source of a report used as a
    /// source, and the source's id for one of the report being run.
    step_id: String,
    limits: &'a CombineLimits,
    /// When the source started to be opened, for the time to its first row.
    opened: Instant,
    /// Made when the first statement's columns are known.
    table: Option<Table>,
    /// What every statement has returned so far, which the source's caps hold.
    read: Count,
    spent: Spent,
    clock: ProgressClock,
    /// The bytes its engine scanned for every statement so far, where it says.
    scanned: Option<u64>,
    /// Which of its statements it is sending.
    at: At,
    /// What its handed-over files became, for a source with a `handoff` (RS7f): DuckDB made
    /// the table itself, so there is no loader and nothing to type.
    handed_over: Option<HandedOverLoad>,
}

/// A source loaded from the files its engine wrote (RS7f): the table DuckDB made from them,
/// and what reading them cost.
struct HandedOverLoad {
    loaded: HandoffLoaded,
    /// The files read, and the bytes they held, which the bucket's listing gave.
    files: u64,
    bytes: u64,
}

/// Which of a source's statements is being read, for its progress.
#[derive(Debug, Clone, Copy)]
enum At {
    Whole,
    Batch(Batch),
    /// Ranges of a partitioned source, several of which may be read at once.
    Parts(Parts),
}

impl At {
    fn batch(self) -> Option<Batch> {
        match self {
            Self::Batch(batch) => Some(batch),
            Self::Whole | Self::Parts(_) => None,
        }
    }

    fn parts(self) -> Option<Parts> {
        match self {
            Self::Parts(parts) => Some(parts),
            Self::Whole | Self::Batch(_) => None,
        }
    }
}

/// What statements a loaded source sent.
enum Sent {
    Whole,
    /// One a batch of its lookup's values.
    Batches(u64),
    /// One a range of its partition.
    Parts(u64),
}

/// The files a handoff's load reads: their whole addresses, and the bytes they hold.
struct HandedFiles {
    addresses: Vec<String>,
    bytes: u64,
}

/// What an engine wrote for a handoff: the addresses it named, or everything in the folder it
/// was given, which the run owns, so everything in it is the engine's own output.
///
/// A key of no bytes is left out: it is the folder marker some tools write, which no reader
/// takes.
async fn handed_files(
    sign_in: &mut BucketSignIn,
    handed: &HandedOver,
    folder: Option<&HandoffFolder>,
    storage: &ObjectStorage,
) -> Result<HandedFiles, String> {
    match (&handed.files, folder) {
        (HandoffFiles::Named { read, .. }, _) => {
            // The engine named where it writes its own results, which the report didn't: a
            // location outside the handoff's bucket is one the run can neither read nor
            // delete, so it says so rather than failing in DuckDB's words.
            let bucket = format!("{}/", storage.url());
            if let Some(outside) = read.iter().find(|file| !file.starts_with(&bucket)) {
                return Err(format!(
                    "it wrote {outside}, which isn't in {}: name the data source of the bucket \
                     it writes its results in, or give the handoff a prefix of its own",
                    storage.url()
                ));
            }
            Ok(HandedFiles {
                addresses: read.clone(),
                // Nothing lists a file the engine named: its size would be a request each, and
                // what a handoff's bytes say is what the files held, not what they cost.
                bytes: 0,
            })
        }
        (HandoffFiles::InFolder, Some(folder)) => {
            let found = sign_in
                .files_under(folder.key())
                .await
                .map_err(|error| describe_error(&error))?;
            let bytes = found.iter().map(|file| file.size).sum();
            let addresses = found
                .iter()
                .filter(|file| file.size > 0)
                .map(|file| format!("{}/{}", storage.url(), file.key))
                .collect();
            Ok(HandedFiles { addresses, bytes })
        }
        (HandoffFiles::InFolder, None) => {
            Err("it wrote into a folder it was never given".to_owned())
        }
    }
}

/// Deletes the objects at `addresses`, each a whole address in `storage`.
async fn remove_keys(
    sign_in: &mut BucketSignIn,
    addresses: &[String],
    storage: &ObjectStorage,
) -> Result<(), DriverError> {
    let bucket = format!("{}/", storage.url());
    for address in addresses {
        let key = address.strip_prefix(&bucket).unwrap_or(address);
        sign_in.remove_key(key).await?;
    }
    Ok(())
}

/// Appends the parts `queued` to `table` as they come, until the reader ends; and when it was
/// done. Stops at the first part DuckDB refuses, which closes the queue.
async fn append_queued(
    table: &mut Table,
    mut queued: mpsc::Receiver<SourcePart>,
) -> (Result<(), DriverError>, Instant) {
    let mut outcome = Ok(());
    while let Some(part) = queued.recv().await {
        if let Err(error) = table.append(part).await {
            outcome = Err(error);
            break;
        }
    }
    queued.close();
    (outcome, Instant::now())
}

/// What the profiler measured of the last statement, or nothing when it can't be read.
pub(crate) async fn read_profile(workspace: &Workspace) -> WorkspaceProfile {
    workspace.profile().await.unwrap_or_else(|error| {
        tracing::warn!(
            "could not read the run's DuckDB's profile: {}",
            describe_error(&error)
        );
        WorkspaceProfile::default()
    })
}

/// A step's progress: what it has read since `opened`, and what it was busy with.
pub(crate) fn progress(step: &Step, read: &Count, opened: Instant, busy: Busy) -> RunEvent {
    progress_at(step, read, opened, At::Whole, busy)
}

/// A source's progress, on the statement `at`.
fn progress_at(step: &Step, read: &Count, opened: Instant, at: At, busy: Busy) -> RunEvent {
    RunEvent::Progress {
        step: step.clone(),
        rows: read.rows,
        bytes: read.bytes,
        elapsed_ms: millis(opened.elapsed()),
        batch: at.batch(),
        parts: at.parts(),
        busy: Some(busy),
    }
}

/// One source about to load, with the report it belongs to: its table and its step's name
/// come from that report's place in the run (RS9c).
struct Loaded<'a, 'r> {
    source: &'a QuerySource,
    report: &'a Composed<'r>,
    table: WorkspaceTable,
}

impl Loaded<'_, '_> {
    fn step_id(&self) -> String {
        self.report.step_id(&self.source.id)
    }
}

impl Loading<'_> {
    fn step(&self) -> Step {
        Step::Source {
            id: self.step_id.clone(),
        }
    }

    fn duckdb_failure(&self, error: &DriverError) -> RunFailure {
        duckdb_failure(error, self.limits).at(self.step())
    }

    /// Runs `statement` and appends its rows to the table, a page at a time, watching the
    /// source's caps.
    async fn fill<O: SourceOpener>(
        &mut self,
        opener: &O,
        statement: &SourceStatement<'_>,
        workspace: &Workspace,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(), RunFailure> {
        let step = self.step();
        let at = self.at;
        let (reader, opening_time) = waiting(Box::pin(Shaped::open(opener, statement)), |so_far| {
            let busy = self.clock.lap(so_far);
            (events.borrow_mut())(progress_at(&step, &self.read, self.opened, at, busy));
        })
        .await;
        self.clock.waited(opening_time);
        let mut reader = reader.map_err(|error| error.at(step))?;
        let opening = reader.opening();
        self.spent.connect += opening.connect;
        self.spent.start += opening.start;
        let filled = match reader.handed_over().cloned() {
            // The engine wrote files instead of answering with rows, so DuckDB reads them
            // into the source's table and the run deletes them (RS7f).
            Some(handed) => {
                self.load_handed_over(opener, statement, &handed, workspace, events)
                    .await
            }
            None => self.fill_from(&mut reader, workspace, events).await,
        };
        if let Some(scanned) = reader.scanned() {
            self.scanned = Some(self.scanned.unwrap_or(0).saturating_add(scanned));
        }
        reader.close().await;
        filled
    }

    /// Reads the files `handed` names into the source's table, and deletes them (RS7f).
    ///
    /// The bucket is the handoff's own data source's, signed in to here as a lake's is: the
    /// extensions DuckDB needs for the provider, then its temporary secret, then the read.
    /// It all happens before [`Workspace::lock`], where every source is loaded, so the
    /// bucket needs no prefix in `allowed_directories`.
    async fn load_handed_over<O: SourceOpener>(
        &mut self,
        opener: &O,
        statement: &SourceStatement<'_>,
        handed: &HandedOver,
        workspace: &Workspace,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(), RunFailure> {
        let step = self.step();
        let failed = |problem: String| RunFailure::new(step.clone(), problem);
        let (Some(planned), Some(handoff)) = (&self.source.handoff, &statement.handoff) else {
            return Err(failed(
                "the engine wrote its result as files for a source that asked for none".to_owned(),
            ));
        };
        let password = if planned.storage.uses_password() {
            opener
                .password(&planned.data_source)
                .await
                .map_err(|error| failed(error.message))?
        } else {
            None
        };
        let mut sign_in = BucketSignIn::new(planned.storage.clone(), password);
        let folder = match handoff {
            Handoff::IntoFolder(folder) => Some(folder),
            Handoff::OwnResult => None,
        };
        let found = handed_files(&mut sign_in, handed, folder, &planned.storage)
            .await
            .map_err(|problem| {
                failed(format!(
                    "the files {} wrote couldn't be found: {problem}",
                    planned.data_source.name
                ))
            })?;
        // The bucket's own extension has to hold the secret's type before the secret is made,
        // and both before anything reads a file (ADR 0008, as built).
        workspace
            .load_lake_extensions(&planned.storage, handed.format)
            .await
            .map_err(|error| failed(describe_error(&error)))?;
        workspace
            .sign_in_to_bucket(&mut sign_in)
            .await
            .map_err(|error| {
                failed(format!(
                    "could not sign in to {}: {}",
                    planned.storage,
                    describe_error(&error)
                ))
            })?;
        let at = self.at;
        let read = &self.read;
        let opened = self.opened;
        let clock = &mut self.clock;
        let (loaded, taken) = waiting(
            Box::pin(workspace.load_handoff(&self.into, &found.addresses, handed.format)),
            |so_far| {
                let busy = clock.lap_loading(so_far);
                (events.borrow_mut())(progress_at(&step, read, opened, at, busy));
            },
        )
        .await;
        self.spent.load += taken;
        self.clock.loaded(taken);
        let loaded = loaded.map_err(|error| self.duckdb_failure(&error))?;
        self.read.rows = loaded.rows;
        self.read.bytes = found.bytes;
        self.read
            .first_row
            .get_or_insert_with(|| self.opened.elapsed());
        self.handed_over = Some(HandedOverLoad {
            loaded,
            files: found.addresses.len() as u64,
            bytes: found.bytes,
        });
        // The rows are in the run's own database now, so the files have done their job. A
        // delete that fails leaves them for the next run of this report, and for the
        // lifecycle rule the manual asks for, rather than failing a run that has its rows.
        let removed = match (&handed.files, folder) {
            (HandoffFiles::Named { remove, .. }, _) => {
                remove_keys(&mut sign_in, remove, &planned.storage).await
            }
            (HandoffFiles::InFolder, Some(folder)) => {
                sign_in.remove_under(folder.key()).await.map(|_| ())
            }
            (HandoffFiles::InFolder, None) => Ok(()),
        };
        if let Err(error) = removed {
            tracing::warn!(
                "could not delete the handed-over files of the source {:?}: {}",
                self.source.id,
                describe_error(&error)
            );
        }
        Ok(())
    }

    /// Reads `reader`'s pages into the table: the reader and the loader take turns at the
    /// source's task, joined by a queue of [`QUEUE_PAGES`], so the next page is received while
    /// DuckDB appends the last one.
    async fn fill_from<R: SourceReader>(
        &mut self,
        reader: &mut R,
        workspace: &Workspace,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(), RunFailure> {
        let schema = reader.batch_schema();
        let mut table = self
            .take_table(reader.columns(), schema.as_deref(), workspace)
            .await?;
        let (pages, queued) = mpsc::channel(QUEUE_PAGES);
        let ((read, read_ended), (appended, appended_ended)) = tokio::join!(
            self.read_into(reader, pages, events),
            append_queued(&mut table, queued),
        );
        // What DuckDB still had to append once the last page was read held the source too.
        self.spent.load += appended_ended.saturating_duration_since(read_ended);
        self.table = Some(table);
        // A loader that failed stopped the reader: its error is the one to tell.
        appended.map_err(|error| self.duckdb_failure(&error))?;
        read
    }

    /// Reads `reader`'s pages into `pages`, under the source's caps, until it has no more,
    /// fails, or the loader stops; and when it ended.
    async fn read_into<R: SourceReader>(
        &mut self,
        reader: &mut R,
        pages: mpsc::Sender<SourcePart>,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> (Result<(), RunFailure>, Instant) {
        let step = self.step();
        let at = self.at;
        let mut rows_seen = false;
        let outcome = loop {
            let (page, waited) = waiting(reader.next_part(), |so_far| {
                let busy = self.clock.lap(so_far);
                (events.borrow_mut())(progress_at(&step, &self.read, self.opened, at, busy));
            })
            .await;
            self.read.waited(waited, !rows_seen);
            self.clock.waited(waited);
            let page = match page {
                Ok(Some(page)) => page,
                Ok(None) => break Ok(()),
                Err(error) => break Err(error.at(step.clone())),
            };
            if page.is_empty() {
                continue;
            }
            rows_seen = true;
            self.read
                .first_row
                .get_or_insert_with(|| self.opened.elapsed());
            self.read.add_part(&page);
            if let Err(failure) = self.read.check(self.source) {
                break Err(failure);
            }
            let (sent, room) = waiting(pages.send(page), |so_far| {
                let busy = self.clock.lap_loading(so_far);
                (events.borrow_mut())(progress_at(&step, &self.read, self.opened, at, busy));
            })
            .await;
            self.spent.load += room;
            self.clock.loaded(room);
            if sent.is_err() {
                // The loader failed, and says why.
                break Ok(());
            }
            if self.clock.due() {
                let busy = self.clock.lap(Duration::ZERO);
                (events.borrow_mut())(progress_at(&step, &self.read, self.opened, at, busy));
            }
        };
        (outcome, Instant::now())
    }

    /// What a source handed over through a bucket ends with (RS7f): DuckDB made its table
    /// from the files, so there is no loader to finish and no column to type, and the columns
    /// are the ones the files brought.
    fn finish_handed_over(
        self,
        handed: HandedOverLoad,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> (LoadedTable, Count) {
        let columns: Vec<KeptColumn> = handed
            .loaded
            .columns
            .into_iter()
            .map(|(name, duckdb_type)| KeptColumn { name, duckdb_type })
            .collect();
        let spent = &self.spent;
        (events.borrow_mut())(RunEvent::SourceDone {
            id: self.step_id.clone(),
            rows: self.read.rows,
            bytes: self.read.bytes,
            first_row_ms: self.read.first_row.map(millis),
            elapsed_ms: millis(self.opened.elapsed()),
            table: columns
                .iter()
                .map(|column| LoadedColumn {
                    name: column.name.clone(),
                    type_name: column.duckdb_type.clone(),
                    declared: false,
                    // The files' own columns: nothing in the run wrote them, so nothing
                    // measured what each holds.
                    bytes: None,
                })
                .collect(),
            warnings: Vec::new(),
            batches: None,
            parts: None,
            phases: Some(SourcePhases {
                connect_ms: millis(spent.connect),
                server_ms: millis(spent.start),
                // The engine wrote the files; reading them is DuckDB's load, not a transfer
                // the run paged.
                transfer_ms: 0,
                load_ms: millis(spent.load),
                type_ms: 0,
            }),
            scanned_bytes: self.scanned,
            handed_over: Some(HandedOverFiles {
                files: handed.files,
                bytes: handed.bytes,
            }),
            reused_ms: None,
        });
        let read = Count {
            rows: self.read.rows,
            bytes: self.read.bytes,
            ..Count::default()
        };
        let table = LoadedTable {
            table: self.into.clone(),
            step_id: self.step_id.clone(),
            columns,
        };
        (table, read)
    }

    /// The table for `columns`: made for the first statement, whose columns a later batch's
    /// must be, typed from `schema` when the source reads Arrow batches of it. The caller puts
    /// it back once its rows are in.
    async fn take_table(
        &mut self,
        columns: &[ExportColumn],
        schema: Option<&Schema>,
        workspace: &Workspace,
    ) -> Result<Table, RunFailure> {
        let names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
        if let Some(table) = self.table.take() {
            let first: Vec<&str> = table.typer.names().collect();
            if first == names {
                return Ok(table);
            }
            let number = self.at.batch().map_or(1, |batch| batch.number);
            return Err(RunFailure::new(
                self.step(),
                format!(
                    "batch {number} returned the columns {}, unlike the first batch's {}",
                    names.join(", "),
                    first.join(", ")
                ),
            ));
        }
        let drivers = DriverTypes::of(self.source.data_source.config.driver());
        let typer = match schema {
            Some(schema) => ColumnTyper::for_batches(columns, schema, drivers),
            None => ColumnTyper::new(columns, self.source.columns.as_deref(), drivers),
        };
        let load_columns: Vec<LoadColumn> = names
            .into_iter()
            .zip(typer.known())
            .map(|(name, known)| LoadColumn {
                name,
                known: *known,
            })
            .collect();
        let loader = workspace
            .loader(&self.into, &load_columns)
            .await
            .map_err(|error| self.duckdb_failure(&error))?;
        Ok(Table {
            loader,
            typer,
            column_bytes: vec![0; columns.len()],
        })
    }

    /// For a lookup that found no value: a source with declared columns sends nothing and
    /// gets an empty table of them; one without runs once with a list of one `NULL`, which
    /// matches nothing, for its result's columns.
    async fn load_nothing<O: SourceOpener>(
        &mut self,
        statements: &Statements<'_>,
        deadline: Instant,
        opener: &O,
        workspace: &Workspace,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(), RunFailure> {
        match &self.source.columns {
            Some(declared) => {
                let columns: Vec<ExportColumn> = declared
                    .iter()
                    .map(|column| ExportColumn {
                        name: column.name.clone(),
                        type_name: None,
                        kind: column.column_type.kind(),
                        excel_format: None,
                    })
                    .collect();
                let table = self.take_table(&columns, None, workspace).await?;
                self.table = Some(table);
                Ok(())
            }
            None => {
                let statement = statements.batch(&[Literal::Null], deadline);
                self.at = At::Whole;
                self.fill(opener, &statement, workspace, events).await
            }
        }
    }

    /// Types the table's columns, makes it, and says so; `batches` is how many statements
    /// a lookup sent.
    async fn finish(
        mut self,
        sent: Sent,
        events: &RefCell<&mut (dyn FnMut(RunEvent) + '_)>,
    ) -> Result<(LoadedTable, Count), RunFailure> {
        if let Some(handed) = self.handed_over.take() {
            return Ok(self.finish_handed_over(handed, events));
        }
        let step = self.step();
        let Some(Table {
            loader,
            typer,
            column_bytes,
        }) = self.table
        else {
            return Err(RunFailure::new(step, "the source sent no statement"));
        };
        let (types, warnings) = typer.types();
        let declared = typer.is_declared();
        let columns: Vec<KeptColumn> = typer
            .names()
            .zip(types.iter())
            .map(|(name, column_type)| KeptColumn {
                name: name.to_owned(),
                duckdb_type: column_type.to_string(),
            })
            .collect();
        let (typed, typing) = timed(loader.finish(&types)).await;
        typed.map_err(|error| duckdb_failure(&error, self.limits).at(step))?;
        let spent = &self.spent;
        let phases = SourcePhases {
            connect_ms: millis(spent.connect),
            server_ms: millis(spent.start + self.read.waited_first),
            transfer_ms: millis(self.read.waited_rest),
            load_ms: millis(spent.load),
            type_ms: millis(spent.typing + typing),
        };
        (events.borrow_mut())(RunEvent::SourceDone {
            id: self.step_id.clone(),
            rows: self.read.rows,
            bytes: self.read.bytes,
            first_row_ms: self.read.first_row.map(millis),
            elapsed_ms: millis(self.opened.elapsed()),
            table: columns
                .iter()
                .zip(&column_bytes)
                .map(|(column, bytes)| LoadedColumn {
                    name: column.name.clone(),
                    type_name: column.duckdb_type.clone(),
                    declared,
                    bytes: Some(*bytes),
                })
                .collect(),
            warnings,
            batches: match sent {
                Sent::Batches(batches) => Some(batches),
                Sent::Whole | Sent::Parts(_) => None,
            },
            parts: match sent {
                Sent::Parts(parts) => Some(parts),
                Sent::Whole | Sent::Batches(_) => None,
            },
            phases: Some(phases),
            scanned_bytes: self.scanned,
            handed_over: None,
            reused_ms: None,
        });
        let read = Count {
            rows: self.read.rows,
            bytes: self.read.bytes,
            ..Count::default()
        };
        let table = LoadedTable {
            table: self.into.clone(),
            step_id: self.step_id.clone(),
            columns,
        };
        Ok((table, read))
    }
}

/// What DuckDB's error says, naming the limit when it hit one of the combine step's.
fn duckdb_failure(error: &DriverError, limits: &CombineLimits) -> SourceError {
    let message = describe_error(error);
    // DuckDB's own advice follows, to `SET memory_limit` and the like, which a report can't:
    // the advice below says what can.
    let message = message
        .split_once("\n\nPossible solutions")
        .map_or(message.as_str(), |(cause, _)| cause)
        .trim_end()
        .to_owned();
    let (key, value, advice) = match WorkspaceLimit::of(error) {
        Some(WorkspaceLimit::Memory) => (
            "memory_limit",
            limits.memory_limit.to_string(),
            "combine fewer rows, or raise memory_limit",
        ),
        Some(WorkspaceLimit::TempDirectory) => (
            "max_temp_size",
            limits.max_temp_size.to_string(),
            "combine fewer rows, or raise max_temp_size",
        ),
        None => return SourceError::new(message),
    };
    SourceError {
        message: format!("DuckDB ran past {value}, its {key}, and stopped: {message}; {advice}"),
        limit: Some(Box::new(LimitHit {
            key: key.to_owned(),
            value,
            raise_in: "[combine]".to_owned(),
            set_in: None,
        })),
    }
}

/// Which query of the run's DuckDB a [`CombineReader`] reads, which its timeout's message
/// names.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DuckDbQuery {
    Combine,
    /// A chart's own query, under the combine step's limits.
    Chart,
}

/// The rows of a query of the run's DuckDB, the combine step's or a chart's, read a page at a
/// time until its timeout.
pub(crate) struct CombineReader<'a> {
    cursor: Option<Box<dyn ResultCursor>>,
    columns: Vec<ExportColumn>,
    cancel: Arc<dyn CancelHandle>,
    deadline: Instant,
    query: DuckDbQuery,
    limits: &'a CombineLimits,
}

impl<'a> CombineReader<'a> {
    /// Starts `query` with `values` bound, in the locked-down workspace. It has the combine
    /// step's `timeout`, counted from now.
    pub(crate) async fn start(
        workspace: &mut Workspace,
        query: &str,
        values: &RunValues,
        query_of: DuckDbQuery,
        limits: &'a CombineLimits,
    ) -> Result<Self, SourceError> {
        let deadline = Instant::now() + limits.timeout;
        let prepared = dbrover_sql::bind(query, Dialect::DuckDb, &values.parameters)
            .map_err(|missing| SourceError::new(missing.to_string()))?;
        let connection = workspace.connection();
        let cancel = connection.cancel_handle();
        let started = async {
            connection
                .execute_bound(&prepared.sql, &prepared.values)
                .await
                .map_err(|error| duckdb_failure(&error, limits))
        };
        let cursor = watch(started, deadline, &cancel, query_of, limits).await?;
        let columns = cursor
            .columns()
            .iter()
            .map(|column| ExportColumn::new(column, Dialect::DuckDb))
            .collect();
        Ok(Self {
            cursor: Some(cursor),
            columns,
            cancel,
            deadline,
            query: query_of,
            limits,
        })
    }
}

impl CombineReader<'_> {
    /// Ends the statement: the rows a statement that changes or writes them says it did.
    pub(crate) async fn finish(self) -> Result<Option<u64>, SourceError> {
        let Some(cursor) = self.cursor else {
            return Ok(None);
        };
        cursor
            .finish()
            .await
            .map_err(|error| duckdb_failure(&error, self.limits))
    }
}

impl SourceReader for CombineReader<'_> {
    fn columns(&self) -> &[ExportColumn] {
        &self.columns
    }

    async fn next_page(&mut self) -> Result<Option<Vec<Row>>, SourceError> {
        let Some(cursor) = self.cursor.as_mut().filter(|cursor| !cursor.is_exhausted()) else {
            return Ok(None);
        };
        let limits = self.limits;
        let page = async {
            cursor
                .next_page(PAGE_ROWS)
                .await
                .map_err(|error| duckdb_failure(&error, limits))
        };
        let page = watch(page, self.deadline, &self.cancel, self.query, self.limits).await?;
        Ok((!page.is_empty()).then_some(page))
    }

    async fn close(self) {
        if let Some(cursor) = self.cursor
            && let Err(error) = cursor.finish().await
        {
            tracing::debug!("the combine query ended with: {}", describe_error(&error));
        }
    }
}

/// Runs `work` until `deadline`, then interrupts the query and fails with the combine step's
/// timeout as the limit hit.
async fn watch<T>(
    work: impl Future<Output = Result<T, SourceError>>,
    deadline: Instant,
    cancel: &Arc<dyn CancelHandle>,
    query: DuckDbQuery,
    limits: &CombineLimits,
) -> Result<T, SourceError> {
    let mut work = std::pin::pin!(work);
    let remaining = deadline.saturating_duration_since(Instant::now());
    tokio::select! {
        outcome = &mut work => return outcome,
        () = tokio::time::sleep(remaining) => {}
    }
    if let Err(error) = cancel.cancel().await {
        tracing::warn!(
            "could not interrupt the combine query: {}",
            describe_error(&error)
        );
    }
    // Whatever it returns now, it ended because of the interrupt.
    let _ = tokio::time::timeout(CANCEL_GRACE, work).await;
    let value = friendly(limits.timeout);
    let message = match query {
        DuckDbQuery::Combine => format!(
            "the combine query ran past {value}, its timeout, and was interrupted; combine fewer \
             rows, or raise timeout"
        ),
        DuckDbQuery::Chart => format!(
            "the chart's query ran past {value}, the combine step's timeout, and was \
             interrupted; read fewer rows in it, or raise timeout in [combine]"
        ),
    };
    Err(SourceError {
        message,
        limit: Some(Box::new(LimitHit {
            key: "timeout".to_owned(),
            value,
            raise_in: "[combine]".to_owned(),
            set_in: None,
        })),
    })
}

/// `10m`, `1h 30m`, as a report file writes a duration.
fn friendly(duration: Duration) -> String {
    match jiff::SignedDuration::try_from(duration) {
        Ok(duration) => format!("{duration:#}"),
        Err(_) => format!("{}s", duration.as_secs()),
    }
}
