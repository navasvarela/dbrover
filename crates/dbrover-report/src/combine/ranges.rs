//! A partitioned source's ranges (`docs/reports-at-scale.md`, RS4b and RS4c), each a statement
//! of its own, `parallel` at once, all feeding the source's one queue and table:
//!
//! 1. open the first range alone: its statement has the cost guard judge the whole period, so a
//!    source too costly to read is refused before any range runs, and its columns make the
//!    table;
//! 2. read it and the other ranges, at most `parallel` at once and starting in order, each
//!    range's pages going to the queue that one appender empties into the table;
//! 3. say each range's numbers as it ends, and the ranges done and running once a second.
//!
//! The readers and the appender are futures of the source's one task, so what they share (the
//! source's counts, its caps, its clock) is borrowed for a moment at a time, never across an
//! await.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use futures_util::{StreamExt, TryStreamExt, stream};

use super::{At, Loading, Table, progress_at};
use crate::declared::Shaped;
use crate::live::waiting;
use crate::run::{Count, millis, timed};
use crate::statement::Statements;
use crate::{
    CostCheck, LimitHit, LimitSetIn, PartDone, PartRange, Parts, PlannedSourcePartition,
    QUEUE_PAGES, RunEvent, RunFailure, SourceOpener, SourcePart, SourceRanges, SourceReader,
    grouped,
};
use dbrover_core::DriverError;
use dbrover_driver_duckdb::Workspace;
use tokio::sync::mpsc;

type Events<'e, 'm, 'f> = &'e RefCell<&'m mut (dyn FnMut(RunEvent) + 'f)>;

/// A partitioned source's ranges for a run.
pub(super) struct Ranges<'s, 'a> {
    pub(super) statements: &'s Statements<'a>,
    pub(super) ranges: &'s SourceRanges,
    pub(super) partition: &'s PlannedSourcePartition,
}

/// What the ranges being read share.
struct Shared<'l, 'a> {
    loading: &'l mut Loading<'a>,
    done: u64,
    running: u64,
}

impl Ranges<'_, '_> {
    /// Reads every range into the source's table: see the module.
    pub(super) async fn load<O: SourceOpener>(
        &self,
        loading: &mut Loading<'_>,
        opener: &O,
        workspace: &Workspace,
        events: Events<'_, '_, '_>,
    ) -> Result<(), RunFailure> {
        let Some(first_range) = self.ranges.ranges.first() else {
            return Err(RunFailure::new(
                loading.step(),
                "the source has no range to read",
            ));
        };
        let started = Instant::now();
        let step = loading.step();
        let first_statement = self.statement(first_range);
        let opened = waiting(Box::pin(Shaped::open(opener, &first_statement)), |so_far| {
            let busy = loading.clock.lap(so_far);
            let at = At::Parts(self.parts(0, 1));
            (events.borrow_mut())(progress_at(&step, &loading.read, loading.opened, at, busy));
        })
        .await;
        loading.clock.waited(opened.1);
        let first = opened.0.map_err(|error| error.at(step.clone()))?;
        let schema = first.batch_schema();
        let mut table = match loading
            .take_table(first.columns(), schema.as_deref(), workspace)
            .await
        {
            Ok(table) => table,
            Err(failure) => {
                first.close().await;
                return Err(failure);
            }
        };
        let names: Vec<String> = table.typer.names().map(str::to_owned).collect();
        let (pages, queued) = mpsc::channel(QUEUE_PAGES);
        let shared = RefCell::new(Shared {
            loading,
            done: 0,
            running: 0,
        });
        let reading = self.read_all(&shared, opener, (first, started), pages, events, &names);
        let appending = append_timed(&mut table, queued, &shared);
        let ((read, read_ended), (appended, appended_ended)) =
            waiting(async { tokio::join!(reading, appending) }, |_| {
                tick(&shared, &step, self.of(), events);
            })
            .await
            .0;
        let loading = shared.into_inner().loading;
        // What DuckDB still had to append once the last page was read held the source too.
        loading.spent.load += appended_ended.saturating_duration_since(read_ended);
        loading.table = Some(table);
        // A loader that failed stopped the readers: its error is the one to tell.
        appended.map_err(|error| loading.duckdb_failure(&error))?;
        read
    }

    fn of(&self) -> u64 {
        self.ranges.ranges.len() as u64
    }

