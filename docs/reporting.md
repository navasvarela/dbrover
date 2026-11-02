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
`actions.toml` and `exports.toml`: problems are listed in the file's tab and counted in the
status bar, and a report with a problem can't run. TOML gives the structure; the logic is in
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
query = "SELECT asset_id, symbol, decimals, chain FROM assets WHERE asset_id IN :asset_ids"
columns = [
  { name = "asset_id", type = "text" },
  { name = "symbol",   type = "text" },
  { name = "decimals", type = "integer" },
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
       tx.amount / power(10, assets.decimals)               AS quantity,
       p.price_usd,
       tx.amount / power(10, assets.decimals) * p.price_usd AS value_usd
FROM tx
JOIN assets USING (asset_id)
ASOF JOIN prices.hourly_prices p
  ON p.asset_id = tx.asset_id AND p.hour <= tx.executed_at
ORDER BY tx.executed_at
"""

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
  sources, exact decimals end to end.
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
parameter values, the output folder) and reads JSON Lines from its standard output until it
exits: the run starting, the source's progress (rows and bytes, once a second), a preview of the
first 200 rows, each file written (path, rows, size, time, warnings), and how the run ended. A
failure says which step failed, and for a limit, which one, its value, and the key that raises
it. Standard error goes to the app's log.

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

- **RP1a — Plan:** this section, RP1's decisions and risks, and the roadmap's entry in layers.
- **RP1b — Output schemas:** in `dbrover-export`, a column schema that maps a result's columns
  to an output's (name, source column, type, Excel format, what to do with a value that doesn't
  fit), exact decimal rounding on text, Excel number formats per column, and the count of cells
  Excel can't hold as numbers.
  **Exit:** golden tests of each type's conversions, and of the CSV and Excel files they make
  (read back with calamine for each cell's type, value and format).
- **RP1c — The report file:** the `dbrover-report` crate, with no I/O: the file's types, reading
  it with every problem as a sentence that names its entry, parameter types and values, file
  names, and the plan (the data source by name or id, the output presets, `columns = "same"`).
  `AppPaths::reports_dir` in the store.
  **Exit:** the trial balance above reads; each problem, later milestones' keys included, is
  reported with its entry; parameter values and file names are checked and formatted; a data
  source is found by name, by id, and not when two share the name.
- **RP1d — Running a report:** in `dbrover-report`, the run: the source's rows a page at a time
  into every output and the preview, its caps, the files put in place only when every output
  finished, and its events; the source read through a trait. In `dbrover-engine`, the reader on a
  real data source: read-only, the cost guard, parameters bound, the watchdog.
  **Exit:** with a reader in memory, each cap stops the run, says which, and leaves no file; on
  SQLite, a report writes CSV and Excel with bound dates, and a statement that writes is refused
  before it is sent; on the PostgreSQL test server, a slow source stops at its timeout.
- **RP1e — The worker:** `dbrover report-worker`, its JSON Lines, the process limits, and the
  app's side, which starts it, reads its events, kills it on **Cancel** or past the time limit,
  and reports a worker that dies.
  **Exit:** the real binary, given a settings folder with a SQLite data source, writes both files
  and reports each step; killed mid-run, it leaves no file and the app's side says how it ended;
  on Linux, a worker under a small `RLIMIT_DATA` fails cleanly.
- **RP1f — Reports in the app:** the explorer's section and the report's tab.
  **Exit:** a kittest with a worker stand-in picks a report, fills its parameters, runs it and
  sees the files and the preview; a failing run shows its error; a file with problems can't run.
- **RP1g — Manual and demo note:** a chapter, **Reports**, the examples under
  `examples/reports/` with a script that makes their synthetic database, and RP1's demo note.

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
