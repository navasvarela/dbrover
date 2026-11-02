# Reports

DbRover answers questions one query at a time: a console runs a statement on one data source,
the grid shows the rows, and W7's exports write them to a file. Some questions need more than
that. A month-end valuation joins transactions in PostgreSQL with asset metadata in DynamoDB and
prices in DuckDB, shapes the result into the columns an accountant expects, and is run again
next month with new dates. Today that means three exports and a spreadsheet.

The RP track adds **reports**: a saved, re-runnable recipe of

1. one or more queries, each on its own data source;
2. a step that combines and shapes their results;
3. one or more output files with a declared schema;
4. a record of each run: how long each step took, how much it read, and how large the files were.

Planned on 2026-11-01. This moves DbRover in a new direction, so [Vision](vision.md) now says
what is in scope and what isn't (below).

## Scope

In:

- Report files that live with the other settings, run from the app, with parameters.
- Several data sources in one report, of any engine DbRover speaks.
- Logic in SQL (and PRQL, see [Logic](#logic)), output schemas for CSV and Excel first.
- Metrics per run, and guardrails so a report can't crash the app or take over the machine.
- Later, a command-line runner, so cron or a CI job can run a report with no server.

Out, and staying out:

- Dashboards, charts that refresh, and a BI semantic layer. W12's charts stay one result's
  picture in a console.
- A scheduling service, sharing, users or permissions. DbRover stays a single-user desktop app.
- Pixel-perfect PDF layouts.
- Writing to a data source. Reports read; every source connects read-only (W11).

## A report file

A report is a TOML file in the `reports/` folder of the settings folder (one folder for every
project, see [RP1's decisions](#rp1s-decisions)), read the same way as
`actions.toml` and `exports.toml`: problems are listed in the report's tab and marked in the
explorer, and a report with a problem can't run. TOML gives the structure; the logic is in
the query languages people already know.

The example below is the use case the design is checked against: crypto transactions in
PostgreSQL, the assets' metadata in DynamoDB, hourly prices in DuckDB, and an Excel file of
each transaction valued at the time it ran.

```toml
[report]
name = "Transactions valued at execution time"
params = { since = "timestamp", until = "timestamp" }

# PostgreSQL: the transactions in the period, filtered at the source.
[[source]]
id = "tx"
data_source = "ledger-postgres"
query = """
SELECT id, wallet_id, asset_id, amount, executed_at
FROM transactions
WHERE executed_at >= :since AND executed_at < :until
"""

# DynamoDB: metadata for the assets those transactions use, read by key.
[[source]]
id = "assets"
data_source = "assets-dynamodb"
lookup = { from = "tx", column = "asset_id", as = "asset_ids" }
query = "SELECT asset_id, symbol, unit, chain FROM assets WHERE asset_id IN :asset_ids"
columns = [
  { name = "asset_id", type = "text" },
  { name = "symbol",   type = "text" },
  # The size of the asset's smallest unit: 0.000000000000000001 for 18 decimals.
  { name = "unit",     type = "decimal(38,18)" },
  { name = "chain",    type = "text", default = "unknown" },
]

# DuckDB: the price series, attached read-only and joined in place.
[[source]]
id = "prices"
data_source = "prices-duckdb"
attach = true

[combine]
query = """
SELECT tx.executed_at, tx.wallet_id, assets.symbol, assets.chain,
       tx.amount * assets.unit               AS quantity,
       p.price_usd,
       tx.amount * assets.unit * p.price_usd AS value_usd
FROM tx
JOIN assets USING (asset_id)
ASOF JOIN prices.hourly_prices p
  ON p.asset_id = tx.asset_id AND p.hour <= tx.executed_at
ORDER BY tx.executed_at
"""
memory_limit = "2 GiB"

[[output]]
format = "xlsx"
file = "valuations-{since:%Y-%m}.xlsx"
sheet_name = "Valuations"
columns = [
  { name = "Executed (UTC)", from = "executed_at", type = "timestamp", format = "yyyy-mm-dd hh:mm" },
  { name = "Wallet",         from = "wallet_id" },
  { name = "Asset",          from = "symbol" },
  { name = "Chain",          from = "chain" },
  { name = "Quantity",       from = "quantity",  type = "decimal(38,18)" },
  { name = "Price (USD)",    from = "price_usd", type = "decimal(38,8)", format = "#,##0.00######" },
  { name = "Value (USD)",    from = "value_usd", type = "decimal(38,2)", format = "#,##0.00" },
]

[[output]]
format = "csv"
file = "valuations-{since:%Y-%m}.csv"
columns = "same"
```

### Parameters

`params` names each parameter and its type. Queries use them as W6's `:name` parameters, so a
run asks for them in the same **Parameter values** dialog and remembers the last values.
Output file names can format them (`{since:%Y-%m}`).

### Sources

Each `[[source]]` runs one query on one data source, through DbRover's own drivers. That keeps
every guard the data source already has: its SSH tunnel, its password command, read-only
(W11) and the cost guard (N5). Its rows stream into a DuckDB table named after its `id`.

- **Lookups.** `lookup` binds a parameter to the distinct values of an earlier source's column,
  so a later source reads by key instead of scanning: `IN :ids` on DynamoDB becomes batched
  reads, `= ANY(:ids)` on PostgreSQL an array parameter. Sources run in the order their lookups
  need; the others run at the same time. A lookup with more values than the engine takes in one
  statement is split into batches.
- **Declared columns.** DynamoDB, MongoDB, Elasticsearch and Redis return documents, so their
  sources declare the columns they produce: a name, a type and an optional default, with
  nested fields by path (`metadata.chain`), as N0b's flattening already reads them. Fields not
  declared are dropped. A value that doesn't fit its column fails the run and names the item.
  SQL sources may declare columns too, to check types early.
- **Attach.** A DuckDB data source with `attach = true` is attached read-only to the combine
  step instead of copied, so its filters and joins run where its data is. Every other source
  is copied: its own query should filter what it reads, and the run's metrics show when it
  doesn't.

### Logic

The combine step is DuckDB SQL over the sources' tables. DuckDB is already bundled, and gives
reports a full analytical SQL without a new language: window functions, `ASOF JOIN` for "the
price at the time of the transaction", `PIVOT`, `GROUPING SETS` for subtotals, and exact
decimals.

[PRQL](https://prql-lang.org) is the second language: `[combine] prql = "…"` is compiled to
DuckDB SQL by `prqlc` before the run, and the compiled SQL is shown beside it, so a pipeline
reads top to bottom (`from tx | join assets | derive value = …`) while running as plain SQL.
Source queries stay in their engine's own language.

Not chosen: a language of DbRover's own, which would have to reimplement what SQL already does;
and an embedded scripting language such as Rhai or Lua, which is hard to bound in memory and
time and hard to explain in metrics. Logic that SQL can't express goes to a program step, the
same kind W8 adds for exports: rows as JSON Lines on stdin, rows back on stdout, trusted the way
H2's commands are.

A report with one source and no `[combine]` writes that source's rows.

### Outputs

`[[output]]` is a W8 preset with an explicit column schema, so reports and exports share one
layer of writers:

- **Columns:** the header, the column it comes `from`, its type and its format. A column whose
  values don't fit its type fails the run, unless it says how to convert.
- **CSV:** separator, quoting, header row and line ending, as W7's options, by the names
  `exports.toml` gives them.
- **Excel:** the sheet, header style, column widths and frozen panes; several `[[output]]`s
  with one file write several sheets. Excel's limit of 1,048,576 rows a sheet fails the run
  with a message, unless the output says to continue on a new sheet.
- **Text templates:** W8's MiniJinja templates, for formats nobody else needs.
- `columns = "same"` reuses the first output's columns.

### Exact numbers

Accounting and crypto amounts are decimals, and 18-decimal tokens lose their last digits in a
float. Decimals stay decimals from source to file: PostgreSQL's `numeric`, MySQL's `DECIMAL`
and DynamoDB's numbers (strings on the wire) load into DuckDB `DECIMAL`s, and the writers write
them without passing through `f64`. An output's declared type rounds once, at the end.
Excel stores numbers as doubles, so a column that needs more than 15 significant digits is
written as text with a warning in the run's log, unless its type says to round.

## Running a report

Reports are listed in the explorer under **Reports**. Opening one shows
its file and a **Run** button; a run asks for the parameters, then shows each step's progress,
a preview of the combined rows, and links to the files it wrote.

### The worker

A report runs in a separate process, `dbrover report-worker`, started from the same binary the
way `dbrover mcp` is. The app sends it the report and the parameters, and it streams back
progress, metrics and the preview as JSON Lines. If the worker runs out of memory or is
killed, the app says the run failed and why; the app, its consoles and their transactions are
untouched. **Cancel** stops the worker.

The worker reads the same settings folder, keychain and trusted commands as the app, so a data
source connects the same way in both.

### Guardrails

In layers, cheapest first:

1. **Sources:** a row cap and a byte cap on each source (defaults in Settings, overridable per
   source), a statement timeout, read-only connections, and the cost guard.
2. **Combine:** DuckDB's `memory_limit` (a quarter of the machine's memory by default),
   `threads` (half the cores), and spilling to a temporary folder capped by
   `max_temp_directory_size`.
3. **Outputs:** the writers stream, Excel included (`rust_xlsxwriter`'s constant-memory mode);
   a cap on each file's size; W8's fuel limit on templates.
4. **The process:** a lower priority, and limits from the operating system: `setrlimit` on
   Linux, a Job Object with memory and CPU caps on Windows. macOS doesn't enforce an address
   space limit reliably, so there DuckDB's memory limit and the source caps carry it.
5. **One run at a time,** by default.

A run stopped by a guardrail says which limit it hit and where to raise it.

## Metrics

Each run records, per step:

| Step | Recorded |
| --- | --- |
| Each source | time to the first row, total time, rows, bytes read, batches for a lookup |
| Combine | time, DuckDB's peak memory, bytes spilled to disk |
| Each output | time, rows, file size |
| The run | total time, the parameters, how it ended |

The last runs of each report are kept in the history database (W4), so the report's tab can
show them side by side and point at the step that dominates. Two hints come from them: a
source that reads far more rows than the report writes should filter in its own query, and a
combine step that spills should get a larger memory limit or fewer rows.

## Testing with real reports

The first iteration (RP1–RP3) is done when real reports from accounting software run on it.
Their shapes, rebuilt on synthetic data in the repository's test containers, never on customer
data:

- **Trial balance:** debits, credits and balances per account for a period, with subtotals per
  account type (`GROUPING SETS`), to Excel.
- **General ledger:** every entry for a period with its account, counterparty and running
  balance per account (window functions), to CSV, past a million rows.
- **Valuation:** the example above, across PostgreSQL, DynamoDB and DuckDB.
- **Cost basis and realised gains:** FIFO lots matched against disposals per asset. This one
  is the stress test of the logic layer: lot matching is running state, which SQL expresses
  with recursive queries or window tricks, and is the case most likely to need a program step.

Each becomes a report file under `examples/reports/`, with an integration test that runs it in
the worker against the CI containers and compares the output with a checked-in file. What these
reports can't express, or express badly, decides what RP4 onwards adds.

## How it's built

- `dbrover-report`, a new crate: the report file's types and reading, the plan (which sources
  run when, with which lookups), loading sources into DuckDB, the combine step, and the metrics.
  Pure where it can be, and tested with DuckDB in memory.
- `dbrover-export` (W7, W8): column schemas with types and conversions, decimal output, and
  Excel's constant-memory mode.
- `dbrover-engine`: running a source's query to completion with caps, batched lookups per
  driver.
- `dbrover-app`: the `report-worker` subcommand, the Reports node and tab, the run panel and
  its history.

## Layers

After W8, which this builds on. One GitHub stack per milestone:

- **RP0 — The plan:** this document, the roadmap entry and the line in Vision.
- **RP1 — One source, one file:** the report file, parameters, the worker with its guardrails,
  output schemas, and the Reports tab. A report with one source writes CSV and Excel. In
  layers RP1a to RP1g, [below](#rp1--one-source-one-file).
- **RP2 — Several sources:** sources loaded into DuckDB, the combine step, attached DuckDB
  sources, exact decimals end to end. In layers RP2a to RP2f,
  [below](#rp2--several-sources).
- **RP3 — Lookups and documents:** lookups with batching, declared columns for document
  sources. The first iteration's exit: the four accounting reports above run and match.
- **RP4 — Metrics:** the run history, the comparison of runs, and the hints.
- **RP5 — PRQL:** `prql` in `[combine]`, with the compiled SQL shown.
- **RP6 — The command line:** `dbrover report run <file> --param since=…`, for cron and CI,
  with the worker's guardrails and an exit code per outcome.
- **RP7 — Manual,** with the examples.

## RP1 — One source, one file

Started on 2026-11-02, after C0. RP1 is the whole path a report takes, from the file to the files
it writes, with the simplest logic there is: one source's rows, shaped by each output's schema.
RP2 adds DuckDB between the two without changing either end.

### The report file in RP1

A trial balance on a ledger kept in one database:

```toml
[report]
name = "Trial balance"
description = "Debits, credits and the balance of each account in a period."
params = { since = "date", until = "date" }

[[source]]
id = "balances"
data_source = "ledger"
query = """
SELECT a.code AS account, a.name AS account_name,
       SUM(e.debit) AS debit, SUM(e.credit) AS credit,
       SUM(e.debit) - SUM(e.credit) AS balance
FROM entries e JOIN accounts a ON a.id = e.account_id
WHERE e.booked_on >= :since AND e.booked_on < :until
GROUP BY a.code, a.name
ORDER BY a.code
"""
max_rows = 100000

[[output]]
format = "xlsx"
file = "trial-balance-{since:%Y-%m}.xlsx"
sheet_name = "Trial balance"
columns = [
  { name = "Account", from = "account" },
  { name = "Name",    from = "account_name" },
  { name = "Debit",   from = "debit",   type = "decimal(18,2)", format = "#,##0.00" },
  { name = "Credit",  from = "credit",  type = "decimal(18,2)", format = "#,##0.00" },
  { name = "Balance", from = "balance", type = "decimal(18,2)", format = "#,##0.00;[Red]-#,##0.00" },
]

[[output]]
preset = "semicolon-csv"
file = "trial-balance-{since:%Y-%m}.csv"
columns = "same"
```

What RP1 reads:

- **`[report]`:** `name`, an optional `description`, `params`, and an optional `timeout` for the
  whole run.
- **One `[[source]]`:** its `id`, its `data_source` (by name, or by id when two share a name, as
  the MCP server takes them), its `query` with `:name` parameters, and its limits `max_rows`,
  `max_bytes` and `timeout`. The data source is one with a SQL dialect: PostgreSQL, MySQL,
  SQLite, SQL Server, DuckDB, ClickHouse or Cassandra. Documents, which need declared columns,
  wait for RP3.
- **`[[output]]`s:** either `format = "csv"` or `"xlsx"` with that format's options by the
  names `exports.toml` gives them (`separator`, `sheet_name`, …), or `preset = "<id>"` naming
  a CSV or Excel preset in `exports.toml`, whose options the output can't change; then `file`,
  `columns` and the output's limits. Templates and programs as outputs wait until a report
  needs one.
- **What later milestones read** (`[combine]`, a second `[[source]]`, `lookup`, `attach`, a
  source's `columns`, two outputs writing one file) is a problem that names the milestone, so a
  file written for RP2 says why it can't run yet instead of running something else.

### Parameters and file names

`params` maps each name to its type: `text`, `integer`, `decimal`, `boolean`, `date` or
`timestamp`. Values are typed in the report's tab and checked against their type as they are
typed: a date as `2026-10-01`, a timestamp as `2026-10-01 12:00` or `2026-10-01T12:00:00.5`.
A run sends them as W6 sends a console's `:name` values, bound where the driver binds and as
literals where it doesn't, in the checked form, so every engine sees the same date. A `:name`
that `params` doesn't declare is a problem in the file.

`file` is a plain file name in the run's output folder. `{name}` puts a parameter's value in it,
and `{name:%Y-%m}` formats a date or timestamp parameter with strftime's codes. A name that
would come out with a folder separator, or empty, fails the run before anything is read.

### Output schemas

Each column of `columns` is `{ name, from, type, format, if_invalid }`; only `name` and `from`
are needed. Without `columns`, an output writes every column of the source as it comes, typed as
W7 types an export. `columns = "same"` takes the first output's.

| `type` | Takes | CSV writes | Excel writes |
| --- | --- | --- | --- |
| `text` | anything | the value | text |
| `integer` | a whole number | its digits | a number; text past 15 digits |
| `decimal` | a decimal number, exponent allowed | its digits, as given | a number; text past 15 significant digits |
| `decimal(p,s)` | a decimal number, rounded to `s` places half away from zero, at most `p` digits | exactly `s` places | as `decimal` |
| `boolean` | `t`/`f`, `true`/`false`, `yes`/`no`, `1`/`0` | `true` or `false` | a boolean |
| `date` | `2026-10-01` | `2026-10-01` | a date |
| `timestamp` | a date and time with no offset; with `time_zone = "UTC"` (or any IANA zone), one with an offset, converted | `2026-10-01 12:00:00`, with its fraction | a date and time |

- **Exact:** decimals are rounded on their digits, as text, never through `f64`.
- **`format`** is Excel's number format for the column (`"#,##0.00"`, `"yyyy-mm-dd hh:mm"`). CSV
  ignores it: a CSV file is read by programs, which want each type's plain text.
- **A value that doesn't fit** fails the run with the output, the column, the row number and
  the value. `if_invalid = "null"` writes an empty cell instead, and `"text"` writes the value
  as it came.
- **More than Excel's 15 digits:** the cell is written as text, and the run's result warns
  once per column with how many cells it happened to.
- **Excel's row limit** of 1,048,576 rows a sheet fails the run, unless the output says
  `continue_on_new_sheet = true`.

### The worker in RP1

The app starts `dbrover report-worker --config-dir <its settings folder>` from its own
executable, writes one JSON request to its standard input (the report file's path, the
parameter values, the output folder, and a staging folder of the run's own inside it) and reads
JSON Lines from its standard output until it exits: the run starting, the source's progress (rows and bytes, once a second), a preview of the
first 200 rows, each file written (path, rows, size, time, warnings), and how the run ended. A
failure says which step failed, and for a limit, which one, its value, and the key that raises
it. Standard error goes to the app's log.

Files are written in the staging folder and renamed into the output folder once every one is
complete. The run removes the staging folder however it ends; a worker killed by **Cancel**,
by its time limit or by the system can't, so the app's side removes it before it reports how
the run ended. A run that fails or is stopped leaves the output folder as it was.

The worker reads `data_sources.toml`, `exports.toml`, the trusted commands and the keychain as
the app does, and asks nobody anything: a data source whose password is neither saved, nor from
a password command, nor from a cloud sign-in fails the run and says so. The worker that runs is
the binary that started the app, so the request needs no version of its own.

### Guardrails in RP1

By layer, as in [Guardrails](#guardrails):

1. **Source:** the statement is checked to only read, and runs in the engine's read-only mode
   (W11's, as a read-only data source's consoles use it); the cost guard refuses what it would
   ask about, since nobody can be asked; a row cap (default 1,000,000), a byte cap on the
   values read (default 1 GiB) and a statement timeout (default 10 minutes), each changed per
   source with `max_rows`, `max_bytes` and `timeout`.
2. **Combine:** nothing to bound until RP2 brings DuckDB.
3. **Outputs:** the writers stream a page at a time, Excel's in constant memory; a cap on the
   values each output writes (default 1 GiB, `max_size`); Excel's row limit.
4. **The process:** a lower priority everywhere (nice 10 on Linux and macOS, below normal on
   Windows). On Linux, `RLIMIT_DATA` at half the machine's memory and `RLIMIT_CPU` at the run's
   time limit. The app kills a worker that outlives the run's time limit (default 30 minutes,
   `[report] timeout`), and **Cancel** kills it at once.
5. **One run at a time:** **Run** is off in every report's tab while one runs, and says which.

The defaults are named constants in `dbrover-report`. A Settings page for them waits for RP4,
whose metrics show what they should be.

### Reports in the app

- **Explorer:** a **Reports** section under the data sources lists the files in `reports/` by
  their names, with a mark on those with problems. Its menu has **New report** (a commented
  starter file on the first data source), **Open the reports folder** and **Reload**.
- **The report's tab:** its name and description, the file's path with **Edit file** (the
  system's editor), the problems if there are any (and **Run** off), a field for each
  parameter with its type and its check, the output folder (Downloads at first, then the last
  one chosen for the report), **Run** and **Cancel**. While it runs: the step, rows, bytes and
  time. When it ends: each file written with its rows and size, opened with a click, and the
  preview's rows; or the error, with the limit to raise.
- The values and the output folder are remembered per report while the app runs, and the file
  is read again before every run, so an edit in another editor is picked up.

### Layers

Each layer is one pull request in a stack, useful and tested on its own, with synthetic data
only.

- **RP1a — Plan** (built, #389): this section, RP1's decisions and risks, and the roadmap's
  entry in layers.
- **RP1b — Output schemas** (built, #390): in `dbrover-export`, a column schema that maps a result's columns
  to an output's (name, source column, type, Excel format, what to do with a value that doesn't
  fit), exact decimal rounding on text, Excel number formats per column, and the count of cells
  Excel can't hold as numbers.
  **Exit:** golden tests of each type's conversions, and of the CSV and Excel files they make
  (read back with calamine for each cell's type, value and format).
- **RP1c — The report file** (built, #392): the `dbrover-report` crate, with no I/O: the file's types, reading
  it with every problem as a sentence that names its entry, parameter types and values, file
  names, and the plan (the data source by name or id, the output presets, `columns = "same"`).
  `AppPaths::report_files` in the store.
  **Exit:** the trial balance above reads; each problem, later milestones' keys included, is
  reported with its entry; parameter values and file names are checked and formatted; a data
  source is found by name, by id, and not when two share the name.
- **RP1d — Running a report** (built, #393): in `dbrover-report`, the run: the source's rows a page at a time
  into every output and the preview, its caps, the files put in place only when every output
  finished, and its events; the source read through a trait. In `dbrover-engine`, the reader on a
  real data source: read-only, the cost guard, parameters bound, the watchdog.
  **Exit:** with a reader in memory, each cap stops the run, says which, and leaves no file; on
  SQLite, a report writes CSV and Excel with bound dates, and a statement that writes is refused
  before it is sent; on the PostgreSQL test server, a slow source stops at its timeout.
- **RP1e — The worker** (built, #394): `dbrover report-worker`, its JSON Lines, the process limits, and the
  app's side, which starts it, reads its events, kills it on **Cancel** or past the time limit,
  and reports a worker that dies.
  **Exit:** the real binary, given a settings folder with a SQLite data source, writes both files
  and reports each step; killed mid-run, it leaves no file and the app's side says how it ended;
  on Linux, a worker under a small `RLIMIT_DATA` fails cleanly.
- **RP1f — Reports in the app** (built, #395): the explorer's section and the report's tab.
  **Exit:** a kittest with a worker stand-in picks a report, fills its parameters, runs it and
  sees the files and the preview; a failing run shows its error; a file with problems can't run.
- **RP1g — Manual and demo note** (built): a chapter, **Reports**, the examples under
  `examples/reports/` (a trial balance and a general ledger, with `ledger.sql`, which makes
  their synthetic SQLite ledger, and their October 2026 CSV files checked in, which a test
  compares each run with), and RP1's demo note.

**Exit:** from the Reports tab, a one-source report with parameters runs in the worker under its
guardrails and writes CSV and Excel with its declared schema: the trial balance above, on a
SQLite ledger made from the examples' script, opens in Excel with its numbers as numbers in its
formats.

### RP1's decisions

Taken on 2026-11-02 when RP1 started; each can be overturned in review.

| Question | Chosen | Consequence |
| --- | --- | --- |
| Reports per project, or one folder | **One `reports/` folder** in the settings folder, beside `exports.toml`. A source names its data source by name, or by id when two share a name. | A report can combine data sources of different projects, as RP2's valuation joins a ledger and a price database that needn't be in one project, and moving a data source to another project never orphans a report. The folder is one place to sync, keep in git or point RP6's command line at. Projects stay an explorer grouping and the MCP server's scope; if MCP clients run reports one day, a project would offer those whose sources it holds. |
| Engines a source can be on | **Those with a SQL dialect.** | DynamoDB, MongoDB, Elasticsearch and Redis sources wait for RP3's declared columns. |
| Where parameters are filled in | **In the report's tab**, one field per parameter with its type, rather than W6's **Parameter values** dialog. | Each value is checked against its type before the run, and the tab shows what the last run used. The dialog stays a console's. |
| Output options | **By the names `exports.toml` gives them** (`sheet_name`, `separator`). | A preset and an output read the same; the design's example now says `sheet_name`. |
| `format` | **Excel's number format only.** | CSV writes each type's plain text. A text format for CSV dates can come when a report needs one. |
| Rounding | **Half away from zero.** | As PostgreSQL's `round(numeric)` and Excel's `ROUND`, so a total matches what an accountant computes. |
| Where files go | **An output folder chosen in the tab**, Downloads at first; `file` is a plain file name. | A report file stays the same on every machine. RP6's command line will take the folder as an argument. |
| Limits' defaults | **Constants, changed per report** in its file. | No Settings page in RP1; RP4's metrics decide what the defaults should be. |
| Passwords in the worker | **Saved ones, password commands and cloud sign-ins only.** | A data source whose password is typed per session can't be a report's source; its run says so. |
| The memory limit on Linux | **`RLIMIT_DATA`, not `RLIMIT_AS`.** | The address space counts what tokio's thread stacks and the allocator reserve without using; the data limit counts memory written to. |
| Windows | **Below-normal priority, no Job Object** in RP1. | The workspace forbids `unsafe`, and no safe wrapper for Job Objects is in the build. As on macOS, the caps and streaming bound a run; a Job Object comes with RP2, when DuckDB's memory makes it matter. |
| The protocol | **One JSON request on standard input, JSON Lines on standard output**, as `dbrover mcp`'s stdio. | Readable in a terminal, which RP6's command line reuses, and nothing to install. |

### RP1's risks

- **Drivers that read ahead.** The row and byte caps count rows as the run reads them, but a
  driver may buffer rows before they are asked for. The caps stop the statement once crossed;
  until then, on macOS and Windows, only the driver's own buffering bounds memory.
- **Excel's precision, again:** an amount with more than 15 significant digits becomes text
  unless its column rounds it; the warning names the column.
- **A typed password.** People who type a password each session can't run reports on that
  data source until they save it or use a command.

## RP2 — Several sources

Started on 2026-11-02, after RP1. RP2 puts DuckDB between RP1's two ends: each source's rows
are loaded into a DuckDB database of the run's own, DuckDB data sources are attached to it
instead of copied, and a `[combine]` query in DuckDB SQL makes the rows the outputs write.
Outputs, parameters, file names and the worker stay as RP1 built them.

### The report file in RP2

Transactions in a PostgreSQL ledger, valued at the hourly price each asset had when it ran,
from a DuckDB file of prices:

```toml
[report]
name = "Valuation"
description = "Each transaction valued at its asset's price in the hour it ran."
params = { since = "timestamp", until = "timestamp" }

# PostgreSQL: the period's transactions, filtered at the source, then loaded into DuckDB.
[[source]]
id = "tx"
data_source = "ledger-postgres"
query = """
SELECT id, wallet, asset, quantity, executed_at
FROM valuation.transactions
WHERE executed_at >= :since AND executed_at < :until
"""

# DuckDB: the price file, attached read-only and read in place.
[[source]]
id = "prices"
data_source = "prices-duckdb"
attach = true

[combine]
query = """
SELECT tx.executed_at, tx.wallet, tx.asset, tx.quantity, p.price_usd,
       tx.quantity * p.price_usd AS value_usd
FROM tx
ASOF JOIN prices.hourly_prices p
  ON p.asset = tx.asset AND p.hour <= tx.executed_at
ORDER BY tx.executed_at, tx.id
"""
memory_limit = "1 GiB"

[[output]]
format = "xlsx"
file = "valuation-{since:%Y-%m}.xlsx"
sheet_name = "Valuation"
columns = [
  { name = "Executed (UTC)", from = "executed_at", type = "timestamp", format = "yyyy-mm-dd hh:mm" },
  { name = "Wallet",         from = "wallet" },
  { name = "Asset",          from = "asset" },
  { name = "Quantity",       from = "quantity",  type = "decimal(38,18)" },
  { name = "Price (USD)",    from = "price_usd", type = "decimal(38,8)", format = "#,##0.00######" },
  { name = "Value (USD)",    from = "value_usd", type = "decimal(38,2)", format = "#,##0.00" },
]

[[output]]
format = "csv"
file = "valuation-{since:%Y-%m}.csv"
columns = "same"
```

What RP2 reads, beyond RP1:

- **Several `[[source]]`s,** each with an `id` that names its table in DuckDB: letters, digits
  and `_`, starting with a letter, unique in the file, and none of DuckDB's own catalogs
  (`memory`, `system`, `temp`, `main`).
- **`attach = true`** on a source whose data source is a DuckDB file: attached read-only under
  its `id`, so `[combine]` reads `prices.hourly_prices` where the data is. An attached source
  has no `query` and no caps; a DuckDB data source in memory can't be attached.
- **`[combine]`:** `query`, one DuckDB statement that only reads, over the sources' tables and
  the attached databases, with the report's `:name` parameters; and the limits of the DuckDB it
  runs in: `timeout`, `memory_limit`, `threads` and `max_temp_size` (see
  [Guardrails in RP2](#guardrails-in-rp2)).
- **Without `[combine]`,** a report has one source, which isn't attached, and runs as in RP1:
  its rows go straight to the outputs, and DuckDB isn't started. With `[combine]`, the outputs
  write the combine's rows, and a report may have one source or several.
- **Still later:** `lookup` and a source's `columns` (RP3), `prql` (RP5), and two outputs in one
  file, each a problem naming its milestone.

### Loading a source

A source runs as in RP1, through DbRover's own driver, read-only, under the cost guard and its
caps, and its pages are appended to a table of text in DuckDB as they arrive. Once its last row
is in, each column gets a type and the table is rewritten as `CREATE TABLE <id> AS SELECT
CAST(…)`, so the combine step sees numbers as numbers. Sources load at the same time, each on a
connection of its own; RP3's lookups will order those that need an earlier source's keys.

A column's type comes from, in order:

1. **The driver's,** where it says more than the values: a `DECIMAL(18,2)` keeps its
   precision, a float stays `DOUBLE`, and a text column stays `VARCHAR` even when its values
   look like numbers, so `0042` and `1000` codes keep their form. A text column whose every
   value is a date or a time is the exception, since SQLite keeps dates as text: it gets the
   type of the values.
2. **The values,** when the driver reports none (PostgreSQL's text protocol, MySQL's
   expressions) or a decimal without its precision (`numeric`):

   | Every value is | Type |
   | --- | --- |
   | a whole number, no leading zero, up to 18 digits | `BIGINT` |
   | a whole number up to 38 digits | `HUGEINT` |
   | a decimal number, at most 38 digits in all | `DECIMAL(p,s)`: the most digits before the point and the most after it among the values |
   | a number with an exponent (`1.5e+20`) | `DOUBLE` |
   | `t`/`f` or `true`/`false` | `BOOLEAN` |
   | a date, `2026-10-01` | `DATE` |
   | a date and time, without an offset | `TIMESTAMP` |
   | a date and time with an offset | `TIMESTAMP WITH TIME ZONE`, read in UTC |
   | anything else, or a mix | `VARCHAR` |

   Empty values (`NULL`) don't count. A decimal of more than 38 digits stays text, and the
   source's result says so once per column: DuckDB's `DECIMAL` holds 38, and casting it in the
   combine step is the author's choice to make.

The run reports each loaded table's columns and their types, so the tab shows what the
combine step works with.

### Exact numbers in RP2

From the driver to the file, a decimal never passes through `f64`: drivers hand over text,
which loads into `DECIMAL(p,s)` with the digits it has, and DuckDB's results come back as text
again, written by RP1b's schemas. In the combine step, DuckDB keeps decimals exact through
`+`, `-`, `*`, `SUM` and `ROUND` (a product's scale is the sum of its factors', up to 38
digits), but **`/` and `power()` return `DOUBLE`**. The manual says to multiply by a decimal
(`* 0.01`) or `CAST(a / b AS DECIMAL(38,18))` where a division is needed, and the output's
declared type then rounds once, at the end.

DuckDB's `TimeZone` is set to UTC in the run's database, so a `TIMESTAMP WITH TIME ZONE` reads
the same on every machine.

### Guardrails in RP2

By layer, as in [Guardrails](#guardrails), what changes from RP1:

1. **Sources:** each copied source keeps RP1's caps (`max_rows`, `max_bytes`, `timeout`), counted
   as its rows load. An attached source copies nothing, so it has none; what the combine step
   reads from it is bounded by the next layer.
2. **Combine:** the run's DuckDB lives in memory, with
   - `memory_limit`, default **2 GiB**, past which DuckDB spills to disk, and fails what can't
     spill;
   - `threads`, default **half the processor's cores**;
   - `temp_directory` in the run's staging folder, removed with it, and `max_temp_size`
     (DuckDB's `max_temp_directory_size`), default **10 GiB**;
   - `timeout`, default **10 minutes**, after which the statement is interrupted;
   - no extension downloads or loads, no files read or written by the query (DuckDB's
     `enable_external_access` is turned off once the sources are attached), and its
     configuration locked, so a query can't raise its own limits;
   - the query checked to be one statement that only reads.

   A run stopped by DuckDB's memory or disk limit names `memory_limit` or `max_temp_size` in
   `[combine]`, as RP1's caps do.
3. **Outputs:** as in RP1.
4. **The process:** as in RP1. On Linux, `RLIMIT_CPU` grows to the run's time limit times
   DuckDB's threads and one, since DuckDB's threads use the processor at once. On macOS and
   Windows, DuckDB's `memory_limit` is now the main bound on a run's memory.
5. **One run at a time,** as in RP1.

### Progress in RP2

The worker's events name the step they are about. `started` lists the sources (attached or
not) and whether there is a combine step; `progress` names its step; a loaded source's
`source_done` carries its table's columns and types and any warning about them; an attached
source sends `source_attached`; the combine step ends with `combine_done`; `step` in a failure
may now be `combine`.

The report's tab shows a line per step: each source with its data source, rows, bytes and time
(or *attached*), then **Combine**, then the outputs; the step that is running has a spinner, a
failed one its error. A loaded source's line opens to show its table's columns and types.

### Layers

Each layer is one pull request in a stack, useful and tested on its own, with synthetic data
only.

- **RP2a — Plan** (built, #398): this section, RP2's decisions and risks, the roadmap's entry in layers, and
  the design's example made exact (no `power()` in it).
- **RP2b — The run's DuckDB** (built, #399): in `dbrover-driver-duckdb`, a database in memory with its limits
  set, tables loaded a page of text at a time and then cast to their types, DuckDB files
  attached read-only, and the lock-down before a query.
  **Exit:** tests in memory: a table of text loads and casts to `DECIMAL(38,18)` with every
  digit; a file attaches read-only and refuses a write; once locked, a query can't read a file,
  load an extension or change `memory_limit`; a query over a small `memory_limit` spills to the
  temporary folder, and one over a small `max_temp_size` fails saying so.
- **RP2c — The report file** (built, #401): several sources, `attach`, `[combine]` and its limits, read and
  planned with their problems; `Plan` holds a list of sources and either a combine step or
  one source; the run opens sources through a trait, so the worker no longer opens them
  itself; the events name their step. A combine plan fails at run time, saying RP2d brings it.
  **Exit:** the valuation above reads and plans; each new problem (an id that can't name a
  table, two sources with one id, `attach` on a PostgreSQL source or with a query, a combine
  that writes, two sources without `[combine]`) is reported with its entry; RP1's examples run
  as before.
- **RP2d — Combining** (built, #402): in `dbrover-report`, each column's type from the driver or its values,
  the sources loaded at the same time under their caps, the attached ones attached, the combine
  query under its timeout and DuckDB's limits, its rows into the outputs, and the events; in the
  worker, the processor-time limit. In `dbrover-engine`, the opener on real data sources.
  **Exit:** with readers in memory, two sources and a combine write CSV and Excel whose
  18-decimal products match to the last digit; each limit (a source's cap, `timeout`,
  `memory_limit`, `max_temp_size`) stops the run, says which, and leaves no file; on the
  PostgreSQL test server with a DuckDB file attached, the valuation writes its files.
- **RP2e — The tab** (built, #403): a line per step with its progress, a loaded source's columns and types,
  and the failing step marked.
  **Exit:** a kittest with a worker stand-in shows two sources, one attached, and the combine
  step progress to the end, and a run failing at the combine step marks it.
- **RP2f — Manual and demo note** (built): the **Reports** chapter's sections on several sources,
  attaching and combining, exact numbers in DuckDB, and the new limits; the valuation under
  `examples/reports/` with `valuation-postgres.sql` and `valuation-prices.sql` making its
  synthetic data, its October 2026 CSV checked in and compared by a test; RP2's demo note.

**Exit:** from the Reports tab, a report with a PostgreSQL source and a DuckDB source, combined
in DuckDB SQL, writes CSV and Excel with exact decimals inside the worker's limits: the
valuation above, on the examples' synthetic data, whose 18-decimal quantities times 8-decimal
prices match a computation by hand to the cent, and to the last digit before rounding.

### RP2's decisions

Taken on 2026-11-02 when RP2 started; each can be overturned in review.

| Question | Chosen | Consequence |
| --- | --- | --- |
| Where DuckDB's code lives | **A `Workspace` in `dbrover-driver-duckdb`,** which `dbrover-report` uses. | One crate knows how DuckDB is built and linked (bundled or prebuilt), and reuses its Arrow-to-text formatting and cancel; `dbrover-report` stays free of the `duckdb` crate's build switch, and its tests run DuckDB in memory, as the design wanted. |
| Typing a loaded source | **The driver's type when it has one, else inferred from the values,** exactly, after loading as text. | PostgreSQL, whose text protocol reports no types, gets `DECIMAL(p,s)` for its `numeric`s with no declaration. A column with leading zeros or mixed values stays text. Declaring a source's columns, which overrides this, comes with RP3's declared columns. |
| A decimal past 38 digits | **Text, with a warning.** | DuckDB's `DECIMAL` holds 38 digits; guessing which digits to drop would be silent loss. |
| How sources load | **At the same time,** each on its own DuckDB connection. | A report's time is its slowest source, not their sum. RP3's lookups add the order some sources need. |
| One source without `[combine]` | **Runs as in RP1,** without DuckDB. | RP1's reports and examples are unchanged, and a simple report starts no DuckDB. |
| Where spilled data goes | **The run's staging folder,** inside the output folder. | One folder to remove however the run ends, which the app already removes after a killed worker. Spills land on the output folder's disk. |
| Limits' defaults | **Constants:** 2 GiB of memory, half the cores, 10 GiB spilled, 10 minutes. | As RP1's; RP4's metrics will say whether they should move. A quarter of the machine's memory, as the design first said, would need a dependency to read on macOS and Windows, and makes a report run differently from one machine to the next. |
| Where DuckDB's limits are set | **In `[combine]`.** | They bound the database the sources load into and the combine runs in, which exists only with a combine step. |
| Locking the query down | **DuckDB's own settings** (`enable_external_access`, `lock_configuration`, no extension autoload), plus a check that the query is one statement that only reads. | The combine step reads only what the report loaded or attached; the attached files are read-only. |
| Windows | **Still no Job Object.** | DuckDB's `memory_limit` bounds a run's largest user of memory on Windows, as on macOS; a Job Object waits for a safe wrapper in the build. |
| A DuckDB file the app has open | **The run fails, saying so,** and the manual says to mark the data source read-only. | DuckDB locks a file one way at a time between processes: a writable connection in the app keeps the worker from attaching it, while a read-only one doesn't. |

### RP2's risks

- **Inferred types.** A column whose values happen to be whole numbers in one period and
  decimals in the next gets a different type each run; the combine step's arithmetic still
  works, but a comparison with text doesn't. RP3's declared columns fix a type.
- **Division.** `/` in the combine step returns `DOUBLE`; the manual's examples cast, and an
  output's decimal type rounds the result, but a careless `/` loses digits past 15.
- **Spilling to the output folder's disk.** A large combine fills the disk the files go to,
  up to `max_temp_size`.
- **A file open for writing.** A DuckDB data source the app has open for writing can't be
  attached by the worker; the run's error says to make it read-only or disconnect it.

## Decisions

- **DuckDB combines.** It's bundled, it spills to disk, and it gives full analytical SQL.
  DuckDB's own scanners for PostgreSQL and MySQL would push filters to the source, but cover
  only some engines, download extensions, and bypass DbRover's tunnels, password commands,
  read-only mode and cost guard. Sources go through DbRover's drivers; DuckDB sources attach.
- **TOML and SQL, not a new language.** TOML matches every other file DbRover reads; SQL and
  PRQL carry the logic.
- **A worker process.** Limits inside one process can't stop every way a query can run out of
  memory; a separate process can be killed without losing the app's state.

## Open questions

- Should MCP clients be able to run a report? Read-only and bounded, it would suit them, but it
  waits until RP4's metrics show what runs cost.
- Where the run history lives when the settings folder is synced (H4's warning about a synced
  `history.sqlite` applies here too).

## Risks

- **Scope.** Reports pull towards BI: charts, schedules, sharing. The Scope section is the line,
  and Vision repeats it.
- **Copying large sources.** A source without a filter copies its whole result into DuckDB. The
  row and byte caps stop it, and the metrics make it visible, but the author has to filter.
- **Excel and precision.** Excel can't hold an 18-decimal amount as a number. Writing it as text
  is correct but surprises people who want to sum it; the warning says why.
- **macOS limits.** With no reliable address-space limit, a runaway DuckDB query on macOS is
  bounded by DuckDB's own memory limit, not by the operating system.