    fn parts(&self, done: u64, running: u64) -> Parts {
        Parts {
            done,
            running,
            of: self.of(),
        }
    }

    /// The statement of `range`, its timeout counted from now; the first judges the whole
    /// period for the cost guard, and the others nothing.
    fn statement(&self, range: &PartRange) -> crate::SourceStatement<'_> {
        let kind = self.partition.partition.kind;
        let cost = if range.number == 1 {
            self.statements.whole_period(&self.ranges.whole, kind)
        } else {
            CostCheck::Judged
        };
        self.statements.range(range, kind, Instant::now(), cost)
    }

    /// Reads every range, `parallel` at once, the first from `first`, already open; and when
    /// the last ended.
    async fn read_all<O: SourceOpener>(
        &self,
        shared: &RefCell<Shared<'_, '_>>,
        opener: &O,
        first: (Shaped<O::Reader>, Instant),
        pages: mpsc::Sender<SourcePart>,
        events: Events<'_, '_, '_>,
        names: &[String],
    ) -> (Result<(), RunFailure>, Instant) {
        let mut first = Some(first);
        let outcome = stream::iter(&self.ranges.ranges)
            .map(|range| {
                let opened = first.take();
                let pages = pages.clone();
                async move {
                    let (reader, started) = match opened {
                        Some(opened) => opened,
                        None => (self.open(shared, opener, range).await?, Instant::now()),
                    };
                    self.read_range(shared, reader, range, started, &pages, events, names)
                        .await
                }
            })
            .buffer_unordered(self.partition.parallel().get())
            .try_collect::<Vec<()>>()
            .await;
        // The appender ends once every sender is gone.
        drop(pages);
        (outcome.map(drop), Instant::now())
    }

    /// Opens `range`'s statement, counting the time it took against the source.
    async fn open<O: SourceOpener>(
        &self,
        shared: &RefCell<Shared<'_, '_>>,
        opener: &O,
        range: &PartRange,
    ) -> Result<Shaped<O::Reader>, RunFailure> {
        let step = shared.borrow().loading.step();
        shared.borrow_mut().running += 1;
        let statement = self.statement(range);
        let (reader, _) = timed(Shaped::open(opener, &statement)).await;
        reader.map_err(|error| error.at(step))
    }

    /// Reads `range`'s pages into the queue, under the source's caps and `max_rows_per_part`,
    /// then says what the range read.
    #[allow(clippy::too_many_arguments)]
    async fn read_range<R: SourceReader>(
        &self,
        shared: &RefCell<Shared<'_, '_>>,
        mut reader: Shaped<R>,
        range: &PartRange,
        started: Instant,
        pages: &mpsc::Sender<SourcePart>,
        events: Events<'_, '_, '_>,
        names: &[String],
    ) -> Result<(), RunFailure> {
        let opening = reader.opening();
        {
            let mut shared = shared.borrow_mut();
            if range.number == 1 {
                shared.running += 1;
            }
            shared.loading.spent.connect += opening.connect;
            shared.loading.spent.start += opening.start;
        }
        let returned: Vec<&str> = reader
            .columns()
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        if returned != names {
            let step = shared.borrow().loading.step();
            let message = format!(
                "range {} returned the columns {}, unlike the first range's {}",
                range.number,
                returned.join(", "),
                names.join(", ")
            );
            reader.close().await;
            return Err(RunFailure::new(step, message));
        }
        let read = self
            .read_pages(shared, &mut reader, range, started, pages)
            .await;
        let scanned = reader.scanned();
        reader.close().await;
        let read = read?;
        let mut shared = shared.borrow_mut();
        shared.running = shared.running.saturating_sub(1);
        shared.done += 1;
        let loading = &mut shared.loading;
        loading.read.waited_first += read.waited_first;
        loading.read.waited_rest += read.waited_rest;
        if let Some(scanned) = scanned {
            loading.scanned = Some(loading.scanned.unwrap_or(0).saturating_add(scanned));
        }
        let step = loading.step();
        drop(shared);
        (events.borrow_mut())(RunEvent::PartDone(PartDone {
            step,
            number: range.number,
            of: self.of(),
            from: range.from.clone(),
            to: range.to.clone(),
            rows: read.rows,
            bytes: read.bytes,
            first_row_ms: read.first_row.map(millis),
            elapsed_ms: millis(started.elapsed()),
            scanned_bytes: scanned,
        }));
        Ok(())
    }

    /// `reader`'s pages into the queue until it has no more, fails, or the appender stops: what
    /// the range read.
    async fn read_pages<R: SourceReader>(
        &self,
        shared: &RefCell<Shared<'_, '_>>,
        reader: &mut Shaped<R>,
        range: &PartRange,
        started: Instant,
        pages: &mpsc::Sender<SourcePart>,
    ) -> Result<Count, RunFailure> {
        let mut read = Count::default();
        loop {
            let (page, waited) = timed(reader.next_part()).await;
            read.waited(waited, read.first_row.is_none());
            let step = || shared.borrow().loading.step();
            let page = match page {
                Ok(Some(page)) => page,
                Ok(None) => return Ok(read),
                Err(error) => return Err(error.at(step())),
            };
            if page.is_empty() {
                continue;
            }
            read.first_row.get_or_insert_with(|| started.elapsed());
            read.add_part(&page);
            {
                let mut shared = shared.borrow_mut();
                let loading = &mut shared.loading;
                loading
                    .read
                    .first_row
                    .get_or_insert_with(|| loading.opened.elapsed());
                loading.read.add_part(&page);
                loading.read.check(loading.source)?;
            }
            self.check_range(range, read.rows, step)?;
            let (sent, room) = timed(pages.send(page)).await;
            shared.borrow_mut().loading.spent.load += room;
            if sent.is_err() {
                // The appender failed, and says why.
                return Ok(read);
            }
        }
    }

    /// Whether `rows` is within `max_rows_per_part`.
    fn check_range(
        &self,
        range: &PartRange,
        rows: u64,
        step: impl Fn() -> crate::Step,
    ) -> Result<(), RunFailure> {
        match self.partition.max_rows_per_part {
            Some(max) if rows > max => {
                let value = format!("{} rows", grouped(max));
                let step = step();
                let crate::Step::Source { id } = &step else {
                    return Err(RunFailure::new(step, "a range ran past max_rows_per_part"));
                };
                let raise_in = format!("the [[source]] {id:?}");
                Err(RunFailure::at_limit(
                    step.clone(),
                    format!(
                        "range {} of {} ({} to {}) returned more than {value}, its \
                         max_rows_per_part; narrow its ranges with a smaller every, filter in \
                         its query, or raise max_rows_per_part",
                        range.number,
                        self.of(),
                        range.from,
                        range.to
                    ),
                    LimitHit {
                        key: "max_rows_per_part".to_owned(),
                        value,
                        raise_in,
                        set_in: Some(LimitSetIn::File),
                    },
                ))
            }
            _ => Ok(()),
        }
    }
}

