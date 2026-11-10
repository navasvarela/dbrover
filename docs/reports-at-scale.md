# Reports at scale

Status: **RS1 built** (2026-11-09); **RS2 started** (2026-11-10); **the rest planned, not
scheduled.** Proposed on
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
  appended or one file each, `max_rows_per_part`, range skew in the profile and its hint.
  **Exit:** a year of generated events, read by month, writes a Parquet file per month with the
  combine's memory that of one month.
- **RS5 — Resume:** work folders kept for large runs, the manifest, **Resume**, **Pause** and
  **Discard**, `--resume`; job ids and page tokens for C0's job engines; `as_of`; then
  incremental sources and their cache.
  **Exit:** a run whose worker is killed in its seventh range resumes, reads ranges 7 to 12
  only, and writes files equal to an uninterrupted run's.
- **RS6 — Columnar sources:** ADR 0007, `ResultCursor::next_batch`, Arrow batches appended to
  the workspace, DuckDB sources with a query and ClickHouse's `ArrowStream` first. To land
  before C5 and C6, so their drivers implement it from the start. C3 started before it
  (2026-11-05) with text rows, which `next_batch`'s default keeps.
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
| Bytes scanned: ClickHouse's `X-ClickHouse-Summary`; each warehouse's own as it comes. | ClickHouse sends that header before the rows: it is complete only for a result that fits its output buffer, unless `wait_end_of_query` holds the whole result on the server. C3's and C4's cursors keep each job's final statistics. | RS2f adds `ResultCursor::scanned` (none by default). Job cursors give their job's bytes (BigQuery's `totalBytesProcessed`, Athena's `DataScannedInBytes`) and Trino its last `processedBytes`. ClickHouse gives the header's `read_bytes`, marked *at least* when it read fewer rows than its `total_rows_to_read`. So RS2f covers the three warehouses that came before RS2, not only ClickHouse. |
| Plan-based hints, among the rules in the app. | RS1e's rules are pure functions of a kept run, in the app's `reports/hints.rs`. | RS2e's three rules join them, reading the combine's plan through a pure summary in `dbrover-plan` (each scan's table, columns and filters, and whether an aggregate reads one scan with no join under it). **Explain report** lists the same hints for the explained combine. |

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
