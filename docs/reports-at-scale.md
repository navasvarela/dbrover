# Reports at scale

Status: **RS1 built** (2026-11-09); **RS2 built** (2026-11-10); **RS3 built** (2026-11-10);
**RS4 built** (2026-11-11); **the rest planned, not scheduled.** Proposed on
2026-11-04, to come after RP5 (PRQL) unless the order changes. Each milestone's decisions are confirmed when it starts, as in
[Reports](reporting.md) and [Cloud and big data](cloud-data.md).

[Reports](reporting.md) (RP1 to RP6) run a recipe of sources, a combine step and outputs in a
worker process, under guardrails sized for a desktop: a million rows and a gibibyte a source,
2 GiB of DuckDB memory, 10 GiB spilled, 30 minutes a run. That covers month-end accounting on an
operational database. It doesn't cover a report whose sources hold terabytes: a year of events
in BigQuery, a lake of Parquet files in S3, an orders table of hundreds of millions of rows.

This plan says how reports reach those sources from a desktop app, and what tools show where a
report's time, memory and bytes go so that its author can make it faster. It builds on RP1 to
RP6, on C0's jobs and cost guards, and on C2's object storage, and lands in milestones RS1 to
RS8, one GitHub stack each. RS9 adds [composing reports](#composing-reports) from other
reports, which helps at any scale.

## The principle: terabytes stay where they are

A desktop can't move a terabyte. At 1 Gbit/s, the best a good office network gives, a terabyte
takes 8,000 seconds, more than two hours, before anything is parsed, and a laptop on Wi-Fi or a
VPN is slower still. Cloud egress is billed by the gigabyte on top. So:

- **A report reduces each large source where it lives.** The warehouse, the lake's own pruning
  or the database's index does the filtering and aggregating, and only the reduced rows cross
  the network: megabytes to a few gigabytes per source, not terabytes.
- **The desktop combines what was reduced.** DuckDB on the worker joins, shapes and writes rows
  that fit the machine's disk, spilling past its memory.
- **DbRover's job is to make that easy to do and easy to check:** read reduced results fast,
  bound and resume long runs, and show the author, for every source, how much was scanned,
  how much came back and how much the report used.

| Fits a desktop | Doesn't fit a desktop |
| --- | --- |
| Running heavy SQL in each warehouse and combining the results locally. | Moving terabytes over the network to process them locally. |
| A lake of Parquet or Iceberg in a bucket, read through DuckDB with its pruning, so a query reads megabytes of a terabyte. | A join of two unreduced sources of a terabyte each, in two different engines. |
| Long runs, hours, on a plugged-in machine, that survive a crash, a sleep or a killed worker. | Always-on, scheduled, multi-user execution: cron and CI run `dbrover report run`, and a server stays out ([Vision](vision.md)). |
| Gigabytes in the combine step, spilled to a local disk. | A cluster: distributing one run across machines. |
| Parquet outputs of any size; CSV of millions of rows. | Excel past a few million rows: its sheets hold 1,048,576 rows each. |

## What limits today's runs

Read from the code as built through RP6:

1. **Every value travels as text.** Drivers hand over `Vec<Option<String>>` rows (ADR 0004), a
   page of 1,000 rows at a time (`PAGE_ROWS` in the engine's `report_source.rs` and in
   `combine.rs`). Formatting each value on the server, parsing it back in the worker and again
   in DuckDB costs CPU on both ends and bytes on the wire.
2. **Each loaded source is copied twice.** `TableLoader` appends text to a `VARCHAR` table in
   `dbrover_load`, then `finish` rewrites it as `CREATE TABLE main.<id> AS SELECT CAST(…)`,
   because RP2 infers a column's type from all its values. For a large source, that is twice
   the I/O and, briefly, twice the space.
3. **The run's DuckDB lives in memory.** Operators spill past `memory_limit` to the staging
   folder; whether, and at what cost, DuckDB also evicts an in-memory database's own tables to
   that folder is something RS1's benchmark measures rather than something the plan assumes.
   A database on a file evicts its pages as any database does.
4. **The combine's rows come back as text to Rust writers,** a page at a time, even when the
   output is a format DuckDB writes itself. Parquet isn't a report output yet (RP1 refuses it),
   though `dbrover-driver-duckdb::parquet` writes it for exports.
5. **A run is all or nothing.** Its staging folder is removed however it ends; a run killed at
   hour three starts again from the first row.
6. **The run's time limit is a wall clock** (30 minutes by default), which a legitimate long run
   hits, and which can't tell a stuck run from a slow one.
7. **What a source cost its engine is unknown.** `source_done` counts rows and bytes received;
   bytes scanned on the server, which a warehouse bills by, aren't asked for.
8. **The metrics are per step and at the end.** RP4 records each step's totals, DuckDB's two
   peaks and each output's time. Nothing shows when a step was waiting on its database, on the
   network, on DuckDB or on a writer, and the combine query's own plan isn't kept.

Items 1 to 6 are execution; items 7 and 8 are tooling. The tooling comes first in the
[recommended path](#recommended-path), because the rest should be built on measurements.

## Execution at scale

### Pushing work to the engines

Every source is already a query in its engine's language, so pushdown today is the author's:
filter and aggregate in the source's query, and RP4's hint says when a source reads ten times
more than the report writes. The question is how much DbRover should do on its own.

| Option | What it does | Trade-off |
| --- | --- | --- |
| **A. Hints from the combine's plan** | Before or after a run, DuckDB plans the combine query over the sources' tables. The plan shows which columns of each source it reads, which filters it applies straight to one source's scan, and which aggregates read one source alone. Each becomes a hint naming the source and the change: "`tx.memo` and `tx.tags` are never read: dropping them from the query saves 41% of its bytes"; "`WHERE region = 'EU'` filters only `orders`: in its query, PostgreSQL can use an index". | Safe: nothing is rewritten, the author edits the file. Pure, tested on plans. Needs the author to act. |
| **B. Wrapping a source's query** | `pushdown = "auto"` on a SQL source wraps its query as `SELECT <used columns> FROM (<query>) AS s WHERE <filters>`, in the source's dialect, for filters that are comparisons, `IN` lists and `BETWEEN` on one column against a literal or a parameter, joined by `AND`. The engine's optimiser pushes the outer filter into its own plan, as PostgreSQL, MySQL, SQL Server, ClickHouse, BigQuery and Snowflake do with a derived table. | Automatic, and limited to forms whose meaning is the same in every SQL dialect. A wrong translation would silently change a report's numbers, so it is opt-in per source, shown in the source's line ("ran with 2 filters and 5 of 9 columns pushed"), and the statement that ran is in the run's profile. Documents and DynamoDB aren't wrapped. |
| **C. DuckDB's scanners** | Attach a PostgreSQL or MySQL data source through DuckDB's `postgres` and `mysql` extensions, so DuckDB pushes filters and projections into its own queries, and reads in parallel with binary `COPY`. | Fast, and pushdown for free. But it bypasses DbRover's drivers: W11's read-only transaction, the password command, the cost guard and the SSH tunnel would each be redone for DuckDB (a tunnel's local port works; the rest needs checking), and the extensions download on first use. RP2 decided against it; this keeps that decision until RS1's measurements show the text path is the bottleneck on PostgreSQL. |
| **D. A federated engine** (DataFusion with a table provider per driver) | DbRover's drivers become tables of a second query engine that pushes filters into each by design. | Principled, and a second SQL dialect for the combine step beside DuckDB's, plus a large build (see [Other technologies](#other-technologies)). Rejected. |

**Proposed: A, then B.** Hints first (RS2), since they need nothing but DuckDB's plan and
change nothing that runs. B comes later, once the hints show which edits people make again and
again; its translation reuses `dbrover-sql`'s dialects and `sqlparser`, already in the build.

**Aggregates** are pushed by hint only. Moving a `GROUP BY` across a join changes the result in
general; a hint says when the combine's plan aggregates one source's scan with nothing joined
first, which is the case where moving it is safe.

**Lookups** (RP3) are DbRover's semi-join pushdown already: the keys of a reduced source filter
a large one. On warehouses they need two changes, both in the milestone that brings each
warehouse: a batch size per engine measured against its limits (a statement of a few hundred
kilobytes, not a thousand keys), and the cost guard judging all of a lookup's batches together,
since BigQuery bills each statement at least 10 MB for every table it reads.

### Streaming

Sources already stream: a page is read, appended and dropped. What doesn't stream is the cost
per value, and the double copy.

**Typed loading when the type is known.** A source whose columns are declared (RP3), or whose
driver reports a type with its precision (`DECIMAL(18,2)`, `BIGINT`, `TIMESTAMP`), gets its
typed table first, and DuckDB's appender casts each text value as it appends. Only columns
whose type comes from their values (PostgreSQL's `numeric` without precision, text protocols
that report no type) keep RP2's load-then-cast, and only those columns are rewritten. One copy
instead of two, and a value that doesn't fit fails at the row that carries it.

**Columnar batches for engines that speak them.** Several engines answer in Apache Arrow, or in
a columnar format that reads into Arrow without text:

| Engine | Columnar result | Comes with |
| --- | --- | --- |
| DuckDB | Arrow, natively (`query_arrow`) | Already attached in place (RP2); a DuckDB source with a query could be read as Arrow. |
| ClickHouse | `FORMAT ArrowStream` over its HTTP interface | The driver's existing HTTP path; needs `arrow-ipc`. |
| Snowflake | Arrow result chunks from the SQL API | C5. |
| Databricks SQL | `ARROW_STREAM` results, inline or as external links | C6. |
| BigQuery | Arrow from the Storage Read API (gRPC), billed apart from the query | Not in C4, which reads `getQueryResults`' JSON as text ([C4's note](cloud-data.md#rows-and-columnar-batches-in-c4)); later, for large results, if gRPC is accepted (below). |
| Trino and Athena | None in their APIs (text rows); Trino's spooled results and Athena's result files in S3 (CSV, or Parquet through `UNLOAD`) are files | C3 delivers text rows; the files are RS7's handoff, through C2's object storage. |
| PostgreSQL, MySQL, SQL Server | None. PostgreSQL's `COPY (query) TO STDOUT` streams text faster than the simple protocol, inside the read-only transaction. | A later layer of this plan, if RS1 measures it worthwhile. |

The proposal: an optional capability on `ResultCursor`, `next_batch`, that returns an Arrow
`RecordBatch`, which the loader hands to DuckDB's `Appender::append_record_batch` (the `duckdb`
crate's `appender-arrow` feature, not yet turned on) with no text in between. Drivers without it keep `next_page`, and the console and the grid keep text, as
ADR 0004 says: values on screen are text, whatever the engine. A new ADR (0007) records the
exception: **bulk paths (reports, exports) may carry Arrow where the engine gives it
natively.** Arrow's `Decimal128` holds 38 digits, so exact decimals stay exact; `arrow` 58 is
already in the build through the `duckdb` crate, so the arrays cost nothing new, and
`arrow-ipc` (for ClickHouse and Databricks streams) is a small pure-Rust addition.

C4 to C6 come after this plan's first milestones, so their drivers implement `next_batch` from
the start rather than adding it later. C3 started first (2026-11-05) and doesn't need it:
Athena's and Trino's APIs answer in text, so their cursors keep `next_page`, and `next_batch`'s
default (`None`) leaves them unchanged ([C3's note](cloud-data.md#rows-and-columnar-batches)).
C4 (2026-11-08) did the same for BigQuery: its REST results are JSON text.

**Backpressure.** A source's reader and its loader are joined by a bounded queue of a few
batches, so a fast network never piles up rows in the worker, and the time each side waits is
measured: it is what tells a source-bound run from a load-bound one (see
[Live progress](#live-progress-and-the-bottleneck)).

**Outputs.** Parquet becomes a report output, written by DuckDB's `COPY (combine query) TO`
straight from the workspace, so no row passes through Rust and DuckDB writes it in parallel.
The lock-down turns `enable_external_access` off; DuckDB's `allowed_directories` setting, set
before the lock to the run's staging folder alone, lets `COPY` write there and nowhere else
(checked in RS3's first layer against the DuckDB in the build). CSV and Excel keep RP1b's
writers and their exact formatting; a CSV written by DuckDB is left for later, once RS1 shows
writers matter and golden tests show DuckDB's formatting matches RP1b's for a schema.

### Partitioning

Two kinds, which a report can combine:

**A partitioned source** splits one source's query into ranges, read as separate statements:

```toml
[[source]]
id = "events"
data_source = "events-bigquery"
query = """
SELECT customer_id, DATE(ts) AS day, COUNT(*) AS events, SUM(amount) AS amount
FROM analytics.events
WHERE ts >= :part_from AND ts < :part_to
GROUP BY 1, 2
"""
partition = { from = ":since", to = ":until", every = "1 month", parallel = 2 }
```

Each range binds `:part_from` and `:part_to`, as a lookup binds its list; neither can be one
of `params`, as a lookup's `as` can't. Ranges are of time
(`every = "1 day"`) or of an integer key (`every = 10_000_000` over `from` and `to`), never
guessed from the data. The source's table gets every range's rows. What it buys:

- each statement stays under the engine's and the source's timeout;
- `parallel` statements at once, bounded, where the engine has the capacity (a warehouse, a
  read replica), and one at a time by default;
- progress in ranges ("month 7 of 12"), and **a range is the unit a resumed run skips**;
- the skew between ranges, which the profile shows.

**A partitioned run** runs the whole report once per range and writes each range's rows before
the next starts:

```toml
[report]
partition = { from = ":since", to = ":until", every = "1 month" }

[[output]]
format = "parquet"
file = "ledger-{part_from:%Y-%m}.parquet"
```

The combine step then works on one range at a time, so the run's memory and spill are a
range's, not the whole period's. `file` stays a plain file name (RP1): a file per range names
the range in it, as above, and a `file` without `part_from` or `part_to` is one file that every
range's rows are appended to. That is correct only when the combine is local to the range:
each output row depends on its range's rows alone. DbRover can't check that in general, so the
manual says what isn't (a running balance carried across months, a `DISTINCT` over the whole
period) and how to make it so (an opening-balance source per range). Excel appends to one
file, within its row limit.

Hash partitions (`hash(customer_id) % 8`) are left out: they need the same expression in every
engine's dialect, and ranges cover the reports this plan is checked against.

### Bounding memory and disk

The guardrails of [Reports](reporting.md#guardrails) hold, and gain what long runs need:

1. **Sources:** `max_rows`, `max_bytes` and `timeout` hold per source as before; a partitioned
   source also takes them per range (`max_rows_per_part`), so one bad range fails early.
   C0's cost guard judges a source's estimate across all its ranges and batches, since nobody
   is there to approve the twelfth month.
2. **Combine:** `memory_limit`, `threads`, `max_temp_size` and `timeout` as in RP2. The
   workspace moves to a file in the run's work folder (RS3), always, since a file-backed DuckDB
   with little in it costs nothing measurable to open: loaded tables are evicted to disk like
   any database's pages, and `max_temp_size` bounds the spill beside it.
3. **Disk for the whole run:** a new `max_disk`, the most the run's work folder may hold (the
   workspace file, the spill, kept ranges and the staging files), checked against the free
   space of its disk before the run starts and as it grows. DuckDB already computes the free
   space of its temporary folder's disk (the default of `max_temp_directory_size` is 90% of
   it), so the worker reads it from DuckDB rather than adding a dependency.
4. **Memory outside DuckDB:** the bounded queues make it computable: `memory_limit` + each
   running source's queue (batches × batch size) + the writers' buffers. The run's plan says
   the bound, and the profile records the worker's peak resident memory (`getrusage` through
   `rustix`, which the worker already uses for its priority and `RLIMIT`s, on Linux and macOS).
5. **Time:** the run's wall-clock `timeout` stays, and a **stall timeout** joins it: a run whose
   steps report no progress (no row, batch, range or DuckDB work) for `stall_timeout` (default
   10 minutes) is stopped as stuck. A long run sets a long `timeout` and keeps a short stall
   timeout. On Linux, `RLIMIT_CPU` keeps following the run's time limit.
6. **Sleep:** a run longer than a few minutes keeps the machine awake while it runs: on macOS
   `caffeinate -i -w <worker pid>`, on Linux `systemd-inhibit` holding a sleep inhibitor
   around a wait on the worker's pid, where it exists; both are programs the system already has. Windows waits for a safe wrapper, as its Job Object does.

The defaults go in Settings › Reports with RP4's, and a report's file overrides them.

### Pause, resume and checkpoints

**A work folder that outlives a failed run.** Today the staging folder is removed however a run
ends. From RS3, every run has a **work folder** in the data folder's `report-runs/<run id>/`
with the workspace file, the spill and a `manifest.json`. A run that finishes removes it; a run
that fails or is stopped keeps it when it had loaded more than `resumable_above` (default
1 GiB), and removes it otherwise, since starting such a run again costs less than keeping its
folder. Output staging stays in the output folder, where the final rename needs it. Work folders expire after 7 days and count against a total in Settings,
and **Discard** removes one at once.

**The manifest** records, as each unit finishes:

- what the run is: the report file's hash, the parameters, the limits, DbRover's version and
  each data source's identity (its id, host and database, not its secrets);
- each unit and its state: a source, a source's range, a lookup's batch, or a run's partition,
  with its rows, bytes and the table or file it was loaded into;
- for a source on a job engine (C0's `JobService`), the job's id and the last page token, since
  a warehouse keeps a finished job's result for a while (BigQuery a day, Snowflake's
  `RESULT_SCAN` a day, Athena's in S3 until deleted): a resumed run pages on instead of paying
  for the query again.

**Resuming** reads the manifest, checks that the report file, parameters and data sources are
the ones it names (else it says what changed and offers to start again), skips the units that
finished, reruns those that didn't, and carries on. A unit is all or nothing, so a resumed run
writes the same rows as one that never stopped, provided the sources still hold what they held.

**Pause** is a cancel that keeps the work folder: the running units stop, and are read again on
resume. Suspending the worker's process (SIGSTOP) was considered and rejected: connections and
warehouse jobs stay open, server timeouts expire, and a warehouse keeps billing a paused job.

**Consistency across a gap.** A source read on Monday and resumed on Tuesday may have changed.
Two answers, both explicit:

- **A pinned time** where the engine has time travel: `as_of = "run_start"` on a source binds
  `:as_of` to the run's start, recorded in the manifest, for queries that read
  `FOR SYSTEM_TIME AS OF :as_of` (BigQuery), `AT(TIMESTAMP => :as_of)` (Snowflake),
  `TIMESTAMP AS OF :as_of` (Delta, Databricks) or an Iceberg snapshot.
- **Closed ranges** everywhere else: a range of last month's ledger doesn't change, and the
  manual says to partition by such ranges when a run may need resuming. PostgreSQL has no time
  travel, and its exported snapshots don't outlive a session.

**Incremental re-runs** keep a source's rows between runs, for sources that only grow:

```toml
[[source]]
id = "events"
data_source = "events-clickhouse"
query = "SELECT * FROM events WHERE ts >= :after"
incremental = { column = "ts", from = ":since", lookback = "2 days", key = ["event_id"] }
```

The rows are kept as Parquet in the data folder's `report-cache/<report>/<source>/`. The first
run binds `:after` to `from`; each later run binds it to the newest `ts` kept minus
`lookback`, replaces kept rows by `key`, and loads the union. Late or back-dated rows inside `lookback` are picked up; older ones aren't, which the
manual says plainly (back-dated journal entries are the accounting case), and **Run from
scratch** drops the cache. The cache counts against the work folders' total and is cleared with
**Clear history**.

### When to delegate

A source *is* delegation: its query runs in its engine, so the heavy part of a report already
runs in the warehouse when its query does the work. What this plan adds is help in deciding
where each part runs, and two paths for results too large for row-by-row paging:

1. **Reduce in the source's engine** whenever the reduction can be written in its dialect: the
   default, which RS2's hints and the data funnel (below) point at.
2. **Read a lake through DuckDB in the combine step.** With C2, a DuckDB data source can hold a
   bucket. Attached to the run's workspace (`attach = true`, as RP2 attaches DuckDB files),
   the combine reads `read_parquet('s3://lake/events/*/*.parquet')` or an Iceberg table, and
   DuckDB's own filter and projection pushdown, row-group statistics and Hive partitions read
   only the bytes that matter, in parallel range requests. The lock-down then allows the
   bucket's prefix (`allowed_directories`) instead of nothing, the bucket's secret is made
   before the lock and stays temporary, and `httpfs` is loaded before autoloading is turned off.
   The cost guard's byte estimate comes from C2's listing and the files' footers. **This is the
   one path where a desktop queries a terabyte directly**, and it fits DbRover well: no new
   engine, C2's sign-ins, and DuckDB's pruning.
3. **Hand results over through a bucket.** For a warehouse result of many gigabytes, paging
   through the warehouse's API is slower than the warehouse writing Parquet to a bucket and
   DuckDB reading it in parallel: BigQuery's `EXPORT DATA`, Snowflake's `COPY INTO @stage`,
   Athena's `UNLOAD` (Athena writes its results to S3 anyway), Databricks's external links.
   Writing to a bucket is a side effect that a report's read-only promise doesn't cover, so a
   source opts in with `handoff = "s3://my-bucket/dbrover-tmp/"`, a prefix the user names, and
   the run deletes what it wrote when it ends (a signed delete per object, a small addition to
   C2's listers, which only list). After C3 to C5, engine by engine.
4. **A federating engine as the source.** When two large sources live in engines that can read
   each other (Athena or Trino over several catalogs, BigQuery over GCS external tables,
   Snowflake over S3), the join of the two runs as one source's query there, and DbRover
   combines its reduced result with the rest.
5. **MotherDuck.** Attaching a MotherDuck database (C2g) to the workspace lets MotherDuck run
   the scans of its own tables. Left for later: the workspace would talk to a service during
   the locked combine, and MotherDuck is untried (C2's hand-over).

What DbRover doesn't do: copy data between warehouses, create tables in a source to stage
intermediate results, or run a cluster. Reports read (W11), and the
[Scope](reporting.md#scope) of reports stays.

**A rule of thumb for the manual:** if a source's result is above a few gigabytes, reduce it
further in its engine, partition it, or hand it over through a bucket; if it is a lake, attach
it; if two large sources meet, meet them in an engine that reads both.

### Other technologies

Judged on fit with DuckDB as the one combine engine, on the binary's size and on build time,
since CI time is a budget. Sizes are rough, from the projects' own reports, to be measured
before any is added.

| Technology | Fit | Cost | Verdict |
| --- | --- | --- | --- |
| **Apache Arrow** (`arrow-rs`) | The batch format between columnar engines and DuckDB. | `arrow` 58 is already built through `duckdb`; `arrow-ipc` adds a small pure-Rust crate and `flatbuffers`. | **Yes**, RS6. |
| **Arrow Flight / Flight SQL** | Few of DbRover's engines serve it (Dremio, InfluxDB 3); the warehouses' fast paths are their own (Snowflake's Arrow chunks, Databricks's links, BigQuery's Storage API). | gRPC: `tonic`, `prost`, `h2` and their build-time code generation. | **No**, until an engine DbRover adds needs it. |
| **BigQuery Storage Read API** | Arrow results in parallel streams, much faster than REST paging for large results. | gRPC, as above. | **Not in C4** (2026-11-08): billed apart from the query and needing gRPC, for results the console reads a page at a time. Decided again with RS6 and RS7, against the bucket handoff (`EXPORT DATA`), which needs nothing new. |
| **DataFusion** | A second SQL engine overlapping DuckDB; its table providers would make pushdown principled. | Hundreds of crates; minutes of build and many megabytes of binary. | **No.** One combine dialect (DuckDB's) is a feature. |
| **Polars** | A DataFrame library; its SQL is a subset, its lazy API not SQL. | Large, as DataFusion. | **No.** |
| **ADBC** | Arrow-native drivers, but as native shared libraries loaded at run time. | Packaging per platform, and native plugins, which [Vision](vision.md) keeps out. | **No.** |
| **DuckDB's scanners** (postgres, mysql) | Pushdown and parallel binary reads for two engines. | Extension downloads; DbRover's guards redone for them. | **Not now** (option C above). |
| **DuckDB's httpfs, iceberg, delta** | Lakes in the combine step. | Downloaded on first use, already accepted in C2. | **Yes**, RS7. |

## Composing reports

Proposed on 2026-11-04, after the rest of this plan, at the user's request: build a report from
parts of other reports. A valuation, a trial balance or a heavy warehouse aggregation is written
once and used by several reports, instead of being copied from file to file and drifting. It
belongs here too because reuse is how a heavy step runs once rather than once per report.

### A report as a source

```toml
[report]
name = "Gains by wallet"
params = { since = "timestamp", until = "timestamp" }

# The rows of the report "valuation" in reports/, run with this report's period.
[[source]]
id = "valued"
report = "valuation"
params = { since = ":since", until = ":until" }

[combine]
query = """
SELECT wallet, SUM(value_usd) AS value_usd
FROM valued
GROUP BY wallet
"""
```

- **What it gives:** the inner report's result, its `[combine]`'s rows with the columns and
  types DuckDB gives them, or its one source's rows without a combine, as the table `valued`.
  Its outputs aren't written and their schemas aren't applied: an output's rounding and formats
  are for files, and the outer report's outputs do their own. Its result is its contract: the outer report can't read the inner report's own sources, so the
  inner report can change how it gets its rows without breaking the reports that use it.
- **Parameters:** `params` gives each of the inner report's parameters a value, either one of
  the outer report's parameters (`":since"`) or a literal of its type; a text literal can't
  start with a colon. A parameter left out, or of another type, is a problem in the outer file
  that names both reports.
- **Where it runs:** in the same worker and the same DuckDB. The inner report's sources load
  with the outer's, under their own limits, into a schema named after the outer source's id
  (`valued.tx`), so ids never clash. Once everything is loaded and the database is locked, each
  inner combine runs as `CREATE TABLE main.valued AS …`, innermost first, then the outer
  combine. Each inner combine runs on a connection whose `search_path` names its schema,
  set before the lock, so it reads its own `tx` as written. This is checked in the milestone's
  first layer against DuckDB's `lock_configuration`. A lookup may read from a report source,
  as from any loaded source.
- **Nesting:** a report used by another may itself use reports, up to a depth of 4. A cycle is a
  problem when the files are read, naming the reports in it.
- **Attached sources** (RP2's `attach = true`) are catalogs, not tables, and keep their ids as
  aliases across the run, since the inner combine names them (`prices.hourly_prices`). Two
  reports in one run attaching different files under one id is a problem naming both; the
  same file under one id is attached once.
- **Limits:** each inner source keeps its own caps, and the outer report's `[combine]` limits
  bound the one DuckDB they share; the inner report's `[combine]` limits and `[report] timeout`
  aren't read. An inner report's `[report] partition` (RS4) is a problem: a partitioned run is
  the outer report's to declare.
- **The profile and the history:** an inner report's steps nest under the outer source's lane
  in the timeline, and hints name them by path (`valued › tx`). The outer report's run is
  recorded with the file hash of every report it used. The inner report's own history doesn't
  change, since it wrote no files.
- **In the app:** the explorer marks a report that others use, and its tab lists them under
  **Used by**. A problem in an inner report shows as a problem in every report that uses it.
  **Edit file** on a report source opens the inner report.

### Reusing a result

With RS5's cache, a report source can take a recent result instead of running again:

```toml
[[source]]
id = "daily"
report = "events-by-customer-day"
params = { since = ":since", until = ":until" }
reuse = "24h"
```

A run of the inner report, alone or inside another, keeps its result as Parquet in the data
folder's `report-cache/`, keyed by its file's hash and its parameters. `reuse` takes a kept
result younger than its age with the same key, and otherwise runs the report and keeps the new
result. A warehouse aggregation that costs ten minutes and real money then runs once a day for
every report that uses it. The profile marks a reused source and how old its result was, and
**Run from scratch** ignores the cache. Without `reuse`, the inner report always runs.

### Options considered

| Option | What it is | Verdict |
| --- | --- | --- |
| **A report as a source** (above) | Another report's result, by name, with its parameters bound. | **Yes.** Encapsulated, checked when files are read, and it uses the run's existing machinery. |
| **Shared definitions** (`include = "parts/sources.toml"`) | Textual reuse of `[[source]]` or `[combine]` blocks across files. | **No.** Nothing is encapsulated, an edit to a part changes every report silently, and a problem's position is hard to report. A report source covers the need with a contract. |
| **Shared SQL functions** | `reports/macros.sql` of DuckDB `CREATE MACRO`s (`fiscal_period(d)`, `to_usd(amount, unit, price)`) loaded into every run's DuckDB before the lock. | **Later, small.** Reusable logic rather than reusable data, and a natural layer of the same milestone if reports start repeating expressions. |
| **Reading another report's output files** | Attach the Parquet or CSV a report wrote. | **No.** Paths differ by machine and by run, nothing says how fresh a file is, and the lock-down would have to allow them. `reuse` does the same with a key and an age. |
| **A separate worker per inner report** | Run each inner report in its own process and pass its result as a file. | **No.** Two DuckDBs to bound instead of one, and a file between them. The shared workspace keeps one set of limits. |

## Performance tooling

Everything below reads one record: the **run profile**, a JSON document per run, versioned,
that extends RP4's `steps` with timings, phases, plans and hints. The app draws it, the command
line writes it, MCP serves it, and two of them compare.

### What a run records

| Step | RP4 records | Added |
| --- | --- | --- |
| Every step | time, rows, bytes | when it started and ended in the run (a timeline), its phases, and the time it waited on its neighbours |
| A source | time to first row, batches | phases: connect, the engine's work until the first row, transfer, loading into DuckDB, typing; bytes scanned on the server where the engine reports them; each range's numbers; the statement that ran (after parameters, lookups and pushdown); the engine's plan when asked |
| The combine | DuckDB's peak memory and spill | DuckDB's operator tree from its profiler (each operator's time, rows in and out, and memory), the columns and filters each source's scan used |
| An output | time, file size | rows a second, and the time spent waiting for rows versus writing them |
| The run | total time, parameters, outcome | the report file's hash, DbRover's version, the machine's cores and memory, the worker's peak resident memory, the work folder's peak size, the hints |

**Bytes scanned** is the number a warehouse bills by and the one that says whether pushdown
worked. Where the engine reports it: BigQuery's `totalBytesProcessed`, Snowflake's
`bytes_scanned` in its query history, Athena's `DataScannedInBytes`, ClickHouse's `read_bytes`
(its `X-ClickHouse-Summary` header), Trino's `processedBytes`; DuckDB's own profiler for an
attached lake. PostgreSQL, MySQL and SQL Server report none outside `EXPLAIN ANALYZE`.

### A timeline

The report's tab gains a **Profile** for any run, newest first:

- **A timeline**, one lane a step: sources in their rounds, a partitioned source's ranges as
  segments of its lane, the combine, the outputs. Each bar is split into its phases in shades
  (connect, server, transfer, load), so a source that spent nine minutes before its first row
  looks different from one that spent nine minutes receiving it. Waiting on a round, or on a
  full queue, is drawn hatched. Hovering a segment shows its numbers.
- **The data funnel**, a bar per source: bytes scanned in its engine, returned to DbRover, kept
  by the combine (the rows its scan passed on, from DuckDB's profile), and written. The widest
  drop is the hint: scanned 1.2 TB, returned 40 GB, used 300 MB says the source's query should
  filter.
- **The combine's plan**, as W10 draws a plan: DuckDB's operator tree read into `dbrover-plan`'s
  `Plan`, with its costliest operators tinted, and the spill against `memory_limit`.

A flame graph was considered for the combine; DuckDB's operators form a tree with times, which
W10's plan view already draws with weights, so the plan view is reused instead of adding a
second drawing.

### Each source's plan and estimated cost

**Explain report** (a button in the tab, and `dbrover report explain`) plans a report without
running it:

- each SQL source's statement, with the parameters given, explained by its engine through W10
  (PostgreSQL, MySQL, MariaDB and SQL Server), and by new readers of DuckDB's, ClickHouse's
  (`EXPLAIN PLAN json = 1, indexes = 1`, whose granules say how much of a table is read), and,
  as C3 to C6 bring them, Trino's and Athena's `EXPLAIN (TYPE IO)`, BigQuery's dry run,
  Snowflake's `EXPLAIN` (partitions and bytes assigned) and Databricks's `EXPLAIN COST`;
- a lookup's source explained with the statement its first batch ran in the last run, which
  the profile keeps, else with a placeholder list the plan says it used;
- the combine explained over empty tables of each source's last known columns, with the last
  run's row counts as DuckDB's estimates;
- C0's cost guard over every estimate, and each estimate in bytes beside the source's limit.

Explaining runs no statement that reads data, except where an engine's `EXPLAIN` does (none of
the above); `EXPLAIN ANALYZE` stays W10's console action.

### Live progress and the bottleneck

While a run runs, the tab shows each running step's rows and bytes a second as a small line,
the range or batch it is on, and **what it is waiting on**: the bounded queues measure, every
second, the share of time a source's reader waited for its engine, its loader waited for rows,
or the reader waited for room. The step's line then says *waiting on PostgreSQL (92%)*, *loading
into DuckDB (70%)* or *writing Excel (80%)*. For a warehouse source, C0's job progress (bytes
and rows the job has processed) shows before its first row arrives. DuckDB's current memory
comes from `duckdb_memory()` once a second, beside its limit.

The run's bottleneck at the end is the step and phase that held the critical path longest,
named under the timeline.

### Hints

RP4's two hints become a table of rules in `dbrover-report`, pure functions of a run profile,
each with an id, a threshold, and one sentence naming the step and the number behind it:

| Id | When | Says |
| --- | --- | --- |
| `source-reads-more` (RP4) | a source reads ≥ 10,000 rows and ≥ 10× the rows written | filter in the source's query |
| `combine-spilled` (RP4) | the combine spilled | raise `memory_limit`, or read fewer rows |
| `unused-columns` | the combine reads none of some of a source's columns, worth ≥ 20% of its bytes | drop them from the query |
| `filter-one-source` | the combine's plan filters one source's scan by a constant or parameter | move the filter into the source |
| `aggregate-one-source` | the combine aggregates one source's scan with nothing joined first | aggregate in the source |
| `scanned-more` | an engine scanned ≥ 100× what it returned | check that the query filters on the table's partition or clustering column; an aggregation scans more than it returns on purpose, so this is a question, as `source-reads-more` is |
| `slow-first-row` | a source spent ≥ 80% of its time before its first row | explain its query (W10), look for an index |
| `load-bound` | a source spent most of its time loading | declare its columns, or partition it |
| `many-batches` | a lookup ran ≥ 50 batches | raise `batch_size`, or join in the combine |
| `round-wait` | a source waited ≥ 30 s for an unrelated source of the round before | the order of the file's sources |
| `writer-bound` | an output's writer took most of the run | write CSV or Parquet; Excel is the slowest |
| `range-skew` | a range took ≥ 5× the median range | split it, or check its data |
| `near-limit` | a peak reached ≥ 90% of its limit without passing it | the limit to raise before it fails |

`[report] quiet_hints = ["source-reads-more"]` silences a hint for a report that reads more
than it writes on purpose (RP4's left-for-later). The thresholds are constants, named in the
manual, and moved by measurements, not guesses.

### Comparing two runs

RP4 lists the newest runs side by side. **Compare** picks any two runs of a report and shows,
per step, the change in time, rows, bytes scanned and peaks, with changes past 50% marked; the
parameters and limits that differ; whether the report file changed between them (its hash); and
the combine's plan beside the other's, operators that appeared or disappeared marked. A step
that took more than 1.5× its median over the runs kept is marked as slower than usual in the
Runs table.

### Where it shows

- **The app:** the report's tab gains **Profile** (timeline, funnel, plan, hints) under each
  run, **Explain**, live waiting and throughput while running, **Compare**, **Resume** and
  **Discard** on a run that kept its work folder.
- **The command line:**
  - `dbrover report run … --progress`: a live line on standard error when it is a terminal,
    which RP6 left for later; cron's mail stays short without it.
  - `--profile <file>`: the run profile as JSON, for CI to keep beside its logs.
  - `--resume <run id>` and `--from-scratch`.
  - `dbrover report explain <report> -p …`: the plans and estimates, as text or `--json`.
  - `dbrover report runs <report>` and `dbrover report compare <report> <run> <run>`, reading
    the history the app keeps.
  - The exit codes stay RP6's; a stopped run that can be resumed says how on standard error,
    and its `failed` or `cancelled` event carries the run's id.
- **MCP:** read-only tools that suit an assistant optimising a report: `list_reports`,
  `explain_report` (under the cost guard, as `dbrover mcp`'s other tools), `report_runs` and
  `report_profile` (the history, profiles and hints), and `compare_report_runs`. Whether an MCP
  client may *run* a report stays [Reports](reporting.md#open-questions)' open question; this
  plan proposes deciding it with RS8, off by default, behind the MCP server's scope.

## How it's built

- `dbrover-report`: the run profile, partitions and ranges, the manifest and resuming, the
  hints' rules, typed loading's plan, `pushdown` and `handoff` read and planned, and the graph
  of reports used as sources.
- `dbrover-driver-duckdb`: the workspace on a file, `allowed_directories`, Parquet by `COPY`,
  appending Arrow batches, the profiler's operator tree, free disk space.
- `dbrover-plan`: DuckDB's and ClickHouse's plan formats, then each warehouse's.
- `dbrover-core`: `ResultCursor::next_batch`, and bytes scanned in a job's progress.
- `dbrover-engine`: phases and waits measured in the source reader, the bounded queues,
  explaining a report's sources.
- `dbrover-store`: the profile in `report_runs`' `steps` (JSON, so no schema change), work
  folders and the incremental cache.
- `dbrover-app`: the Profile, Explain, Compare, Resume and the live view; the command line's new
  subcommands and flags; the MCP tools in `dbrover-mcp`.

No new crate is planned. `arrow-ipc` is the one new dependency (RS6); C4 didn't choose gRPC.

## Milestones

One GitHub stack per milestone, each layer one pull request, useful and tested on its own, with
synthetic data only. Tests that need large data generate it with DuckDB's `range()` at run time
and are marked slow (`--ignored`), run by hand before a release, as RP3's general ledger is; CI
runs the small cases.

- **RS0 — Plan:** this document and the roadmap's entries.
- **RS1 — Seeing a run** (the tooling, on today's runs):
  - RS1a — Plan: RS1's decisions and risks.
  - RS1b — The run profile: timestamps on every event, a step's phases and waits measured in
    the source reader and the loader, the worker's peak memory, the report file's hash; the
    profile kept in `report_runs`.
  - RS1c — The timeline: the tab's **Profile** with the timeline and the bottleneck.
  - RS1d — Live progress: throughput and what each step waits on, in the tab and in
    `dbrover report run --progress`.
  - RS1e — Hints as rules: the table above for what a profile already holds (`slow-first-row`,
    `load-bound`, `many-batches`, `round-wait`, `writer-bound`, `near-limit`), ids and
    `quiet_hints`.
  - RS1f — Comparing runs: **Compare** in the tab, `--profile`, `dbrover report runs` and
    `dbrover report compare`.
  - RS1g — Manual and demo note, with a benchmark of today's paths (text, typed, per engine)
    on generated data, which sets the numbers the later milestones are judged by.
  **Exit:** a slow run shows, without guessing, which source or step held it and why.
- **RS2 — Explaining a report:**
  - RS2a — Plan.
  - RS2b — DuckDB's plans in `dbrover-plan`: the profiler's operator tree after a run, and
    `EXPLAIN`'s before one.
  - RS2c — The combine's plan in the Profile, and the funnel's "used by the combine".
  - RS2d — **Explain report** and `dbrover report explain`: each source through W10's formats
    and a new ClickHouse reader, the combine over empty tables, the cost guard over it all.
  - RS2e — Plan-based hints: `unused-columns`, `filter-one-source`, `aggregate-one-source`.
  - RS2f — Bytes scanned from ClickHouse, and `scanned-more`; each warehouse adds its own as it
    comes.
  - RS2g — Manual and demo note.
  **Exit:** a report's sources and combine are explained before it runs, and its hints name
  the columns and filters to move.
- **RS3 — Larger than memory on one machine:**
  - RS3a — Plan, with `allowed_directories` checked against the DuckDB in the build.
  - RS3b — The workspace on a file in a work folder, `max_disk` and the free-space check.
  - RS3c — Typed loading when the type is known, the bounded queues.
  - RS3d — Parquet outputs by `COPY`.
  - RS3e — `stall_timeout`, and keeping the machine awake.
  - RS3f — Manual and demo note.
  **Exit:** a slow test loads 100 million generated rows across two sources, combines them past
  `memory_limit` and writes Parquet, inside `max_disk`, with peak memory within the computed
  bound.
- **RS4 — Partitions:** a source's ranges with `parallel`, a run's partitions with outputs
  appended or one file each, `max_rows_per_part`, range skew in the profile and its hint. What
  today's code changes in this is in [RS4 against today's code](#rs4-against-todays-code).
  - RS4a — Plan. Settles the decisions in that section. Docs only.
  - RS4b — A partitioned source, one range at a time: `partition` on a `[[source]]` read and
    planned, its ranges computed when the values are checked, each range a statement with
    `:part_from` and `:part_to` bound, appended to the source's one table, under the source's
    `timeout` per range and `max_rows_per_part`; the cost guard judges the whole period before
    the first range. *Exit:* a source read by month over a year of generated rows loads the same
    table as the same source unpartitioned, in twelve statements; a month past
    `max_rows_per_part` stops the run naming the month. *Tests:* reading and planning problems,
    table-driven (types, reserved names, refused combinations); the ranges of time and key,
    table-driven (month ends, leap days, a last range cut short, too many ranges); runs over a
    fake opener that records each statement's bound values; the statement the cost guard
    estimates.
  - RS4c — Ranges at once: `parallel` statements of one source feeding its one queue; each
    range's numbers in a `part_done` event and in the source's entry in the history; *month 7 of
    12* in the tab and on `--progress`'s line. *Exit:* with `parallel = 3`, never more than three
    of a source's twelve statements are open at once, and the table is the same as with one.
    *Tests:* a fake opener that counts open statements; events and history of each range; the
    progress line's words.
  - RS4d — A partitioned run: `[report] partition`, each range run on its own (sources loaded,
    the database locked, the combine query run) in a workspace made for it and removed after
    it; outputs a file per range when the file name says `part_from` or `part_to`, appended to
    one file otherwise (CSV and Excel); the preview and the charts over every range; every file
    put in place together when the last range is done. *Exit:* a report over a quarter by month
    writes the same CSV as the same report unpartitioned over the quarter (the guard
    [Risks](#risks) asks for), and a Parquet file per month. *Tests:* the run's ranges and file
    names, table-driven, with names two ranges would share; partitioned against unpartitioned
    runs on generated data; a failed range leaving no file; the workspace of each range gone.
  - RS4e — Appended Parquet: each range's rows copied to a part file in the staging folder, and
    the parts joined in order into the output's file by DuckDB once the last range is done.
    *Exit:* a report by month appending to one Parquet file writes the rows, in order, of the
    same report unpartitioned. *Tests:* order and rows of the joined file; `max_size` on it.
  - RS4f — Range skew in the profile: a partitioned source's ranges, and a partitioned run's,
    as segments of their lanes in the Profile's timeline, each with its numbers on hover; the
    `range-skew` hint. *Exit:* a run whose seventh month takes six times the median month shows
    it as the long segment, and the hint names it. *Tests:* the hint's rule, table-driven at its
    thresholds; the timeline's segments from a kept run.
  - RS4g — Manual, demo note and the exit test.
  **Exit:** a year of generated events, read by month, writes a Parquet file per month with the
  combine's memory that of one month: `crates/dbrover-app/tests/report_at_scale.rs` gains an
  ignored test that runs a partitioned report through the real worker and checks twelve files,
  every row, and that each month's DuckDB peak and the worker's peak memory stay within those
  of the same report run over its largest month alone.
- **RS5 — Resume:** work folders kept for large runs, the manifest, **Resume**, **Pause** and
  **Discard**, `--resume`; job ids and page tokens for C0's job engines; `as_of`; then
  incremental sources and their cache. What today's code changes in this is in
  [RS5 against today's code](#rs5-against-todays-code).
  - RS5a — Plan. Settles the decisions in that section. Docs only.
  - RS5b — Finished ranges keep their files: in a partitioned run, a range's own files go in
    place when the range ends; appended outputs, the charts of the result and the preview are
    written at the end from each range's rows, kept in the run's work folder, with RS4e's
    Parquet parts beside them. *Exit:* a run that fails in its seventh month leaves six monthly
    files in place and no appended file, and an uninterrupted run writes what RS4's did.
    *Tests:* a failed range's files; the appended CSV, Excel and Parquet, the charts and the
    preview against RS4's; the parts gone after the run.
  - RS5c — The manifest and kept work folders: `manifest.json` written as each unit ends;
    `resumable_above`; a failed, stopped or killed run keeps its work folder past it; the
    history marks a kept run; kept folders expire, count against a total, and are discarded.
    *Exit:* a run killed in its seventh month past `resumable_above` keeps a folder whose
    manifest names six ranges, and one under it keeps nothing. *Tests:* the manifest's units
    as a run goes, table-driven over failures; keeping or removing at the threshold, on the
    worker's side and the app's; expiry and the total.
  - RS5d — Resuming a partitioned run: `dbrover report run --resume <run id>` and
    `--from-scratch`; what the manifest names checked against the report, its values and its
    data sources; finished ranges skipped. *Exit:* the milestone's exit test. *Tests:* each
    change that refuses a resume, and its message; a resumed run's events and files against an
    uninterrupted run's.
  - RS5e — Resuming the combine's sources: a kept workspace's finished sources, and a
    partitioned source's finished ranges, read no more. *Exit:* a run killed while loading its
    third source resumes with the first two loaded, and writes what an uninterrupted run does.
    *Tests:* a partitioned source's half-read range removed; column types across a resume.
  - RS5f — In the app: **Resume**, **Pause** and **Discard** in the report's tab, and Settings ›
    Reports' **Keep a stopped run past** and **Kept runs may hold**. *Exit:* a paused run
    resumes from the tab. *Tests:* the tab's buttons for each state, kittest.
  - RS5g — Jobs and `as_of`: a job engine's job kept in the manifest and read again on resume;
    `as_of = "run_start"`. *Exit:* a resumed source on a fake job engine reads its kept job
    without sending its query again. *Tests:* a kept job, an expired one, and `:as_of`'s value
    across a resume.
  - RS5h — Incremental sources: `incremental` on a source, its rows kept in `report-cache/`,
    `:after` bound from them. *Exit:* a second run reads only rows newer than the newest kept
    less `lookback`, and its table equals a full read's. *Tests:* reading and planning
    problems; the cache over three runs with late rows; **Run from scratch**.
  - RS5i — Manual, demo note and the exit test's numbers.
  **Exit:** a run whose worker is killed in its seventh range resumes, reads ranges 7 to 12
  only, and writes files equal to an uninterrupted run's. Met on 2026-11-12 (see
  [RS5's exit test](#rs5s-exit-test)). Built: RS5a to RS5g and RS5i; RS5g without kept jobs,
  and RS5h not at all (see the as-built notes under
  [RS5 against today's code](#rs5-against-todays-code)).
- **RS6 — Columnar sources:** ADR 0007, `ResultCursor::next_batch`, Arrow batches appended to
  the workspace, DuckDB sources with a query and ClickHouse's `ArrowStream` first. To land
  before C5 and C6, so their drivers implement it from the start. C3 started before it
  (2026-11-05) with text rows, which `next_batch`'s default keeps. Its layers, RS6a to RS6f, and
  what today's code changes in it, are in [RS6 against today's code](#rs6-against-todays-code).
  **Exit met** on 2026-11-13 (see [RS6's columnar path](#rs6s-columnar-path)): ClickHouse's
  columnar rows a second are 1.7 times its text path's, and the four accounting examples match
  their checked-in files.
  **Exit:** the RS1 benchmark shows the columnar path ahead of the text path on ClickHouse, and
  the four accounting examples still match to the last digit.
- **RS7 — Lakes and warehouses in the combine:** C2's buckets attached to the workspace, with
  their secrets, `allowed_directories` for their prefixes, and the cost guard's estimate from
  listings and footers; then, engine by engine after C3 to C5, `handoff` through a bucket.
  **Exit:** on MinIO in CI, a report joins a PostgreSQL source with a partitioned Parquet lake,
  and the profile shows the lake's bytes read far below its size.
- **RS8 — Reports over MCP:** the read-only tools above, and the decision on running reports.
- **RS9 — Composing reports:** independent of RS1 to RS8 except `reuse`, so it can be
  scheduled at any point after RP5.
  - RS9a — Plan.
  - RS9b — The report file: `report` and its `params` on a source, read and planned, with the
    graph of reports, cycles and depth refused, and every problem naming both reports.
  - RS9c — Running a report source: inner sources in their schema, inner combines locked and
    innermost first, the result as the outer source's table.
  - RS9d — The tab and the profile: **Used by**, nested lanes, and file hashes kept.
  - RS9e — Shared SQL functions, if reports repeat expressions by then.
  - RS9f — `reuse`, once RS5's cache exists.
  - RS9g — Manual and demo note.
  **Exit:** a report built on the valuation example writes gains by wallet that equal a sum
  over the valuation's own CSV file; a cycle of two reports is a problem in both files.

## RS1 against today's code

Settled on 2026-11-09, when RS1 started, by reading the plan against the code as of 0.31.0.
Where the code changes the plan, the plan above still says what it said; this section wins.

| The plan said | Today's code | RS1 does |
| --- | --- | --- |
| Record the report file's hash with each run. | VC1e keeps the Git blob id of the text each run ran (`report_runs.version`, the `started` event's `version`). | Uses it as the file's hash. No second hash. |
| Hint ids and `quiet_hints` (RS1e). | RP4g gave the two hints their ids and reads `[report] quiet_hints`, refusing an unknown id. | RS1e adds six ids to `HintId`; the file format doesn't change. |
| `--progress` (RS1d), which RP6 left for later. | RP6i built it: a line of rows, bytes and seconds per running step. | RS1d adds throughput and what each step waits on to that line. |
| The hints' rules in `dbrover-report`. | RP4's hints are computed in the app (`reports/insights.rs`) from the history's `ReportRun`, a `dbrover-store` type; `dbrover-report` and `dbrover-store` don't depend on each other. | The rules are pure functions of a kept run in the app's `reports/hints.rs`. RS8 moves them where `dbrover mcp` can call them. |
| The profile in `report_runs`' `steps`, "so no schema change". | `steps` is a JSON array that 0.31 reads strictly: an object there would make it skip every newer run. | Each step's entry gains its start, phases and limits, which older versions ignore. The run's own numbers (the worker's peak memory, the machine's cores, DbRover's version, the run's time limit) go in a new nullable `profile` column, as VC1e added `version`: history schema 6. |
| Timestamps on every event. | Events say how long a step took, never when it ran. | Every line the worker writes carries `at_ms`, from the worker's start, around the event as it was: readers that ignore it (MCP, scripts reading `--json`) are unchanged. |
| A step's waits, measured by the bounded queues. | There are no queues until RS3c: a source reads a page, appends it to DuckDB, then reads the next, in one task. | RS1 measures where a step's time went: connecting, waiting on its engine for the first row, waiting for the rest, appending to DuckDB, typing its columns. "Waiting for room" comes with RS3c's queues. |
| The combine's phases and plan. | The combine step reports its first row's time, its peaks and its total. | RS1 splits it into the query until its first row and the rows to the outputs; DuckDB's operator tree is RS2. |
| `near-limit`, and the limits that differ between two runs. | The history keeps no limits. | The `started` event carries each source's caps, the combine's limits and the run's time limit, and the profile keeps them. |
| The worker's peak resident memory through `getrusage`, via `rustix`. | `rustix` 1.1 has no `getrusage`; `nix` 0.31, already in the build through `russh`, has it behind its `resource` feature. | `ru_maxrss` through `nix` on Linux and macOS, sent as the run ends; unknown on Windows. The machine's memory isn't recorded: there is no portable call without a new dependency. |
| DuckDB's memory from `duckdb_memory()` once a second. | The run's DuckDB has one connection, busy loading or running the combine. | Left for RS3, where the workspace on a file gets a second connection; RS1 keeps DuckDB's two peaks at the end, as RP4 did. |
| `dbrover report compare <report> <run> <run>`. | Runs have no id outside SQLite. | A run is named by its `report_runs` row id, which the history now returns with each run and `dbrover report runs` lists. |
| A benchmark of the text and typed paths, per engine. | Every source loads as text, then is cast; declared columns (RP3) only choose the types. | RS1g measures SQLite, DuckDB, PostgreSQL and MySQL (from the repository's Docker Compose file), each with inferred and declared columns, on data made by DuckDB's `range()`. These are the numbers RS3c's typed loading and RS6's columnar path are judged by. |

## RS2 against today's code

Settled on 2026-11-10, when RS2 started, by reading the plan against the code as of RS1's merge
(0340e72). Where the code changes the plan, the plan above still says what it said; this
section wins.

| The plan said | Today's code | RS2 does |
| --- | --- | --- |
| DuckDB's operator tree from its profiler, through `duckdb-rs` (a risk to check). | `duckdb-rs` 1.10506 (DuckDB 1.5.6) answers `get_profiling_info()` with a tree of operators, each a map of metrics. The workspace already profiles every statement (`no_output`), but `custom_profiling_settings` names only its two peaks. | RS2b adds each operator's name, type, time, rows and extra information to those settings, and the workspace writes the tree as JSON in the shape of DuckDB's own JSON profile (its metric names in lower case). `dbrover-plan` reads that and `EXPLAIN (FORMAT JSON)` as one format, `DuckDbJson`; a workspace test pins the metrics' names. |
| The combine's plan and the columns each scan used, in the profile. | `combine_done` carries the two peaks; a source's entry in `steps` keeps no columns. | `combine_done` carries the plan's JSON, and the combine's entry in `steps` keeps it; a source's entry keeps the columns it loaded, each with its type and the bytes received for it, which `unused-columns` weighs. No schema change: `steps` is JSON, and older versions ignore the new fields. |
| The Profile's data funnel. | RS1c drew the timeline and the bottleneck; there is no funnel. | RS2c draws it: rows and bytes each source returned, the rows its scan passed on in the combine's plan, and the rows written. RS2f adds the bytes scanned. |
| Explain report, in the tab and `dbrover report explain`. | A run happens in `dbrover report-worker`; the app's own process never connects for a report. | Explaining happens in the worker too: the request says `explain`, and the worker answers with one `explained` event. The tab's button and the command both start it, so the guards and the connections are a run's. |
| Each source through W10's formats and new DuckDB and ClickHouse readers; Trino's, Athena's, BigQuery's and Snowflake's as C3 to C6 bring them. | W10 reads PostgreSQL, MySQL, MariaDB and SQL Server. C3 (Athena, Trino) and C4 (BigQuery) are built. `Dialect::plan_request` has no DuckDB or ClickHouse; BigQuery's dry run is its `estimate_cost`. | RS2d draws PostgreSQL, MySQL, MariaDB and SQL Server through W10, DuckDB through RS2b's reader and ClickHouse through a new reader of `EXPLAIN json = 1, indexes = 1`. SQLite and Trino show their `EXPLAIN` as text. BigQuery shows its dry run's bytes. Athena's `EXPLAIN` runs as a query, so it is left out with the engines that have no `EXPLAIN`, and the source says so. Readers of Trino's and Athena's `EXPLAIN (TYPE IO)` wait for a report that needs them. |
| A lookup's source explained with the statement its last run's first batch sent. | The profile keeps no statement. | A lookup's source is explained with a list of one `NULL`, as a run sends when its lookup finds no value, and its explanation says so. |
| The combine explained over empty tables, with the last run's row counts as DuckDB's estimates. | DuckDB estimates from a table's own rows, and an empty table has none: it takes no row counts from outside. The history keeps no source's columns. | The combine is explained over empty tables of the columns the last finished run loaded (kept since RS2c), for its plan's shape (scans, the columns read, filters, aggregates), which the plan-based hints read. Its costs aren't shown. A source no run has loaded yet stops the combine's explanation, naming it. |
| C0's cost guard over every estimate, beside the source's limit. | `estimate_cost` answers on DynamoDB (a whole table) and BigQuery (a dry run's bytes); a run refuses what the guard holds. | Each source's explanation shows its estimate, its data source's `ask_above_bytes`, and whether a run would be refused. |
| Bytes scanned: ClickHouse's `X-ClickHouse-Summary`; each warehouse's own as it comes. | ClickHouse sends that header before the rows: it is complete only for a result that fits its output buffer, unless `wait_end_of_query` holds the whole result on the server. C3's and C4's cursors keep each job's final statistics. | RS2f reads `ResultCursor::progress`, which job engines already answered once a statement ends: BigQuery's `totalBytesProcessed`, Athena's `DataScannedInBytes`, Trino's last `processedBytes`. ClickHouse's cursor now answers it from the header's `read_bytes`, and only when the header has all of it: when it read every row of its `total_rows_to_read`. A short summary is left out rather than shown as less than was scanned (this row first said *at least*; as built, nothing). So RS2f covers the three warehouses that came before RS2, not only ClickHouse. |
| Plan-based hints, among the rules in the app. | RS1e's rules are pure functions of a kept run, in the app's `reports/hints.rs`. | RS2e's three rules join them, reading the combine's plan through a pure summary in `dbrover-plan` (each scan's table, columns and filters, and whether an aggregate reads one scan with no join under it). **Explain report** lists the same hints for the explained combine. |

As built, three more facts of DuckDB 1.5.6, each pinned by a test:

- The profiler writes some extra-information keys as DuckDB names them inside
  (`__projections__`, `__estimated_cardinality__`), and a scan's table with its catalog and
  schema (`memory.main.orders`); the reader names keys as `EXPLAIN` does, and the plan's table
  reads drop the catalog and schema.
- Planned over empty tables, DuckDB's statistics turn a filtered scan into an empty result,
  which hides the very columns and filters the hints read. The explain's workspace switches off
  the two optimizers that plan from what a table holds (`statistics_propagation` and
  `empty_result_pullup`) before it is locked; a run's isn't touched.
- DuckDB lists the filters a join derives under `Dynamic Filters`, apart from a scan's own
  `Filters`, so `filter-one-source` reads only the latter.

## RS3 against today's code

Settled on 2026-11-10, when RS3 started, by reading the plan against the code as of RS2's merge
(b1a375f) and trying DuckDB 1.5.6, the version in the build. Where the code changes the plan,
the plan above still says what it said; this section wins.

| The plan said | Today's code, and DuckDB 1.5.6 | RS3 does |
| --- | --- | --- |
| `allowed_directories`, set before the lock to the run's staging folder alone, lets `COPY` write there and nowhere else (a risk to check). | Checked. It must be set while external access is on: afterwards DuckDB refuses it (*Cannot change allowed_directories when enable_external_access is disabled*). Once locked, `COPY … TO` a file in the folder works, for Parquet and CSV alike; a file anywhere else, `staging/../other/x.parquet` included, is refused (*file system operations are disabled by configuration*). DuckDB adds its own `temp_directory` to the list. | RS3d sets it to the run's output staging folder, before the lock, and a workspace test pins all three behaviours. |
| The workspace on a file in the run's work folder, in the data folder's `report-runs/<run id>/`, with a setting for another disk. | `Workspace::open` makes an in-memory database (RP2b); it spills to `duckdb/` in the output folder's staging folder. A run has no id until the history keeps it. | RS3b opens the run's database as `workspace.duckdb` in a work folder named as the staging folder is (process and time), in `report-runs/` of the data folder or of Settings › Reports' **Work folder**, and spills to its `spill/`. The worker makes it and removes it however the run ends; the app's side removes it again after a worker it killed, as it does the staging folder. Explaining a report keeps an in-memory workspace: its tables are empty. |
| Free space read from DuckDB, whose `max_temp_directory_size` defaults to 90% of it. | DuckDB 1.5.6 reports that default as the text *90% of available disk space*, never as a number, and has no function that says a disk's free space. | The free space comes from `statvfs`, through `rustix`'s `fs` feature: `rustix` is in the build already, so no new crate. On Windows it is unknown, and `max_disk` alone bounds a run. |
| `max_disk` checked against the free space before the run and as it grows. | The app's side of a run already has a watchdog thread, which kills the worker at the run's time limit (`dbrover_engine::WorkerRun`). | The run's disk budget is `max_disk`, or 90% of the work folder's free space when the run starts if that is less, as DuckDB's own default for spilling is: a run isn't refused for a limit it may never reach. The watchdog wakes each second, adds up the work folder and the staging folder, and past the budget kills the worker, which fails at `max_disk`, saying whether the limit or the disk was the bound. DuckDB's `max_temp_directory_size` becomes the smaller of `max_temp_size` and the budget. `max_disk` is a `[report]` key and a Settings › Reports default, 20 GiB: twice the default spill. |
| Typed loading when the type is known: declared columns (RP3), or a driver's type with its precision. | `ColumnTyper` types every column after its last row; the driver's type decides alone for decimals with a precision and for floats, and text of dates becomes dates (SQLite keeps dates as text). DuckDB's appender casts text into a typed column as it appends: a value that doesn't fit fails the append with *Could not convert string "abc" to DECIMAL(18,2)*. | RS3c makes the table with the types known before the first row: a declared column's (but `decimal` without a precision), a driver's `DECIMAL(p,s)`, and a float as `DOUBLE` on every engine but SQLite, whose types are only affinities. The other columns load as text, and only those whose values choose another type are rewritten, by `ALTER TABLE … ALTER … TYPE`, not the table by `CREATE TABLE … AS SELECT`. |
| The appender takes each value as text, the text path's bottleneck (RS1g). | Measured on 2,000,000 rows of the benchmark's four columns, in release mode: text rows into text columns 1.35 s (today's path); text rows into typed columns 1.00 s; the same page as one Arrow batch of text into typed columns 0.84 s; parsed into typed Arrow arrays in Rust first, 0.72 s. Typing all four columns afterwards took 0.12 s, and rewriting one column 0.03 s. Most of the cost is DuckDB storing strings, not the calls. | RS3c appends each page as one Arrow batch of text, which DuckDB casts into the typed columns. That needs the `duckdb` crate's `appender-arrow` feature, which adds no crate: `arrow` 58 and `num` are already in `Cargo.lock`. Parsing values in Rust is RS6's columnar path. |
| A bounded queue of a few batches between a source's reader and its loader, and the time each side waits. | A source reads a page, appends it, and reads the next, in one task; its phases add up to its time, which the Profile splits by share, and `load-bound` reads `load_ms`. | A queue of four pages, both sides in the source's own task, so receiving the next page overlaps appending the last. `load_ms` becomes the time DuckDB held the source: the reader waiting for room in the queue, and for the last pages to be appended. So the phases still add up, the Profile and `load-bound` keep their meaning, and the progress line's *loading* share is the same wait. |
| Memory outside DuckDB is computable: `memory_limit` plus each running source's queue, plus the writers' buffers. | Pages are 1,000 rows, of any width. RS1b records the worker's peak resident memory. | The bound is `memory_limit` + for each source of the widest round, six pages (the queue's four, the one being read, the one being appended) of its largest page's memory + the worker's own baseline, measured by RS3f's exit test. The page's memory is counted as its values' bytes and each value's `String`. |
| Parquet outputs, written by DuckDB's `COPY (combine query) TO` from the workspace. | RP1 refuses Parquet. The combine query's rows reach the outputs, the preview and the charts as pages of text. | RS3d writes a Parquet output with `COPY (combine query) TO` its staging file, with `compression` (`zstd`, or `snappy`) as the export's preset has it. The other outputs, the preview and the charts read the rows back from that file with `read_parquet`, so the combine query runs once. A Parquet output takes the result's columns as DuckDB types them: `columns` is refused on it (name and cast them in the query), and so is a Parquet output in a report without `[combine]`. `max_size` holds the file's size. |
| `stall_timeout`: no row, batch, range or DuckDB work for 10 minutes stops the run as stuck. | Progress events come every second, also while a step waits (RS1d), so their arrival says nothing; DuckDB's work shows in no event. | The worker adds its processor time so far (`getrusage`, through `nix`, as RS1b's peak memory) to each progress event. The watchdog counts as progress any other event, a progress event with more rows or bytes or another batch, and a second or more of processor time since the last progress; past `stall_timeout` without any, it kills the worker, which fails at `stall_timeout`. Waiting on an engine that returns nothing is a stall, so a warehouse's long first row needs `stall_timeout` raised with the source's `timeout`. Where processor time is unknown (Windows), every progress event counts, and only a worker that stops sending them is stuck. `[report] stall_timeout` and a Settings › Reports default. |
| Keeping the machine awake during a run longer than a few minutes: `caffeinate -i -w <pid>` on macOS, `systemd-inhibit` on Linux. | Nothing. | After two minutes of a run, the watchdog starts `caffeinate -i -w <worker pid>` on macOS, and on Linux, where it exists, `systemd-inhibit --what=idle:sleep --mode=block tail --pid=<worker pid> -f /dev/null`; neither needs elevated rights, and both end with the worker, or when the run ends. Settings › Reports' **Keep the computer awake** turns it off. Tests start a stand-in program, never these. Windows waits, as the plan says. |
| DuckDB's memory from `duckdb_memory()` once a second, left for RS3 by RS1. | `duckdb_memory()` reads zero once a statement ends; sampling during one needs a second connection on a busy database. | Left out: the profile keeps DuckDB's peaks and the worker's, which the exit test needs. Sampling waits for a view that would show it. |
| A work folder kept after a failed run past `resumable_above`, and its manifest. | — | RS5. RS3's work folder is always removed. |

As built, five more facts of DuckDB 1.5.6, each pinned by a test:

- A transaction's new rows stay in memory, spilled to the temporary folder, until it commits:
  RP2's `CREATE TABLE … AS SELECT` of a large source, and a table loaded in one transaction,
  counted against `max_temp_size` on a file workspace. Loading commits each page, which DuckDB
  writes to the file, and `ALTER … TYPE` rewrites one column; a workspace test loads a table
  four times its `memory_limit` under a 4 MiB spill limit.
- Appending an Arrow batch of text into typed columns casts it, but a value that doesn't cast
  fails the whole batch with *type mismatch in AppendDataChunk*, naming no value. The loader
  then appends that page row by row, whose error names it.
- A database on a file is its own catalog, named after the file: the profiler's tables read
  `workspace.main.orders` rather than `memory.main.orders`, which the plan's readers drop.
- `COPY (query) TO` takes the combine query's bound parameters (`$1`) as the query does, and
  answers with the rows it wrote, which the driver reads as a change's count.
- `allowed_directories` holds DuckDB's own spill folder as well as the one set.
- DuckDB's buffer pool passes `memory_limit` at its peak, while a source loads: it allocates
  before it writes pages out. On a workspace on a file loading 50 million rows its own peak
  measure read 0.95 GB under a limit of 512 MiB, 2.0 GiB under 1 GiB and 1.2 GiB under 2 GiB,
  while its use between statements stayed at the limit; a workspace in memory reached 2 GiB
  under 1 GiB with 30 million rows. So the memory bound counts DuckDB at twice
  `memory_limit`: 2 × `memory_limit` + the queues + the worker's own, which the exit test
  measures as the peak of the same report over 1,000 rows (about 70 MB).

## RS4 against today's code

Settled on 2026-11-11, when RS4 started, by reading the plan against the code as of 0.32.0
(877f3c7). Where the code changes the plan, the plan above still says what it said; this section
wins. Hash partitions and state carried across ranges stay out, as [Recommended
path](#recommended-path) says.

| The plan said | Today's code | RS4 does |
| --- | --- | --- |
| `partition = { from = ":since", to = ":until", every = "1 month", parallel = 2 }`, ranges of time or of an integer key, never guessed from the data. | Parameters are typed (`date`, `timestamp`, `integer` and others), checked in `Plan::values`; file names already format dates with `jiff` (`{since:%Y-%m}`). | `from` and `to` are each a parameter (`":since"`) or a literal: a TOML date, a TOML local date-time, or an integer; both of one kind. `every` is a span of `jiff`'s friendly form for dates (`"1 day"`, `"1 week"`, `"1 month"`, `"1 year"`: no hours) and timestamps (hours and minutes too), and a positive integer for keys. A range is half open, `[part_from, part_to)`; each bound is `from` plus *k* × `every` counted from `from`, so months that start on the 31st keep their day where a month has it, and the last range is cut at `to`. A `from` not before `to`, or more than 10,000 ranges, is a problem of the values, named before anything runs. |
| Each range binds `:part_from` and `:part_to`, as a lookup binds its list; neither can be one of `params`. | A lookup's list is written into the query's text in every language; the report's own parameters are bound by SQL drivers and written in as literals for the others (`statement.rs`). | The range's bounds go as the report's own parameters go: bound by SQL drivers, of the range's type (date, timestamp or integer), and written in as literals in other languages. A partitioned source's query uses at least one of them, else partitioning only repeats it; `[report] params` naming either is a problem. |
| Each statement stays under the engine's and the source's timeout; `max_rows_per_part` holds per range. | A source's `timeout` runs from its first statement, every batch of a lookup inside it. | A partitioned source's `timeout` bounds each range's statement on its own. `max_rows` and `max_bytes` bound the whole table, `max_rows_per_part` each range; it is a source key with no default in Settings › Reports, since it is the report's own knowledge of its data, and it is a problem on a source that isn't partitioned. |
| The cost guard judges a source's estimate across all its ranges. | The engine asks the cost guard before each statement (`check_cost` in the engine's `report_source.rs`); DynamoDB, Athena and BigQuery have one. | The first range's statement is judged as the whole period: the guard estimates the query with `:part_from` and `:part_to` bound to `from` and `to`, whose rows are every range's. The others aren't judged again, and don't start before the first is open. BigQuery's minimum of 10 MB a statement isn't counted once a range; the bytes each range scanned are in its numbers (RS4c). |
| `parallel` statements at once where the engine has the capacity, one at a time by default. | A source's reader and loader share its task and a queue of four pages (RS3c); each statement opens its own connection. | `parallel`, 1 to 16, default 1: that many statements of the source run at once, each on its own connection, all feeding the source's one queue and table. Ranges start in order; the table's rows are in no particular order, as a SQL table's never are. The worker's memory bound counts `parallel` + 5 pages for such a source instead of RS3c's six. |
| (Not said: a lookup and a partition on one source.) | A lookup sends a statement a batch of values. | A source has a lookup or a partition, not both: both multiply statements, and no report the plan is checked against needs them together. |
| Two kinds of partition, which a report can combine. | — | Not in one source: a partitioned run's sources aren't partitioned themselves in RS4, since a source read by day inside a run by month needs its range bound to the run's; that waits for a report that needs it. A partitioned source in a report without `[combine]` is a problem pointing at `[report] partition`, which reads one source a range at a time with no combine step. |
| A partitioned run runs the whole report once per range, the combine step on one range at a time. | Once its sources are in, the run's DuckDB is locked (`lock_configuration`), which can't be undone (RP2, RS3d). | Each range opens a workspace of its own in the run's work folder, loads the sources, locks it, runs the combine query, and closes and removes it before the next range starts: a range's memory, spill and file are its own. Every query of the run may use `:part_from` and `:part_to`, the combine query too, and one must. Ranges go one at a time, which is what bounds the run, so `parallel` is a problem there. A report without `[combine]` reads its one source a range at a time into its outputs. |
| Outputs appended to one file, or one file per range when `file` names `part_from` or `part_to`; Excel appends within its row limit. | Each output's file name is rendered once per run (`Plan::values`); rows are written as they stream, and every file is put in place once all are complete, so a failed run leaves no file. | A file naming `part_from` or `part_to` is one per range, its name rendered with the range's bounds; two ranges rendering one name is a problem of the values. Any other file is appended to by every range: a CSV or Excel writer stays open across ranges (one header, Excel going on to a new sheet only with `continue_on_new_sheet`), and a Parquet file is joined from a part a range (RS4e). Every file waits under its partial name in the staging folder and goes in place with the others when the last range is done, so a run that fails in its seventh range still leaves no file. RS5 keeps the finished ranges' files instead. |
| (Not said: charts.) | A chart draws the result, or its own query over the sources' tables (CH3). | In a partitioned run, a chart of the result draws every range's rows, into one file. A chart with its own query is a problem, since it would read one range's tables, and so is a chart file naming the range. |
| (Not said: limits in a partitioned run.) | — | Each range applies the sources' caps and the combine step's limits on its own, since it loads its tables anew; the run's `timeout`, `stall_timeout` and `max_disk` hold for the whole run, and an appended file's `max_size` for the whole file. The cost guard judges each source's first range as the whole period, as for a partitioned source. |
| Progress in ranges (*month 7 of 12*). | Progress carries a lookup's batch, shown as *batch 2 of 5* (RP3). | `started` gives each partitioned source's number of ranges, the word for one (`month` when `every` is one calendar unit, else `range`) and `parallel`, and the run's partition the same way; progress says how many ranges are done and running. A `part_done` event carries each range's bounds and numbers. |
| The skew between ranges, shown in the profile; `range-skew`: a range that took ≥ 5× the median range. | The history keeps one entry a step, which the timeline draws as a lane split into phases (RS1c); hints are rules over a kept run in the app (RS1e). | A source's and the combine step's entries keep their ranges (`parts`: bounds, start, time, rows, bytes, bytes scanned, and the combine's peaks); a partitioned run's per-range events are summed into one entry a step. The lane draws a segment a range. `range-skew` names a range that took at least 5× the median of its step's ranges, when there are three ranges or more and it took 10 seconds or more, so a run of short ranges isn't told about noise. No schema change: `steps` is JSON, and older versions ignore the new fields. |
| A range is the unit a resumed run skips (RS5). | RS3's work folder is removed however the run ends. | Ranges are numbered from 1 and computed from the file and the values alone, never from data, so a resumed run computes the same ones, and RS5's manifest can record them by number and bounds. RS4 appends each range's rows straight into its source's table, so a range that fails halfway leaves rows there: harmless while a failed range fails the run, and what RS5 settles (a range's rows kept apart until it ends, or deleted by its bounds) before it skips ranges. |
| A year of generated events, read by month, with the combine's memory that of one month. | RS3's exit test runs a report through the real worker in `crates/dbrover-app/tests/report_at_scale.rs`, ignored. | RS4g adds a second ignored test there: a year of generated events, partitioned by month, a Parquet file a month; it runs the same report over its largest month alone, and checks the partitioned run's DuckDB peak and the worker's peak memory against that run's, within a quarter, since DuckDB's own peak moves by that much between runs of one size (RS3's as-built notes). |

As built, more that the plan above didn't say:

- **The run's outputs became a sink that outlives a range.** RP1's run wrote its files from one
  feed and put them in place at its end; RS4d moved the writers, the preview and the charts into
  one object that a partitioned run's ranges all write to, which completes a range's own files
  at the range's end and puts every file in place once the last range is done. An unpartitioned
  run is the same with one range.
- **Each written file says which output it is of** (`output_written`'s `output`), since a
  partitioned run's output writes a file a range; the history numbers its files by it.
- **A partitioned run's steps are summed in the history**, each range kept as a part of its
  step: the sources' and the combine step's numbers add up, the combine step's peaks are its
  largest range's, and its plan is the last range's. Hints read the sums, and `range-skew` the
  parts.
- **With `parallel` above one, a source's phases add up every range's,** so they can come to
  more than the source's own time; the Profile draws their shares, and ranges read at once keep
  the source's one bar, with a strip of its ranges under it.
- **A range that returns no rows types its columns as text** on an engine that doesn't say its
  columns' types (SQLite, text protocols), as any source without rows does; a combine query
  that adds them up fails on that range. The manual says to declare the columns or cast. An
  earlier range's types aren't carried over: each range's DuckDB is new.
- **DuckDB 1.5.6 reads a list of Parquet files in the list's order** with
  `preserve_insertion_order` on, its default, which RS4e's join of a range's parts relies on; a
  report test checks the joined file's rows against the ranges' order.
- **A partitioned source's ranges start in order, and the first opens alone,** so the cost
  guard's whole-period estimate comes before any other range's statement is sent.

## RS5 against today's code

Settled on 2026-11-12, when RS5 started, by reading the plan against the code as of RS4's merge
(302194d). Where the code changes the plan, the plan above still says what it said; this section
wins. Each row is a decision for the reviewer to confirm.

| The plan said | Today's code | RS5 does |
| --- | --- | --- |
| A unit is all or nothing: a source, a source's range, a lookup's batch, or a run's partition. | A partitioned run's ranges each have a DuckDB of their own (RS4d); a source's table is loaded a page at a time, each page committed (RS3c); a lookup sends a batch at a time into one table. | Three kinds of unit: **a partitioned run's range**, **a source** loaded into the workspace, and **a partitioned source's range**. A lookup's batches aren't units: a lookup source is read again whole, since its values come from another table and its batches are small by design. The combine step is never a unit: a resumed run runs it again over the loaded tables. A run without `[combine]` and without `partition` streams its one source into its files, so it has nothing to keep and isn't resumable. |
| (Not said: the files of finished ranges.) | Every file waits in staging until the last range, so a run that fails in its seventh range leaves no file; an appended CSV or Excel writer stays open across ranges (RS4d). | **A range's own files go in place when the range ends**, so a failed run leaves the finished months' files, and a resumed run doesn't write them again. The promise that a failed run leaves no file now holds for appended files and unpartitioned runs. |
| (Not said: appended outputs across a resume.) | An appended CSV or Excel output is one writer across ranges; an Excel file can't be appended to once written. RS4e's Parquet parts wait in the staging folder, which the app removes after a killed worker. | **Each range's rows are kept in the work folder**, as the sink receives them (JSON Lines of text values, so `NULL` and the empty string stay apart), and the appended CSV and Excel outputs, the charts of the result and the preview are written from them, range by range in order, once the last range is done. RS4e's Parquet parts move to the work folder too. This writes and reads the result's rows a second time, which a partitioned run's reduced result makes cheap, and it is what lets a resumed run write the same appended file as one that never stopped. When nothing but the preview reads them, a range keeps only the rows the preview still needs. |
| A partitioned source's range is a unit. | RS4 appends each range's rows straight into its source's one table, several ranges at once with `parallel`, so a range that fails halfway leaves rows there (RS4's last row). | **A range's rows carry its number** in a hidden column while the source loads; a resumed run deletes the rows of every range the manifest doesn't name as finished, and the column is dropped once the source's last range is in. Deleting by the range's bounds was rejected: the query needn't return the column it partitions on. A table a range, joined at the end, was rejected: it copies every row once more. |
| (Not said: column types across a resume.) | `ColumnTyper` types a source's columns from every value it saw, once the last row is in (RP2, RS3c). | The typer's state is kept in the manifest as each unit ends, and restored on resume. It may have seen values of a range that didn't finish; types only narrow as values are seen, and a resumed run reads that range again, so the types are those of an uninterrupted run whenever the source holds what it held. |
| A work folder kept when the run had loaded more than `resumable_above` (default 1 GiB). | RS3's work folder is removed however the run ends: by the worker, and by the app's side after a worker it killed. | `resumable_above` counts the bytes the **finished** units read, since only they are reused. It is a `[report]` key and a Settings › Reports default (**Keep a stopped run past**, 1 GiB). The worker keeps the folder when its run fails; the app's side, which removes a killed worker's folders, reads the manifest and keeps it past the threshold. **Pause** keeps it whatever it read. A kept folder is marked in its manifest, and an event, `work_kept`, tells the history. |
| The manifest records the report file's hash, the parameters, the limits, DbRover's version and each data source's identity. | Runs record the file's Git blob id (VC1e) and the typed values; a data source has an id, an engine, a host, a port, a database and a user. | Resuming refuses, and says what changed, when the blob id, a parameter's value, DbRover's version, or a data source's id, engine, host, port, database or user differs; secrets are never in the manifest. The limits are recorded but may change: a run stopped at a Settings › Reports limit resumes once the limit is raised there. A limit in the file is an edit to the file, which refuses, since DbRover can't tell which edits keep a resumed run equal to an uninterrupted one. |
| **Resume** and `--resume`. | A run has no id until the history keeps it (RS3b); `dbrover report runs` prints the history's ids. | **A run's id is the history's**: `--resume <run id>`, the id `dbrover report runs` prints. The run's profile (JSON, so no schema change) gains `kept`, its work folder's name, and a resumed run's `resumed_from`. A resumed run works in the kept folder, and the history keeps it as a run of its own. A plain run never resumes: the command line says, before it starts, which run could, and the tab offers **Resume** beside **Run**. `--from-scratch` discards the report's kept folders, and from RS5h its incremental cache, before running. |
| Work folders expire after 7 days and count against a total in Settings; **Discard** removes one at once. | Settings › Reports has **Work folder** and **Disk a run may use** (RS3b). | Kept folders expire 7 days after they were kept, and Settings › Reports' **Kept runs may hold**, default 50 GiB, removes the oldest past it. Both are applied when a run starts, from the app or the command line. A run of a report that finishes discards the report's other kept folders, which it supersedes. Folders a crashed app left unkept stay as today. |
| **Pause** is a cancel that keeps the work folder. | **Cancel** kills the worker (`RunStopper`); the run ends as cancelled. | **Pause** kills the worker as **Cancel** does and keeps the work folder; the history keeps the run as cancelled, and the tab and `report runs` say *paused* of a cancelled run that can resume. The command line's Ctrl-C stays a cancel, which keeps the folder past `resumable_above` as any stopped run does. |
| A source on a job engine keeps its job's id and last page token, and a resumed run pages on. | Athena and BigQuery submit a job and page its result (`JobService` in `dbrover-core`), but the job's id stays inside the cursor; BigQuery needs a job's location to read it from another connection. | The job's id, and BigQuery's location, are kept in the manifest when a unit's statement starts, and a resumed unit reopens the job and **reads its result from the first page**, not from the last token: the rows of the page being appended when the worker died can't be told from the others without bookkeeping a page at a time, and reading a kept result costs no query. A job the engine no longer has (BigQuery keeps a result a day) runs again, under the cost guard. |
| `as_of = "run_start"` binds `:as_of` to the run's start. | Parameters are bound by SQL drivers and written in as literals elsewhere (`statement.rs`), as `:part_from` is. | As planned: `:as_of` is the run's start as a UTC timestamp, kept in the manifest and bound again on resume; a source with `as_of` must use it, and `params` can't name it. |
| Incremental sources keep their rows as Parquet in `report-cache/<report>/<source>/`. | Parquet is written by `COPY` from the workspace (RS3d); `allowed_directories` holds the staging folder. | As planned, for sources of a report with `[combine]`: the cache is read into the source's table before its new rows, the new rows replace kept ones by `key`, and the table is written back to the cache by `COPY` once the run finishes, replacing it in one rename. The cache's folder joins `allowed_directories`. **Clear history** and `--from-scratch` clear it, and it counts against **Kept runs may hold**. |

The milestone's exit test is RS4's year of events, by month, killed in its seventh month: a third
ignored test in `crates/dbrover-app/tests/report_at_scale.rs` kills the worker once the seventh
month starts, resumes the run by the history's id, checks that only months 7 to 12 are read,
and compares every file with an uninterrupted run's.

As built, where RS5 differs from the table above:

- **A partitioned source isn't resumed range by range.** RS5e keeps a source loaded into the
  combine step's workspace as a unit; a partitioned source stopped while it read its ranges is
  read again from its first. The hidden range column the table describes needs the appender to
  know when each range's last page is in, which waits for a report that needs it.
- **Jobs aren't kept.** A source on a job engine is read again with its query on resume:
  reopening a kept job needs the job's id, and BigQuery's location, out of the driver's cursor,
  and a way to open a reader on them (`dbrover-core`'s `JobService` pages from a token already).
  Left for a later layer.
- **Incremental sources and their cache** (RS5h) aren't built.
- **`:as_of` needs no key.** Every query may use it, bound to the run's start in UTC and kept in
  the manifest, unless the report has a parameter named `as_of`, which binds its own value: a
  key whose one value is `run_start` added nothing.
- **A range's own files are put in place, and said, as it ends;** the history and `--json` see
  them before the run's preview.
- **Appended Parquet parts stay in the work folder until the run ends,** rather than being
  removed once joined, so a run that fails after joining one resumes without them missing.
- **A range a killed worker was in the middle of leaves its DuckDB behind;** a run resuming it
  removes it before the range starts again. The exit test found this.
- **The report's file and output folder are compared canonically,** so the app and the command
  line, which may name them differently, resume each other's runs.

## RS6 against today's code

Settled on 2026-11-13, when RS6 started, by reading the plan against the code as of RS5's merge
(9e11e1c) and trying ClickHouse 25.8, the version in the repository's Docker Compose file.
Where the code changes the plan, the plan above still says what it said; this section wins.
Each row is a decision for the reviewer to confirm.

| The plan said | Today's code, and the engines | RS6 does |
| --- | --- | --- |
| An optional `ResultCursor::next_batch` returning an Arrow `RecordBatch`, `None` by default. | `ResultCursor` has `next_page`, `progress` and `finish`; C3's and C4's notes left "the cursor itself" as the seam. A `None` that means both "no batches here" and "no more rows" can't be told apart. | `next_batch` lives on a small `BatchCursor` trait (`schema`, `next_batch(max_rows)`), which a cursor hands out through `ResultCursor::batches()`; `None`, the default, means its engine gives text. A caller reads a cursor through one or the other. ADR 0007 records the rule and the alternatives. |
| (Not said: when a cursor's format is chosen.) | ClickHouse answers each statement in the format the request names; Databricks's request names `ARROW_STREAM` too, and Snowflake's a result format. DuckDB's cursor fetches Arrow chunks whichever way it is read. | `Connection::execute_for_batches(sql, values)` starts a statement for a caller that will read batches; the default runs it as `execute_bound` does. DuckDB needs no override: its cursor answers both ways. A cursor started for batches still answers `next_page`, from its batches. |
| `arrow` 58 already in the build; `arrow-ipc` the one new crate. | `arrow` 58.4 comes through `duckdb`; `dbrover-core` depends on no Arrow crate. `arrow-ipc` 58.4 has no default features: its `lz4` and `zstd` codecs are optional. | `dbrover-core` depends on `arrow-array` and `arrow-schema` at the `duckdb` crate's version, so a batch reaches DuckDB's appender unconverted; the two are bumped together. `dbrover-driver-clickhouse` adds `arrow-ipc` without codecs, and asks ClickHouse for an uncompressed stream (`output_format_arrow_compression_method = 'none'`; ClickHouse's default is LZ4). |
| Arrow's `Decimal128` keeps decimals exact; the loader hands the batch to `append_record_batch`. | A table is made with the types known before its first row (RS3c's `LoadColumn`), the other columns as text, typed by their values once the last row is in (`ColumnTyper`). `DuckDbType` has nine types, which the history and a resumed run read back by name. | **Bulk types** (`dbrover_core::bulk_kind`): booleans, signed and unsigned integers, floats, `Decimal128` of up to 38 digits, `Date32`, timestamps with and without a zone, and text. Each maps onto a `DuckDbType` the table is made with before the first row: integers to `BIGINT` (`UBIGINT`'s to `HUGEINT`), floats to `DOUBLE`, a timestamp with a zone to `TIMESTAMP WITH TIME ZONE`. A **text column of a batch is typed by its values**, exactly as a page's is, with the column's type name from the driver: so a source's types are the text path's wherever the engine sends text. `DuckDbType` gains nothing. |
| (Not said: an engine's types that have no bulk equivalent.) | ClickHouse 25.8's Arrow output sends `DateTime` as `UInt32` seconds, `Date` as `UInt16` days, `Enum8` as `Int8`, `Int128` as `FixedString(16)` and `IPv4` as `UInt32`, and refuses a `UUID` column, failing the statement. DuckDB's Arrow export has lists, structs, maps, intervals, blobs and enums, which its driver already formats as text (`values.rs`). | **A driver sends a column outside the bulk types as text**, the same text its `next_page` gives. DuckDB's cursor formats such a column with the formatter its pages use. ClickHouse's asks for the query's columns first (`DESCRIBE TABLE (query)`, which plans and reads nothing), then sends `SELECT * REPLACE (toString(c) AS c, …) FROM (query)` for every column whose type isn't bulk, and the query alone when all are. So `DateTime`, `DateTime64` and `Date` reach the loader as text and are typed by their values as before; `Date32` comes as a date. |
| (Not said: which sources read batches.) | A source loaded into the workspace may declare its columns (`Shaped` checks and rewrites each value as text, RP3), return documents (ADR 0006), send a lookup's batches or a partition's ranges; a report without `[combine]` streams its source into the outputs as text. | **A source reads batches when it is loaded into the combine step's DuckDB, declares no columns, returns rows, and its engine gives batches.** Lookups' statements and a partitioned source's ranges read them too, all into the one table. Declared columns stay on the text path (RS3c made it fast), and so do documents and a report without `[combine]`, whose rows go to the outputs as text. `columnar = false` on a `[[source]]` keeps it on the text path whatever its engine: for the benchmark's comparison, and for a source whose non-bulk text differs between the paths. |
| A source's caps count rows and bytes; the profile keeps the bytes each column received (RS2c). | A page's bytes are its values' text, `NULL` counting nothing. | A batch's bytes are its values' size in their types: a text value's bytes, a fixed-width value's width, `NULL` nothing. So `max_bytes` and `unused-columns` weigh a columnar source by what it moved, which is less than its text for numbers. |
| The memory outside DuckDB is computable from the queue (RS3c). | Six pages of a source's largest page; pages are 1,000 rows. | A batch is at most `BATCH_ROWS` (8,192) rows: DuckDB's chunks are 2,048, gathered up to that; ClickHouse's blocks (up to 65,409 rows) are cut into slices, which share the block's memory until the last slice is appended. The bound counts, for a columnar source, six batches of its largest **block**. |
| `next_batch` for C4 to C6. | C3 (Athena, Trino) and C4 (BigQuery) are built on text; C5 and C6 aren't started. | Nothing changes for C3 and C4. C5 and C6 implement `batches()` from the start: the trait is shaped by their needs (a format chosen when the statement starts; batches of the service's own size). BigQuery's Storage Read API stays where C4 left it. |
| Exports, too, may carry Arrow (ADR 0007's wording). | Exports (W7, W8) write text through `dbrover-export`'s writers. | Not in RS6: reports only. An export to Parquet from a columnar engine is the next candidate, once a user needs it. |
| **Exit:** the RS1 benchmark shows the columnar path ahead of the text path on ClickHouse, and the four accounting examples still match. | RS1g's benchmark has SQLite, DuckDB, PostgreSQL and MySQL, no ClickHouse; the examples run on SQLite, MongoDB, PostgreSQL, DynamoDB and DuckDB (`report_examples.rs`). | The benchmark gains ClickHouse (rows from `numbers()`), and a third column kind beside *inferred* and *declared*: *columnar*, the default now for DuckDB and ClickHouse, with *inferred* run as `columnar = false`. The exit compares ClickHouse's columnar rows a second with its inferred ones on the same build, and every engine's against RS3f's table. The examples' DuckDB prices source reads batches now, and the four still match their checked-in files. |

Layers, one pull request each, in one stack:

- **RS6a — The plan:** this section, [ADR 0007](adr/0007-arrow-on-bulk-paths.md) and the
  roadmap's layers. Docs only.
- **RS6b — Batches in `dbrover-core`:** `BatchCursor`, `ResultCursor::batches`,
  `Connection::execute_for_batches`, `bulk_kind`. *Tests:* the bulk types, table-driven; a
  text cursor hands out no batches.
- **RS6c — Loading a batch:** the workspace's `TableLoader::append_batch`; in
  `dbrover-report`, a reader's next part for loading (a page or a batch), the table made from a
  batch's schema, text columns typed by their values, bytes counted, `columnar = false`.
  *Exit:* a source of batches on a stand-in opener loads into the same table, with the same
  types, as its text. *Tests:* each bulk type's table type; a text column typed by its values;
  caps and column bytes over batches; a lookup's and a partition's batches into one table.
- **RS6d — DuckDB's batches:** the DuckDB cursor's `batches()`, chunks gathered up to
  `BATCH_ROWS`, non-bulk columns as their text; the engine's report source started for batches.
  *Exit:* a report over a DuckDB source writes what it wrote on the text path, and its profile
  says it read batches. *Tests:* every DuckDB type through a batch, against `next_page`'s text;
  a cancel during a batch.
- **RS6e — ClickHouse's `ArrowStream`:** `arrow-ipc`; `execute_for_batches` describing the query
  and casting non-bulk columns to text; the stream decoded as it arrives, blocks cut into
  batches; `next_page` from batches. *Exit:* a report over a ClickHouse source writes what the
  text path writes. *Tests:* against the Compose file's ClickHouse, every type of the probe
  above; a statement killed mid-stream; an empty result; a query that `DESCRIBE` refuses.
- **RS6f — Manual, benchmark and the exit:** the manual's columnar note and `columnar`, the
  benchmark with ClickHouse and the *columnar* rows, the as-built notes, and the roadmap.

As built, where RS6 differs from the table above:

- **A DuckDB batch is one chunk,** of at most 2,048 rows, or the part of one asked for: not
  chunks gathered up to `BATCH_ROWS`, which would copy DuckDB's arrays into larger ones. The
  engine asks for 8,192 rows at a time; ClickHouse's blocks are cut to that.
- **ClickHouse's `Float32` is sent as text.** As a `DOUBLE`, a `Float32` shows its binary
  value's digits (`0.1` becomes `0.10000000149011612`), where ClickHouse's text shows the
  shortest that reads back, which is what the text path loaded. `Float64` is a batch's.
- **A ClickHouse `UInt64` loads as `HUGEINT`,** where the text path typed small values as
  `BIGINT`: an unsigned 64-bit value may need 20 digits. `numbers()` is `UInt64`; cast it with
  `toInt64` in the query for a `BIGINT`.
- **A ClickHouse statement with duplicate column names, or one `DESCRIBE` refuses** (not a
  query, or with a `FORMAT` of its own), is read as text: its columns can't each be replaced by
  their text.
- **A ClickHouse cursor started for batches answers `next_page`** with each value as Arrow
  formats it, not as ClickHouse's JSON writes it; only a caller that asked for batches gets one.
- **The profile doesn't say which path a source took.** Its columns' types show it (a column of
  only `NULL`s keeps its engine's type), and `columnar = false` is in the report's file.
- **The four accounting examples don't read batches:** their DuckDB prices are attached, and
  every other source declares its columns or is a document database. They still match their
  checked-in files, which is the check that the text path is unchanged; the columnar path's own
  check is `crates/dbrover-engine/tests/report_columnar.rs`, which runs a DuckDB and a
  ClickHouse source on both paths and compares the files.
- **Lookups** read batches as a whole source does, through the same loader; the tests cover a
  partitioned source's ranges, not a lookup's batches.

## Benchmark

RS1g's baseline, the numbers RS3c's typed loading and RS6's columnar sources are judged by.
`crates/dbrover-engine/tests/report_benchmark.rs`, an ignored test run by hand in release mode,
runs one report per engine and column kind: one source of generated rows (`id` integer, `amount`
decimal(18,2), `ts` timestamp, `label` text), loaded into the combine step's DuckDB and added up
into one CSV line, so the outputs cost nothing. Each engine makes its rows in the source's own
query (DuckDB's `range()`, PostgreSQL's `generate_series`, ten digits cross-joined in SQLite and
MySQL), so nothing is loaded beforehand. *Inferred* types the columns from their values (RP2's
text path); *declared* gives them in the file (RP3). The phases are each source's own profile.

Measured on 2026-11-09 on an Apple M4 Pro (12 cores, 24 GB), with PostgreSQL 17 and MySQL 8.4
from the repository's Docker Compose file on the same machine, so there is no network in these
numbers: 10,000,000 rows, in seconds.

| Engine | Columns | Rows | Run (s) | Source (s) | Until first row | Receiving | Loading | Typing | Combine (s) | Rows a second | DuckDB peak |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| DuckDB | inferred | 10,000,000 | 12.48 | 12.45 | 0.00 | 3.49 | 7.31 | 0.14 | 0.02 | 803,148 | 977 MB |
| DuckDB | declared | 10,000,000 | 16.32 | 16.30 | 0.00 | 7.41 | 7.29 | 0.14 | 0.01 | 613,534 | 981 MB |
| SQLite | inferred | 10,000,000 | 14.65 | 14.62 | 0.00 | 5.89 | 7.10 | 0.13 | 0.01 | 684,134 | 877 MB |
| SQLite | declared | 10,000,000 | 18.47 | 18.45 | 0.00 | 9.81 | 7.06 | 0.13 | 0.01 | 541,976 | 964 MB |
| PostgreSQL | inferred | 10,000,000 | 11.40 | 11.38 | 0.45 | 1.65 | 7.60 | 0.14 | 0.01 | 878,811 | 892 MB |
| PostgreSQL | declared | 10,000,000 | 15.57 | 15.55 | 0.29 | 5.68 | 7.93 | 0.14 | 0.01 | 643,128 | 981 MB |
| MySQL | inferred | 10,000,000 | 11.60 | 11.58 | 0.08 | 2.75 | 7.09 | 0.13 | 0.01 | 863,334 | 894 MB |
| MySQL | declared | 10,000,000 | 15.53 | 15.51 | 0.08 | 6.75 | 7.05 | 0.13 | 0.01 | 644,828 | 981 MB |

At a million rows each time is a tenth of these, within a few hundredths of a second.

What it says:

- **Loading into DuckDB is the text path's bottleneck**, about 7 of 11 to 15 seconds whatever the
  engine: the appender takes each value as text. That is what RS3c's typed appends and RS6's
  Arrow batches remove, and RS6's exit compares against these rows a second.
- **Typing is cheap.** The second copy RP2's load-then-cast makes (`CREATE TABLE … AS SELECT
  CAST(…)`) takes 0.14 s for ten million rows: the double copy costs space more than time, and
  RS3c's case for typing on arrival is the appender, not the cast.
- **Declared columns are slower today**, by about 4 seconds: each value is checked and rewritten
  in its type's plain form in Rust before it is appended as text again. The profile counts that
  as *receiving rows*, since it happens inside the source's reader, so `slow-first-row` and the
  Profile's bottleneck aren't misled, but *receiving* on a declared source includes DbRover's own
  work. RS3c, which appends declared columns typed, should turn this from a cost into a saving.
- **The engines themselves are fast here**: under half a second to the first row, and 1.6 to 6
  seconds of receiving: PostgreSQL quickest, then MySQL, while SQLite and DuckDB make their rows
  in the worker's own process, so their *receiving* is their query's work too. Over a network, *receiving* grows with the
  bytes: the Profile tells the two apart.
- **DuckDB's memory** peaked at under 1 GB for 10 million rows of about 40 bytes each, inside the
  2 GiB default `memory_limit`, with no spill.

### RS3's typed path

RS3f ran the same benchmark again on 2026-11-10, on the same machine and servers, with RS3c's
typed loading: tables made with the types known before the first row, a page appended as one
Arrow batch, only the other columns rewritten, and the reader a few pages ahead of the loader.
The workspace is now a file in the run's work folder (RS3b). 10,000,000 rows, in seconds:

| Engine | Columns | Rows | Run (s) | Source (s) | Until first row | Receiving | Loading | Typing | Combine (s) | Rows a second | DuckDB peak | Run before |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| DuckDB | inferred | 10,000,000 | 8.31 | 8.28 | 0.01 | 4.16 | 3.66 | 0.42 | 0.01 | 1,208,313 | 536 MB | 12.48 |
| DuckDB | declared | 10,000,000 | 8.67 | 8.66 | 0.00 | 8.39 | 0.23 | 0.00 | 0.01 | 1,155,134 | 92 MB | 16.32 |
| SQLite | inferred | 10,000,000 | 9.12 | 9.10 | 0.00 | 7.21 | 1.28 | 0.58 | 0.01 | 1,098,538 | 704 MB | 14.65 |
| SQLite | declared | 10,000,000 | 10.84 | 10.82 | 0.00 | 10.60 | 0.19 | 0.00 | 0.01 | 924,129 | 227 MB | 18.47 |
| PostgreSQL | inferred | 10,000,000 | 10.42 | 10.40 | 0.70 | 2.01 | 7.05 | 0.61 | 0.01 | 961,723 | 721 MB | 11.40 |
| PostgreSQL | declared | 10,000,000 | 7.70 | 7.68 | 0.34 | 6.60 | 0.71 | 0.00 | 0.01 | 1,302,252 | 89 MB | 15.57 |
| MySQL | inferred | 10,000,000 | 9.64 | 9.62 | 0.28 | 3.38 | 5.34 | 0.57 | 0.01 | 1,039,609 | 722 MB | 11.60 |
| MySQL | declared | 10,000,000 | 8.55 | 8.53 | 0.09 | 7.89 | 0.52 | 0.00 | 0.01 | 1,172,470 | 262 MB | 15.53 |

*Loading* is now what DuckDB held the source (RS3c): the reader waiting for room in the queue,
and for the last pages to be appended, not the appending itself, which overlaps *receiving*. So
the two columns can't be added up against RS1g's: the run's time is what to compare.

What it says:

- **Every run is faster,** by 9% (PostgreSQL inferred) to 51% (PostgreSQL declared): the
  appender no longer holds the reader up, and DuckDB casts a typed page faster than it stores
  its text.
- **Declared columns turned from a cost into a saving,** as RS1g hoped: 4 seconds slower than
  inferred before, they are now as fast or faster, since every column is typed before its first
  row and nothing is rewritten after the last (*typing* 0.00). DuckDB's peak falls from about
  980 MB to 90 to 260 MB, with no second copy of the table.
- **Inferred columns** still load the columns their values type as text, and rewrite them at
  the end (0.4 to 0.6 s for two of four columns, `id` and `ts`, against 0.14 s for RS1g's
  in-memory copy of all four, on a file now). On PostgreSQL the source sends faster than DuckDB
  takes text, so *loading* dominates: declaring its columns halves the run.
- **The engines' own time** is unchanged; *receiving* now holds the declared columns' check in
  Rust (RS1g), which the queue overlaps with appending.

RS6's columnar path is judged against these rows a second.

### RS6's columnar path

RS6f ran the benchmark again on 2026-11-13, on the same machine, with ClickHouse 25.8 from the
Compose file added (its rows from `numbers()`, `id` an `Int64`, `amount` a `Decimal(18, 2)`,
`ts` a `DateTime`), and a third column kind: *columnar*, a source read as Arrow batches (RS6),
which is now the default for DuckDB and ClickHouse; *inferred* is the text path, run with
`columnar = false`. Other sessions were building on the machine: a first run had DuckDB's
inferred row at 11.26 s, and DuckDB alone again gave 8.04 s, so the rows below, from a second
full run, carry a few tenths of a second of noise. 10,000,000 rows, in seconds:

| Engine | Columns | Rows | Run (s) | Source (s) | Until first row | Receiving | Loading | Typing | Combine (s) | Rows a second | DuckDB peak | RS3f's run |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| DuckDB | inferred | 10,000,000 | 8.97 | 8.95 | 0.00 | 4.09 | 4.39 | 0.42 | 0.01 | 1,117,193 | 536 MB | 8.31 |
| DuckDB | declared | 10,000,000 | 8.90 | 8.88 | 0.00 | 8.59 | 0.26 | 0.00 | 0.01 | 1,125,872 | 92 MB | 8.67 |
| DuckDB | columnar | 10,000,000 | 2.23 | 2.21 | 0.00 | 0.50 | 1.71 | 0.00 | 0.01 | 4,518,752 | 85 MB | — |
| SQLite | inferred | 10,000,000 | 9.85 | 9.82 | 0.00 | 7.38 | 1.80 | 0.61 | 0.01 | 1,017,915 | 705 MB | 9.12 |
| SQLite | declared | 10,000,000 | 11.11 | 11.09 | 0.00 | 10.82 | 0.23 | 0.00 | 0.01 | 902,038 | 227 MB | 10.84 |
| PostgreSQL | inferred | 10,000,000 | 10.60 | 10.58 | 0.41 | 2.09 | 7.43 | 0.62 | 0.01 | 945,447 | 725 MB | 10.42 |
| PostgreSQL | declared | 10,000,000 | 7.74 | 7.73 | 0.35 | 6.64 | 0.70 | 0.00 | 0.01 | 1,294,330 | 88 MB | 7.70 |
| MySQL | inferred | 10,000,000 | 9.77 | 9.74 | 0.09 | 3.20 | 5.84 | 0.57 | 0.01 | 1,026,167 | 726 MB | 9.64 |
| MySQL | declared | 10,000,000 | 8.52 | 8.50 | 0.09 | 7.61 | 0.78 | 0.00 | 0.01 | 1,176,055 | 257 MB | 8.55 |
| ClickHouse | inferred | 10,000,000 | 8.32 | 8.30 | 0.02 | 1.89 | 5.94 | 0.43 | 0.01 | 1,204,238 | 540 MB | — |
| ClickHouse | declared | 10,000,000 | 7.24 | 7.22 | 0.02 | 6.53 | 0.64 | 0.00 | 0.01 | 1,384,274 | 89 MB | — |
| ClickHouse | columnar | 10,000,000 | 4.87 | 4.85 | 0.04 | 0.17 | 4.36 | 0.28 | 0.01 | 2,060,581 | 308 MB | — |

What it says:

- **The exit is met:** on ClickHouse the columnar path reads 2,060,581 rows a second against
  the text path's 1,204,238, 1.7 times as many, and runs in 4.87 s against 8.32 s (and 7.24 s
  with declared columns). The first full run gave 4.11 s against 8.93 s.
- **DuckDB gains most,** four times the rows a second: its chunks reach the run's DuckDB as
  they left the source's, with nothing formatted or parsed, and *receiving* falls from 4.1 s
  to 0.5 s.
- **ClickHouse's `DateTime` still travels as text** (its Arrow output has no timestamp for it),
  so `ts` is loaded as text and rewritten at the end (*typing* 0.28 s), and DuckDB's peak is
  that column's: 308 MB against 85 to 92 MB for an all-typed table. *Loading* is DuckDB
  storing those strings. A `DateTime64` or `Date32` in the query, or a cast in `[combine]`,
  would be the next step, if a report needs it.
- **The text path is unchanged:** every engine's inferred and declared rows are within noise of
  RS3f's.

### RS3's exit test

`crates/dbrover-app/tests/report_at_scale.rs`, an ignored test run by hand, sends a report to
the real worker as the app does: two sources of 50,000,000 generated rows each, on an in-memory
DuckDB data source, joined on their ids and sorted, under a `memory_limit` of 1 GiB, written as
Parquet by `COPY`, with `max_disk = "40 GiB"`. Measured on 2026-11-10, in a debug build of
DbRover (DuckDB is the prebuilt release library either way):

| What | Measured | Bound |
| --- | ---: | ---: |
| Rows loaded | 100,000,000 (two sources of 50,000,000) | — |
| Loading both sources, at once | 660 s | — |
| The combine step: join, sort and `COPY` to Parquet | 4.7 s, 50,000,000 rows | — |
| The Parquet file | 192 MB | `max_size` 20 GiB |
| DuckDB spilled | 4.1 GiB | `max_temp_size` 30 GiB |
| The run's folders at most (work folder and staging, sampled twice a second) | 5.3 GiB | `max_disk` 40 GiB |
| DuckDB's own peak | 2.0 GiB | `memory_limit` 1 GiB |
| The worker's peak resident memory | 1.73 GiB | 2.07 GiB: 2 × `memory_limit`, the queues' 2 MB, the worker's own 70 MB |

The load is the text path of a debug build, at about 75,000 rows a second for each source;
the benchmark above is the release build's. The combine step went past its `memory_limit`, as
it was meant to, and spilled; DuckDB's own peak passed the limit while the sources loaded, as
the as-built notes under [RS3 against today's code](#rs3-against-todays-code) say, which is why
the bound counts DuckDB at twice it. The run's work folder was gone after it.

### RS4's exit test

`crates/dbrover-app/tests/report_at_scale.rs` gains a second ignored test, run by hand: a year of
events, one a second (31,536,000 in 2025), generated in an in-memory DuckDB data source's own
query from each range's bounds, run by month through the real worker into a Parquet file a
month, the combine step adding them up a customer a day under a `memory_limit` of 4 GiB. The
same report over January alone, the largest month, is its yardstick. Measured on 2026-11-11, in a
debug build of DbRover (DuckDB is the prebuilt release library either way):

| What | Measured | Against |
| --- | ---: | ---: |
| Events read | 31,536,000, in twelve statements | — |
| The run, every month | 345 s | — |
| Parquet files | twelve, `events-2025-01.parquet` to `events-2025-12.parquet`, 150,000 or 155,000 rows each | — |
| Each month's DuckDB peak | 236 to 254 MiB | January alone: 250 MiB |
| The worker's peak resident memory | 256 MiB | January alone: 255 MiB |
| The run's folders at most | 54 MB | `max_disk` 20 GiB |

Each month's DuckDB held a month's rows, as the year's would have twelve; nothing spilled under
the 4 GiB `memory_limit`. The worker's peak varies more between runs than DuckDB's: an earlier
run of the same test measured 293 MiB over the year against 232 MiB for January, since memory a
range's DuckDB frees isn't always handed back to the system before the next range. So the test
holds DuckDB's peaks within a quarter of January's, and the worker's within a half.

### RS5's exit test

`crates/dbrover-app/tests/report_at_scale.rs` gains a third ignored test, run by hand: RS4's
year of events, an event a second, by month into a Parquet file a month and one CSV file of
every month, under `resumable_above = "1 KiB"`. It runs once uninterrupted, then again with the
worker killed once the seventh month has started, then resumes that run from its kept work
folder. Measured on 2026-11-12, in a debug build:

| What | Measured |
| --- | ---: |
| The uninterrupted run | 309 s |
| The run killed in July | killed after 142 s, six months done, 745 MB read, its work folder kept |
| The run resuming it | 146 s, reading July to December only: 15,897,600 events, months 7 to 12 |
| Files | thirteen, each byte for byte the uninterrupted run's: twelve Parquet files and the CSV file of every month |

The first run of the test found a range's DuckDB, left by the killed worker, refusing the
resumed range ("already exists"); RS5d removes it before the range starts again. The test's
kill waits for a progress event of July, which a small year (`DBROVER_RS4_EVERY_SECONDS`)
finishes too fast to give, so it is run at its default size.

## Recommended path

1. **RS1 and RS2 first.** They help every report that exists today, need no new dependency,
   and turn every later decision into a measurement: whether the text path is the bottleneck,
   for which engines, and by how much. The benchmark in RS1g is the baseline.
2. **RS3 next:** a workspace on disk, typed loading and Parquet outputs remove today's hardest
   ceilings (memory, the double copy, rows through Rust) with DuckDB features already built.
3. **RS6 before C5:** the batch capability is cheaper designed into the warehouse drivers than
   added to them later.
4. **RS4 and RS5** once runs are long enough to need them, which RS1's history will show.
5. **RS7** after C2 is merged for the lake, and with each of C3 to C5 for handoff.
6. **RS8** whenever an MCP client should help tune reports.
7. **RS9 (composing reports) whenever it is wanted.** It is small, doesn't depend on the scale
   work, and could come right after RP5, ahead of RS1. Only `reuse` waits for RS5.

Left out: DataFusion, Polars, Arrow Flight and ADBC; DuckDB's scanners for now; a cluster or any
distributed execution; writing to a source to stage results; moving data between warehouses;
hash partitions; carried state across partitions; suspending a worker as a pause; and
everything [Reports](reporting.md#scope) already keeps out (dashboards, schedules, sharing).

## Decisions

Proposed defaults; each is confirmed when its milestone starts.

| Question | Proposed | Consequence |
| --- | --- | --- |
| What comes first | **The tooling (RS1, RS2),** then the execution milestones. | Execution changes are chosen by measurement; the first milestones help today's reports too. |
| Where the heavy work runs | **In each source's engine;** the desktop combines reduced results. | A terabyte never crosses the network; DbRover's work is reading reduced results fast and showing where reduction is missing. |
| Pushdown | **Hints from DuckDB's plan first; opt-in wrapping of a SQL source's query later.** | Nothing is rewritten without the author asking; a wrapped query is shown and kept in the profile. |
| DuckDB's postgres and mysql scanners | **Not now,** as RP2 decided. | DbRover's guards stay in one place; reopened if RS1 shows PostgreSQL loading is the bottleneck and `COPY` doesn't fix it. |
| Values on the bulk path | **Arrow where the engine gives it natively,** text otherwise; screens stay text (ADR 0004, amended by ADR 0007). | Exact decimals through `Decimal128`; no new dependency for the arrays. |
| Typing loaded sources | **Typed on arrival when the type is known,** RP2's load-then-cast for the columns whose type comes from their values. | One copy for most sources; inferred columns behave as before. |
| The run's DuckDB | **On a file in a work folder, always;** the folder is kept after a failure only past `resumable_above`. | Loaded tables evict to disk like any database's pages; a small run that fails costs nothing to keep for, and starts again. |
| Where work folders live | **The data folder's `report-runs/`,** with a setting for another disk; output staging stays in the output folder. | Resumable work survives an output folder being cleaned, and the final rename stays on one disk. The spill moves off the output folder's disk, which RP2 put it on. |
| Partitions | **Explicit ranges of time or integer keys** in the file, `:part_from` and `:part_to` bound; one at a time by default. | Nothing is guessed from data; a range is the unit of progress, retry and resume. |
| Partitioned runs | **The author asserts the combine is local to a range;** the manual says what breaks it. | Bounded memory per range; a running balance needs an opening-balance source. |
| Pause | **A cancel that keeps the work folder,** not a suspended process. | No connections or warehouse jobs held open, and nothing billed while paused. |
| Consistency on resume | **`as_of` where the engine time-travels; closed ranges elsewhere.** | A resumed run equals an uninterrupted one when the data under it didn't change, and the manual says when it can. |
| The run's time | **A wall-clock `timeout` and a `stall_timeout`.** | A long run isn't mistaken for a stuck one. |
| Disk | **A `max_disk` for the work folder,** checked against the free space DuckDB reports. | No new dependency; a run fails before it fills a disk. |
| Results handed over through a bucket | **Opt-in per source,** to a prefix the user names, deleted when the run ends. | Fast large results from warehouses, with the only write a report makes being one the user asked for, outside every data source. |
| The profile | **One JSON document per run,** versioned, in `report_runs`' `steps`. | No schema migration; the app, the command line and MCP read the same thing. |
| A flame graph | **W10's plan view for DuckDB's operator tree,** a timeline for the steps. | One plan view to maintain; the timeline covers what a flame graph of steps would. |
| New engines | **None.** DataFusion, Polars, Flight and ADBC are out. | The binary and CI's build time stay where they are; one combine dialect. |
| Composing reports | **A report as a source:** its result by name, with its parameters bound, run in the same worker and DuckDB. | Reports reuse each other through a contract, their result, not through copied text. One set of limits bounds the whole run. |
| What a report source exposes | **Only the inner report's result,** not its sources. | An inner report can change how it gets its rows without breaking the reports that use it. |
| Reusing results | **`reuse = "<age>"`, opt-in,** keyed by the inner file's hash and its parameters. | A heavy step runs once for many reports, and freshness is stated in the file rather than assumed. |

## Risks

- **The text path may be fast enough,** or the network may dominate whatever DbRover does.
  RS1's benchmark decides whether RS6 is worth its ADR; the plan is ordered so that nothing
  after RS3 starts without it.
- **DuckDB's settings move.** `allowed_directories`, the profiler's operator tree through
  `duckdb-rs`, `duckdb_memory()` and the default of `max_temp_directory_size` are DuckDB's,
  read by name; each is checked in its milestone's first layer against the DuckDB in the build
  (1.5), and the workspace's tests pin them.
- **A partitioned run that isn't local to its ranges** writes wrong numbers silently. The
  manual's examples, and a check that a partitioned report's outputs over two ranges match the
  unpartitioned run's on generated data, are the guard.
- **Data at rest.** Work folders and the incremental cache keep source rows on disk for days.
  They live in the user's data folder with its permissions, expire, count against a total, and
  are removed by **Discard**, **Run from scratch** and **Clear history**; the manual says so.
  Encrypting them is left out.
- **Bills.** Partitions, batches and resumes multiply statements; BigQuery bills each at least
  10 MB a table. The cost guard judges a source across all of them, and the profile shows bytes
  scanned per range.
- **Handoff buckets.** A run killed before it deletes its handoff files leaves them; the next
  run of the report, or **Discard**, deletes them, and the manual suggests a lifecycle rule on
  the prefix.
- **Laptops.** Sleep, Wi-Fi changes and battery: keeping awake covers sleep on macOS and Linux;
  a dropped connection fails its unit, which resume reruns.
- **The history file grows.** A profile with DuckDB's operator tree and each range's numbers is
  tens of kilobytes to a megabyte, against RP4's few kilobytes a run. `runs_kept` bounds it;
  the plan and the per-range numbers are the first to be capped in size if it matters.
- **A shared report edited for one use.** Changing a report changes every report built on it.
  **Used by** in its tab and the file hashes in each run's profile show who is affected and
  since when.
- **CI time.** Large-data tests are slow tests run by hand; CI runs small cases only, and RS7's
  lake test reuses C2's MinIO.

## What stays out

- A server, a scheduler, a cluster, or sharing results: [Vision](vision.md)'s non-goals.
- Writing to a data source, staging tables in a warehouse, or moving data between engines.
- A second query engine beside DuckDB, and native driver plugins.
- Excel as an output for very large results, beyond its continued sheets.
- Automatic query rewriting beyond the opt-in filters and columns of option B.

## Open questions

- Should an MCP client run reports (RS8)? The read-only tools come first either way.
- Is the BigQuery Storage Read API worth gRPC in the build, or does the bucket handoff cover
  BigQuery's large results? C4 left it out (it reads text pages); decided with RS6 and RS7.
- Should the run profile be exportable to a standard trace format (Chrome's trace events, which
  `chrome://tracing` and Perfetto open), for people who already read traces? Cheap to add to
  `--profile` once the profile exists.