/// The source's progress, once a second while its ranges are read.
fn tick(shared: &RefCell<Shared<'_, '_>>, step: &crate::Step, of: u64, events: Events<'_, '_, '_>) {
    let mut shared = shared.borrow_mut();
    let parts = Parts {
        done: shared.done,
        running: shared.running,
        of,
    };
    let loading = &mut shared.loading;
    let busy = loading.clock.lap(Duration::ZERO);
    let event = progress_at(step, &loading.read, loading.opened, At::Parts(parts), busy);
    drop(shared);
    (events.borrow_mut())(event);
}

/// Appends the parts `queued` to `table` as they come, until every reader has ended; and when
/// it was done. Its time waiting for a page counts as the source waiting on its engine, and
/// its time appending as loading, in the source's progress. Stops at the first page DuckDB
/// refuses, which closes the queue.
async fn append_timed(
    table: &mut Table,
    mut queued: mpsc::Receiver<SourcePart>,
    shared: &RefCell<Shared<'_, '_>>,
) -> (Result<(), DriverError>, Instant) {
    let mut outcome = Ok(());
    loop {
        let (page, idle) = timed(queued.recv()).await;
        let Some(page) = page else {
            break;
        };
        shared.borrow_mut().loading.clock.waited(idle);
        let (appended, busy) = timed(table.append(page)).await;
        shared.borrow_mut().loading.clock.loaded(busy);
        if let Err(error) = appended {
            outcome = Err(error);
            break;
        }
    }
    queued.close();
    (outcome, Instant::now())
}
