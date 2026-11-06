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
- A command-line runner, so cron or a CI job can run a report with no server (RP6).

Out, and staying out:

- Dashboards, charts that refresh, and a BI semantic layer. A chart stays one result's
  picture: in a console, or as a file a report writes beside its others
  ([Charts as code](charts-as-code.md#the-scope-line), CH3).
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
  so a later source reads by key instead of scanning: `IN :ids` on DynamoDB becomes reads by
  key, and on PostgreSQL a list of the values (see [Lookups](#lookups)). Sources run in the
  order their lookups need; the others run at the same time. A lookup with more values than
  the engine takes in one statement is split into batches.
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
  sources. The first iteration's exit: the four accounting reports above run and match. In
  layers RP3a to RP3g, [below](#rp3--lookups-and-documents).
- **RP4 — Metrics:** the run history, the comparison of runs, the hints, and a Settings page
  for the limits' defaults. In layers RP4a to RP4f, [below](#rp4--metrics).
- **RP5 — PRQL, and the track finished:** `prql` in `[combine]`, with the compiled SQL shown,
  and what RP4 and RP6 left for later. In layers RP5a to RP5c, RP4g to RP4j and RP6g to RP6j,
  [below](#rp5--prql-and-the-track-finished).
- **RP6 — The command line:** `dbrover report run <file> --param since=…`, for cron and CI,
  with the worker's guardrails and an exit code per outcome. Built in layers RP6a to RP6f,
  [below](#rp6--the-command-line).
- **RP7 — Manual,** with the examples: the chapter brought up to RP6, PRQL and RP5's other
  layers, [below](#rp7--manual).

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
- **The report's tab** (laid out again in RP8, with the file edited in it: see
  [The report's page and its editor](report-editor.md)): its name and description, the file's
  path with **Edit file** (the
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

## RP3 — Lookups and documents

Started on 2026-11-03, after RP2. RP3 lets a source read by the keys an earlier source found,
and lets a source be on an engine that returns documents, by declaring the columns it makes.
With both, the four reports of [Testing with real reports](#testing-with-real-reports) run on
the examples' synthetic data, and the first iteration is done. The combine step, outputs,
parameters and the worker stay as RP1 and RP2 built them.

### The report file in RP3

The valuation of the design's example, with the assets' metadata in DynamoDB, read by the keys
the period's transactions use:

```toml
[report]
name = "Valuation"
params = { since = "timestamp", until = "timestamp" }

# PostgreSQL: the period's transactions. amount is in the asset's smallest unit.
[[source]]
id = "tx"
data_source = "ledger-postgres"
query = """
SELECT id, wallet, asset_id, amount, executed_at
FROM valuation.transactions
WHERE executed_at >= :since AND executed_at < :until
"""
columns = [
  { name = "id",          type = "integer" },
  { name = "wallet",      type = "text" },
  { name = "asset_id",    type = "text" },
  { name = "amount",      type = "decimal(38,0)" },
  { name = "executed_at", type = "timestamp" },
]

# DynamoDB: the assets those transactions use, read by key, never scanned.
[[source]]
id = "assets"
data_source = "assets-dynamodb"
lookup = { from = "tx", column = "asset_id", as = "asset_ids" }
query = 'SELECT asset_id, "symbol", unit, metadata FROM valuation_assets WHERE asset_id IN :asset_ids'
columns = [
  { name = "asset_id", type = "text" },
  { name = "symbol",   type = "text" },
  { name = "unit",     type = "decimal(38,18)" },
  { name = "chain",    from = "metadata.chain", type = "text", default = "unknown" },
]

[[source]]
id = "prices"
data_source = "prices-duckdb"
attach = true

[combine]
query = """
SELECT tx.executed_at, tx.wallet, assets.symbol, assets.chain,
       tx.amount * assets.unit AS quantity, p.price_usd,
       tx.amount * assets.unit * p.price_usd AS value_usd
FROM tx
JOIN assets USING (asset_id)
ASOF JOIN prices.hourly_prices p
  ON p.asset = assets.symbol AND p.hour <= tx.executed_at
ORDER BY tx.executed_at, tx.id
"""
```

What RP3 reads, beyond RP2:

- **`lookup = { from, column, as, batch_size }`** on a source: `from` names a source listed
  before it, which is loaded (not attached); `column` is one of that source's columns; `as` is
  the name the query uses as `:asset_ids`, which can't be one of `params`. `batch_size`, at
  most the engine's own (see [Lookups](#lookups)), lowers how many values one statement
  takes. One lookup a source.
- **A source's `columns`:** a list of `{ name, from, type, default }`. `name` is the column in
  the source's table; `from` is where its value is, `name` by default: a result's column, or a
  document's field by its path (`metadata.chain`, `lines[0].sku`); `type` is one of the output
  types (`text`, `integer`, `decimal`, `decimal(p,s)`, `boolean`, `date`, `timestamp`), `text`
  when left out; `default` is the value a missing or `null` field gets, checked against the
  type when the file is read.
- **Sources on every engine.** A source on an engine without a SQL dialect (DynamoDB, MongoDB,
  Elasticsearch, OpenSearch, Redis) declares its `columns`, since its rows are documents or
  replies with no fixed columns; its query is in the engine's own language, with the report's
  `:name` parameters. A source on a SQL engine may declare them, to fix its types.
- **Still later:** `prql` (RP5) and two outputs in one file, each a problem naming its
  milestone, as before.

### Lookups

A lookup binds its parameter to the distinct values of the earlier source's column, without
`NULL`, in their order, once that source is loaded. Each value goes into the statement as a
literal of the source's language, typed by its column in DuckDB: a number unquoted, anything
else quoted. The list is written where `:asset_ids` is, as that language writes a list:

| Language | A query reads | `:ids` becomes |
| --- | --- | --- |
| SQL, every dialect | `WHERE id IN :ids` | `('a', 'b', 7)` |
| PartiQL (DynamoDB) | `WHERE pk IN :ids` | `['a', 'b', 7]` |
| MongoDB's shell | `db.assets.find({_id: {$in: :ids}})` | `["a", "b", 7]` |
| Elasticsearch's requests | `{"terms": {"id": :ids}}` | `["a", "b", 7]` |
| Redis's commands | `MGET :ids` | `a b 7`, each quoted if it needs to be |

The report's own parameters go into a statement as they did: bound where a SQL engine's driver
binds, and for the other languages written in as literals of their type, as the lookup's are (a
date in MongoDB's shell is `ISODate("2026-10-01T00:00:00Z")`).

**Order.** A source with a lookup runs once the source it reads from is loaded. Sources run in
rounds: the first holds every source without a lookup, and each later round the sources whose
lookup reads from the round before; a round's sources load at the same time, as RP2's do. A
lookup can only name a source above it in the file, so the rounds always end.

**Batches.** A lookup with more values than one statement takes runs its query once per batch,
the values in order, and every batch's rows go into the source's one table. The most values a
statement takes, per language:

| Language | Values a statement | Why |
| --- | --- | --- |
| PartiQL (DynamoDB) | 100 | DynamoDB refuses an `IN` of more: "The IN operator is provided with too many operands". |
| CQL (Cassandra, ScyllaDB) | 100 | ScyllaDB's default `max_partition_key_restrictions_per_query`, and Cassandra's guardrail warns past it. |
| Every other language | 1,000 | Statements stay well under ClickHouse's 256 KiB `max_query_size` and SQL Server's limits on a long `IN` list, and a MongoDB or Elasticsearch request stays small. |

`batch_size` lowers it, for an engine that bills per value read or a server set to take fewer.

**No values.** When the earlier source returned no value, a source with declared columns sends
nothing and its table is made empty with those columns; a SQL source without them runs once
with the list `(NULL)`, which matches no row, so its table has the query's columns.

### Declared columns

A source's declared columns are what its table has, in their order, and nothing else: a field
not declared is dropped. Each value is checked against its type and written in the type's plain
form, as an output's declared column is (RP1b), then loaded; the table's types are the declared
ones, not the values', so a column keeps its type from one run to the next:

| `type` | DuckDB's type |
| --- | --- |
| `text` | `VARCHAR`; an object or array is written as its JSON |
| `integer` | `BIGINT`, so at most 18 digits |
| `decimal` | `DECIMAL(p,s)` from the values, as RP2 types an undeclared one |
| `decimal(p,s)` | `DECIMAL(p,s)`, rounded half away from zero; `p` at most 38 |
| `boolean` | `BOOLEAN` |
| `date` | `DATE` |
| `timestamp` | `TIMESTAMP`; a time with an offset is converted to UTC |

- **Rows** (a SQL engine, Redis's replies of rows) give each declared column the result's
  column named by its `from`; a name the result doesn't have fails the run, listing the ones
  it has.
- **Documents** give it the field at its `from` path. Extended JSON's values are read as their
  value: `{"$numberLong": "7"}` is `7`, `{"$numberDecimal": "0.1"}` is `0.1`, an `ObjectId` its
  hex, and a `Date` its time in UTC, `2026-10-01 12:00:00`. DynamoDB's numbers keep their
  digits.
- **A value that doesn't fit** its type fails the run, naming the source, the column, the item
  (its number in the source, and its `_id` when it has one) and the value. A missing field or
  `null` gets the column's `default`, or `NULL`.

### Guardrails in RP3

By layer, as in [Guardrails](#guardrails), what changes from RP2:

1. **Sources:** a source's `max_rows`, `max_bytes` and `timeout` hold for the whole source,
   every batch included: the caps count rows and bytes across batches, and the timeout runs
   from the first batch's start. A source with declared columns counts the bytes of the values
   it keeps. Every statement is checked to only read and
   asked of the cost guard as RP1's are; on DynamoDB, a lookup on the partition key reads by
   key and the cost guard lets it through, while a lookup on another attribute is a scan,
   which a guarded data source refuses.
2. **Combine:** as in RP2. A lookup's values are read from the loaded table in the run's
   DuckDB, under its `memory_limit`.
3. **Outputs, the process, one run at a time:** as in RP2.

### Progress in RP3

`started` names, for each source with a lookup, the source it waits for. `progress` on a
batched source says which batch of how many it is reading, and its `source_done` how many
batches it ran. The report's tab shows a source that waits as *waiting for tx*, and a batched
one's line as *batch 2 of 7*.

### Layers

Each layer is one pull request in a stack, useful and tested on its own, with synthetic data
only.

- **RP3a — Plan** (built, #407): this section, RP3's decisions and risks, and the roadmap's entry in layers.
- **RP3b — Parameters in every language** (built, #408): in `dbrover-language`, `:name` parameters found in
  each language's commands, literals and lists written as each language reads them, and the
  most values a statement takes; in `dbrover-document`, a path read from its text and a scalar's
  plain value (Extended JSON's numbers, ids and dates).
  **Exit:** unit tests, per language, of the parameters found (not inside strings, comments or
  `asset:id`-like keys), each literal and list, and a statement with parameters replaced; of
  paths read back from their text; and of each Extended JSON scalar's value.
- **RP3c — The report file** (built, #410): `lookup` and a source's `columns` read and planned with their
  problems; sources on every engine, a source's query in its language; the rounds; the events'
  new fields. A run whose plan needs lookups or declared columns fails, saying RP3d brings them.
  **Exit:** the valuation above reads and plans into two rounds; each new problem (a lookup
  from a later, unknown or attached source, an `as` that is a parameter or that the query
  doesn't use, a `batch_size` over the engine's, a document source without `columns`, a
  `default` that doesn't fit its type, two declared columns of one name) is reported with its
  entry; RP1's and RP2's examples plan and run as before.
- **RP3d — Running lookups and documents** (built, #412): in `dbrover-report`, declared columns taken from
  rows and documents, a lookup's values read from DuckDB, its batches under the source's caps
  and timeout, the rounds; in `dbrover-engine`, a source in any language, and its result's
  shape.
  **Exit:** with readers in memory, a lookup of 2,500 values runs three batches of at most
  1,000 into one table, an empty lookup sends nothing, caps and the timeout count across
  batches, and a value that doesn't fit names its item; on DynamoDB Local, a lookup on the
  partition key reads by key under the cost guard, and one on another attribute is refused;
  on the MongoDB test server, a lookup with nested fields, defaults and `NumberLong`s loads
  its declared columns.
- **RP3e — The tab** (built, #413): a source waiting for another, its batches, and its declared columns
  marked as declared.
  **Exit:** a kittest with a worker stand-in shows a lookup waiting, then its batches, then
  done.
- **RP3f — The four reports** (built, #414): under `examples/reports/`, the trial balance with subtotals per
  kind of account (`GROUPING SETS`), the general ledger with each entry's counterparty from
  MongoDB and its running balance, the valuation above, and cost basis with realised gains by
  FIFO lot; the scripts that make their synthetic data in SQLite, PostgreSQL, DynamoDB, MongoDB
  and DuckDB; their October 2026 CSV files checked in, and a test that runs each in the worker's
  code and compares.
  **Exit:** the four match their checked-in files, and the cost basis also matches a FIFO worked
  out row by row outside SQL; the general ledger, on a ledger of 1,100,000 entries, writes every
  row to CSV and goes on to a second Excel sheet, with each account's closing balance equal to
  its sum (a test marked slow, run with `--ignored`).
- **RP3g — Manual and demo note** (built): the **Reports** chapter's sections on lookups, document
  sources and declared columns, and the four reports; RP3's demo note.

**Exit:** the first iteration's: the four accounting reports run from their files, each in the
worker under its guardrails, on synthetic data in SQLite, PostgreSQL, DynamoDB, MongoDB and
DuckDB, and write what their checked-in files say.

### RP3's decisions

Taken on 2026-11-03 when RP3 started; each can be overturned in review.

| Question | Chosen | Consequence |
| --- | --- | --- |
| How a lookup's values reach the engine | **Written into the statement as literals,** a list as the language writes one, rather than bound. | One way for every engine, whether its driver binds or not, and the read-only check and the cost guard see the statement that runs. PostgreSQL's `= ANY(:ids)` with an array parameter, which the design first mentioned, isn't offered: a lookup is `IN :ids` there as everywhere. Quoting is the language's own, as W6's literals are. |
| A literal's type | **The earlier column's type in DuckDB:** numbers unquoted, everything else quoted. | A MongoDB `_id` that is a number matches a number; a key stored as text must be loaded as text, which a declared `text` column makes sure of. |
| How many lookups a source has | **One,** reading one column. | Two lists would batch as their product. A key of two columns is looked up by one and filtered in `[combine]`. |
| What a lookup reads from | **A source above it, loaded, not attached.** | The order is the file's, so there is never a cycle. An attached file's tables are read in `[combine]`, where a join does what a lookup would. |
| Running sources in order | **Rounds:** each round's sources at the same time, a round once the one before it is loaded. | Simple to show and to reason about; a source in round two waits for every source of round one, not only its own, which costs time only when round one's sources take very different times. |
| The most values a statement | **Constants per language:** 100 for PartiQL and CQL, 1,000 otherwise; `batch_size` lowers it. | No engine is asked more than it takes by default; raising them waits until a report needs it. |
| Caps and timeout across batches | **For the whole source.** | A source's limits mean what they say however many batches it needs; the engine's watchdog is given the source's deadline, not one per statement. |
| Sources without a SQL dialect | **Declare their columns,** and only declared fields are kept. | A table has the same columns on every run, whatever fields the period's documents happen to have; nothing is guessed from the first page. |
| Declared types | **The output types**, mapped to DuckDB's (`integer` is `BIGINT`). | One vocabulary for a report's types; a `decimal(p,s)` past 38 digits is a problem in the file, since DuckDB holds 38. |
| A value that doesn't fit | **Fails the run,** naming the item. | As the design says. `if_invalid`, which outputs have, waits until a source needs it. |
| Cost basis | **In SQL,** with no program step: each lot and each disposal is a range of the asset's running quantity, and FIFO matches the ranges that overlap. | The design expected this report to need a program step; window functions and a join do it, exactly, in decimals. A program step waits for a report that needs one. |
| The general ledger past a million rows | **A test marked slow** (`--ignored`), on a ledger the test makes, besides the October example's checked-in file. | CI time is a budget; a million rows through a debug build takes minutes. The test is run by hand before a release, as Excel's whole-sheet test is. |
| The examples' DynamoDB table and MongoDB collection | **Fixed names** (`valuation_assets`, `counterparties`), made by scripts beside the reports: PartiQL `INSERT`s after an `aws dynamodb create-table`, and a MongoDB shell script. | The report files run as they are in the manual and in the test, which makes them afresh. |

### RP3's risks

- **DynamoDB's `IN` limit.** DynamoDB's documentation doesn't state how many values an `IN`
  takes; 100 is what DynamoDB Local accepts, refusing 101. If AWS refuses fewer, `batch_size`
  lowers it until the constant changes.
- **PartiQL's reserved words.** DynamoDB refuses a statement that names an attribute such as
  `symbol` or `value` unquoted, with only "Statement wasn't well formed"; the manual says to
  quote them, as the examples do.
- **Long statements.** A thousand long keys make a statement of hundreds of kilobytes, near
  ClickHouse's `max_query_size`; the error is the engine's, and `batch_size` is the fix.
- **A lookup's values in memory.** The distinct values are read into the worker before the
  batches run. The earlier source's `max_rows` bounds how many there can be.
- **Number or text keys.** A key loaded as a number looks up numbers. An earlier source whose
  ids are digits in a text column of a driver that reports no types, such as PostgreSQL's, is
  inferred a number by RP2's typing; declaring it `text` keeps it text.
- **Tests that need servers.** The valuation, cost basis and general ledger need the
  PostgreSQL, DynamoDB and MongoDB test servers, which CI starts on Linux only; on macOS and
  Windows only the trial balance, on SQLite, runs end to end.

## RP4 — Metrics

Started on 2026-11-03, after RP3. RP4 keeps what each run measured, so the report's tab can
put the last runs side by side, say which step took the time, and suggest what to change.
It also gives the limits' defaults a Settings page, which RP1 and RP2 left to RP4. The report
file, the combine step, the outputs and the guardrails stay as RP1 to RP3 built them.

### The report file and the worker in RP4

The report file doesn't change. What a run measures comes from the worker's events, which RP1
to RP3 already fill with most of it; RP4 adds what is missing:

| Step | Recorded | Already sent | New in RP4 |
| --- | --- | --- | --- |
| Each source | time to the first row, total time, rows, bytes read, batches for a lookup | `source_done`: all of it | — |
| Combine | time, DuckDB's peak memory, the most it spilled to disk | `combine_done`: time, rows, time to the first row | `peak_memory`, `peak_temp_size` |
| Each output | time, rows, file size | `output_written`: rows, size | `elapsed_ms` |
| The run | total time, the parameters, how it ended | `finished` or `failed`; the app has the parameters and sees **Cancel** | — |

- **DuckDB's peak memory and spill** come from DuckDB's own profiler, turned on in the run's
  DuckDB before it is locked down: after the combine query, the profiler's
  `SYSTEM_PEAK_BUFFER_MEMORY` and `SYSTEM_PEAK_TEMP_DIR_SIZE` are the most memory DuckDB's
  buffer manager held and the most it held in its temporary folder, over the run's whole
  DuckDB: loading the sources and the combine query. They are named after the keys that bound
  them, `memory_limit` and `max_temp_size`, so the tab can show each beside its limit. The
  profiler writes nothing anywhere (`no_output`), so the lock-down stays as it is.
- **An output's time** is the time spent in its writer: converting each page to its schema,
  writing it, and completing the file. Outputs are written while their rows stream in, so
  their time overlaps the source's or the combine step's, and the tab says so.
- **A run that fails** keeps what its steps measured up to the failure, and its error; one
  that is cancelled, what the app had seen.

### Run history

Each run is kept in the history database W4 made (`history.sqlite` in the settings folder), in a
table of its own, `report_runs`, one row a run:

| Column | Holds |
| --- | --- |
| `report` | the report's file name without `.toml`, as the explorer lists it |
| `started_at` | when the run started, in UTC, as the history's other times |
| `elapsed_ms` | the run's total time |
| `outcome` | `finished`, `failed` or `cancelled` |
| `params` | the values the run used, as JSON |
| `steps` | each source, the combine step and each output with what it measured, as JSON |
| `failure` | the failed step and its error, for a run that failed |

The schema's version goes from 2 to 3. As W4's rule says, the migration only adds: a table and
its index, which an older DbRover neither reads nor minds. The app writes a run's row once the
run ends, however it ends: the worker doesn't open the history, so a killed worker's run is kept
too. Only the newest 20 runs of each report are kept; Settings › Reports changes how many. Rows
are deleted when a run is added, so there is nothing to prune at start-up.

The parameters are kept as typed. They are dates, numbers and names, not secrets; a report
whose parameters are something to keep out of a file is one the manual says to run with care,
as the query history already keeps every console's SQL.

### The report's tab in RP4

Below the last run, a **Runs** section shows the report's newest runs, at most five, side by
side: a column per run, newest first, headed by when it started, how it ended and its total
time; under it the parameters, then a row per step with its time and rows, or an output's file
size (bytes, time to the first row, batches, DuckDB's peak memory and spill, and the file's name
in the cell's tooltip). A run of an older version of the file, whose steps differ, shows the
steps it had and — for the others.

- **A step's own time.** The step whose rows the outputs write (the combine step, or a report's
  one source without it) writes each page before it reads the next, so its time holds the
  outputs'. The table takes the outputs' time off it, so a slow writer shows as its output, not
  as its feed.
- **The step that dominates** is the one with the longest share of the run's time, by its own
  time. It is in the accent colour in each run's column, and named under the table when every
  finished run listed agrees: "Source entries takes the most time in every run listed: 80% of
  the newest." Sources of one round run at the same time, so shares can add up to more than the
  whole.
- **Hints** come from the newest finished run, each one sentence that names the step and the
  number behind it:
  - a source that reads at least 10,000 rows and ten times as many as the outputs write:
    "**Source entries** read 1,100,000 rows for 40 written. If the combine step drops most of
    them, filter them in the source's query, where its database can use an index." An
    aggregation reads more than it writes on purpose, so the hint says what to look for, not
    that something is wrong;
  - a combine step that spilled to disk: "Combine spilled 3.2 GB to disk, past its
    memory_limit. A larger memory_limit in [combine], or fewer rows from the sources, keeps it
    in memory."
- **Clear history**, beside the section's heading so it stays in reach, deletes the report's
  runs after asking.

The tab reads the runs from the history when it opens, and again after each run.

### Settings › Reports

A page for the limits' defaults, which a report's file overrides key by key, as before:

| Setting | Key in `settings.toml`'s `[reports]` | Default |
| --- | --- | --- |
| Rows a source may return | `max_rows` | 1,000,000 |
| Values a source may return | `max_bytes` | 1 GiB |
| A source's time limit | `source_timeout` | 10 minutes |
| Memory before spilling to disk | `memory_limit` | 2 GiB |
| Spilled to disk at most | `max_temp_size` | 10 GiB |
| Threads | `threads` | half the processor's cores |
| The combine query's time limit | `combine_timeout` | 10 minutes |
| Values an output may write | `max_size` | 1 GiB |
| A whole run's time limit | `run_timeout` | 30 minutes |
| Runs kept per report | `runs_kept` | 20 |

Each is a choice of a few values around its default, as the query history's settings are, and
saves at once; `settings.toml` takes any value in a report file's form (`"4 GiB"`, `"45m"`).
A report file's keys keep their names (`timeout` in `[[source]]`, `[combine]` and `[report]`);
the settings' names say which `timeout` they set.

The app sends the defaults to the worker in the run's request, beside the parameters' values,
so the worker reads no `settings.toml` of its own and a run uses what the tab showed. A report
as read keeps only the limits its file sets; planning fills the rest from the defaults. Under a
run stopped by a limit that has a default, the report's tab links to Settings › Reports beside
the file's key. The error doesn't say which of the two set the value: the run knows only the
value it ran with. (RP4h made it say: the plan keeps which limits the file sets.)

### Layers

Each layer is one pull request in a stack, useful and tested on its own, with synthetic data
only.

- **RP4a — Plan** (built, #426): this section, RP4's decisions and risks, and the roadmap's
  entry in layers.
- **RP4b — What a run measures** (built, #427): in `dbrover-driver-duckdb`, the run's DuckDB profiled and its
  peaks read after a query; in `dbrover-report`, `combine_done`'s `peak_memory` and
  `peak_temp_size`, and each output's time in `output_written`.
  **Exit:** in memory, a combine step over a small `memory_limit` reports a peak spill above
  zero and one with room reports none; a locked-down workspace still reports its peaks; each
  written file has its time; RP1's to RP3's events still read.
- **RP4c — Run history** (built, #429): in `dbrover-store`, the `report_runs` table, adding a run, reading a
  report's newest runs and deleting them, keeping the newest 20; in `dbrover-engine`, commands
  that record, list and clear a report's runs on the history's thread; in the app, each run
  recorded when it ends, finished, failed or cancelled.
  **Exit:** a run's row reads back with every step's numbers; the 21st run of a report deletes
  its oldest and leaves other reports' alone; a version 2 file migrates and keeps its
  statements; with a stand-in worker, a finished, a failed and a cancelled run are each
  recorded with their outcome.
- **RP4d — Runs side by side** (built, #430): the tab's **Runs** section, the step that dominates, the two
  hints and **Clear history**.
  **Exit:** table tests of the dominant step and of each hint's thresholds (a source just under
  and just over 10,000 rows and ten times the rows written, a combine with and without a
  spill); a kittest with a stand-in worker runs a report twice and sees two columns, the
  dominant step marked and the spill hint.
- **RP4e — Settings › Reports** (built, #431): `[reports]` in `settings.toml`, the page, the
  defaults applied when a report is planned and sent in the run's request, and a link to the page
  under a run stopped by a limit. `ByteSize` moves to `dbrover-core`, so the store reads sizes.
  **Exit:** a default sent in the request stops a run that the constant would have let through;
  a report's own key still wins; `settings.toml` without `[reports]` reads as before; the page's
  choices save, the next run's request carries them, and search finds them.
- **RP4f — Manual and demo note** (built): the **Reports** chapter's section on runs, the hints and the
  Settings page; RP4's demo note.

**Exit:** run a report twice from its tab: both runs are listed side by side with each step's
time, the step that takes most of it is marked, and a combine step that spilled says to raise
its memory limit or read fewer rows; the limits' defaults change in Settings › Reports.

### RP4's decisions

Taken on 2026-11-03 when RP4 started; each can be overturned in review.

| Question | Chosen | Consequence |
| --- | --- | --- |
| Where runs are kept | **A `report_runs` table in `history.sqlite`,** as the design said. | One file to sync, back up or delete, with the history's WAL and busy timeout; the history's retention settings don't touch runs, which have their own count. |
| Who writes a run | **The app, once the run ends.** | A worker that is killed, by **Cancel**, its time limit or the system, can't write; the app has every event it sent. RP6's command line will write its own runs the same way. |
| How a run's steps are stored | **JSON in one column,** the parameters in another. | A report's steps differ from one report, and one version of its file, to the next; a table per step kind would be joined for every read and migrated for every new metric. Nothing searches inside them. |
| How many runs are kept | **The newest 20 per report,** in Settings. | Enough to see a trend over months of monthly runs; a few kilobytes each. |
| How runs are told apart from the report | **By its file name,** as the explorer lists it. | Renaming a report's file outside DbRover starts its runs afresh (RP4i's **Rename…** moves them); editing the file keeps them, and a column per run shows the steps it had. |
| DuckDB's peak memory and spill | **DuckDB's profiler,** read after the combine query. | Exact peaks, not samples, at no cost to the run. They cover the whole run's DuckDB, loads included; the peak memory can exceed `memory_limit` a little, since DuckDB counts some memory the limit doesn't hold back. |
| What "spilled" means | **The most held on disk at once,** not the total written. | The number `max_temp_size` bounds, which is what the hint compares. DuckDB doesn't report the total. |
| An output's time | **The time in its writer,** overlapping its feed's. | Shows when a writer, Excel's say, is the slow part; it is never added to the run's total. |
| The step that dominates | **The longest share of the run's time,** marked in each run; named under the table when the finished runs listed agree. | Simple to read off the table. Concurrent sources may add up to more than the run. |
| A step's time in the table | **Its own:** the step that feeds the outputs without their time. | A slow writer shows as its output; without it, every report without a combine step would name its one source, whatever its writers cost. The cost: a writer's completing the file is taken off its feed too, which can understate the feed by that much. |
| Limits as read | **Only what the file sets,** the rest filled in when the report is planned. | Settings › Reports applies to every report without reading the files again, and to the worker through the request. |
| A source's hint | **At least 10,000 rows read, and ten times what the outputs write.** | Below 10,000 rows filtering in the query saves too little to mention; a lookup's batches count as one source. Aggregations trip it on purpose, so it is worded as a question to check. |
| Where the defaults are set | **Settings › Reports, sent in the run's request.** | The worker reads no settings file of its own, and a run uses the defaults the tab shows. RP6's command line will read `[reports]` itself. |
| The defaults' values | **Unchanged:** RP1's and RP2's constants. | The examples run well inside them (the general ledger's million rows sets its own `max_rows`), so nothing measured argues for moving them; Settings lets a machine with more memory raise them. |

### RP4's risks

- **The history file grows.** Twenty runs of a report with a dozen steps is tens of kilobytes;
  a hundred reports, a few megabytes. The count is in Settings.
- **The profiler's numbers move with DuckDB.** `SYSTEM_PEAK_BUFFER_MEMORY` and
  `SYSTEM_PEAK_TEMP_DIR_SIZE` are DuckDB's names, read as text; a DuckDB that drops them gives
  no peak, which the tab shows as unknown rather than zero.
- **Hints that are wrong for the report.** An aggregation reads far more than it writes on
  purpose; the hint says what to check rather than what is wrong, and `quiet_hints` (RP4g)
  silences it for such a report.
- **A synced settings folder.** H4's warning about a synced `history.sqlite` holds for runs: two
  machines running one report each keep their own runs.

## RP5 — PRQL, and the track finished

Started and built on 2026-11-04, after RP6. RP5 lets the combine step be written in
[PRQL](https://prql-lang.org), compiled to DuckDB's SQL before the run. The same stack finishes
what RP4 and RP6 left for later, as layers RP4g to RP4j and RP6g to RP6j, and RP7 brings the
manual up to date. The report file's other tables, the worker and its guardrails stay as RP1 to
RP6 built them.

Two later plans touch the same ground, and the choices below leave room for them:
[Reports at scale](reports-at-scale.md) (RS1's hints as rules with ids, RS1's live progress,
RS8's read-only MCP tools) and [Charts as code](charts-as-code.md) (`[[chart]]` outputs, not
built here).

### PRQL in `[combine]`

```toml
[combine]
prql = """
from lines
filter ordered_at >= $since && ordered_at < $until
derive net = amount - refunded
group {category} (aggregate {net = sum net})
sort category
"""
memory_limit = "1 GiB"
```

- **`prql`** takes the place of `query`: a `[combine]` has one of the two, and its limits' keys
  as before.
- **Compiled when the file is read,** by the app, `dbrover report run` and the worker alike,
  with `prqlc` for DuckDB's dialect. A `prql target:` line in the text doesn't change it: the
  combine step runs in DuckDB. The compiled SQL then goes through the SQL path's checks (one
  statement that only reads) and runs as a `query` would.
- **Parameters** are PRQL's own, `$since`. `prqlc` leaves them as `$since` in the SQL, which the
  compiler's side rewrites to the `:since` the run binds, with the lexer, so a `$` in a string is
  never one. Each must be in `[report] params`, as in SQL.
- **A PRQL that doesn't compile** is a problem of the file, as a TOML error is, and points into
  the PRQL: its line and column there, the line itself with a caret under the place, and
  `prqlc`'s hint when it gives one:

  ```text
  [combine]: prql, line 2, column 8: Unknown name `ordred_at`
      filter ordred_at >= $since && ordered_at < $until
             ^
  ```

- `[report] prql`, which RP1 kept for later, becomes a problem that says PRQL goes in
  `[combine]`.

### The compiled SQL, shown

The combine step's SQL isn't shown anywhere today but in the file. A PRQL report's compiled SQL
is shown where a person looks for what ran:

- **The report's tab:** under the parameters, **Combine SQL**, folded, with the SQL `prqlc`
  made, as `prqlc` lays it out and as it ran, and **Copy**.
- **The run's details:** the combine step's line folds out the same SQL, beside the loaded
  sources' columns. DuckDB's errors name a line of the SQL, so a combine step that fails shows
  it unfolded.
- **`dbrover report run`:** the `started` event carries `combine_sql` for a PRQL report, so
  `--json` has it; the human lines print it, its lines numbered, under a combine step's error,
  where DuckDB's line numbers point.

A SQL report shows none of these: its SQL is the file's.

### What `prqlc` costs

Measured on 2026-11-04 on an M-series Mac, `prqlc` 0.13.14 with its default features off (they
are its command line's):

| | Adds |
| --- | --- |
| Crates | 35 entries new to `Cargo.lock`, a few of them second versions of crates already there: `prqlc`, `prqlc-parser`, `chumsky`, `ariadne`, `sqlformat`, `schemars`, `vergen` (its build script's), a second `sqlparser` (0.62, beside the workspace's 0.63), `winnow` and `strum`, and small ones |
| A debug build (CI's clippy and tests) | about 11 s of processor time, built beside the rest, so less on the clock |
| A release build | about 40 s of processor time; the second `sqlparser` is 17 s of it |
| The release binary | 5.9 MB stripped, per architecture, against a program that already links `regex`, `serde_json`, `chrono`, `csv` and `sqlparser`: about 2.5% of DbRover's |

The second `sqlparser` goes when `prqlc` moves to the workspace's. `prqlc` is pinned to its
minor version (`~0.13.14`), as DuckDB is: PRQL's language still changes between minor versions,
and a report's compiled SQL shouldn't change under it with a `cargo update`.

### Finishing RP4

- **RP4g — Quiet hints.** Each hint gets an id, the one RS1's table of rules gives it:
  `source-reads-more` and `combine-spilled`. `[report] quiet_hints = ["source-reads-more"]`
  silences a hint for a report that reads more than it writes on purpose; an id that isn't one
  is a problem naming the ids there are. The ids live in `dbrover-report`, where RS1e's rules
  will, so RS1e adds rules to the same list rather than replacing it. The tab says how many
  hints a report silences, and each hint's tooltip names its id and how to silence it.
- **RP4h — Where a limit came from.** A run stopped by a limit says whether the report's file
  set it or Settings › Reports did: *It stopped at max_rows = 1,000,000 rows, the default in
  Settings › Reports. Raise it there, or set max_rows in the [[source]] "lines".* The plan keeps,
  for each limit, whether the file set it; `LimitHit` gains `set_in` (`file` or `settings`),
  absent in older events and for a limit of a format's own (an Excel sheet's rows). The tab
  links to Settings › Reports only for `settings`.
- **RP4i — Renaming a report.** **Rename…** on a report in the explorer renames its file in
  `reports/` and moves its runs in the history with it, in one engine command, and reopens its
  tab under the new name. A file renamed outside DbRover still starts its runs afresh, as the
  manual says: nothing in the file ties it to them.
- **RP4j — MCP clients running reports.** RS8 plans read-only tools for reports and leaves
  running them open; RP4j decides it. `dbrover mcp` gets two tools:
  - `list_reports`, read-only, for every project: the reports whose data sources are all in the
    project, with their parameters and problems. The others are invisible, as other projects'
    data sources are. RS8's tools (`explain_report`, `report_runs` and the others) join it.
  - `run_report`, behind a new project permission, **Run reports** (`reports` in `[mcp]` and
    `[mcp_defaults]`), off by default. It runs the report in the worker as `dbrover report run`
    does, with every guardrail, the files going to Downloads, where the app's runs go until a
    folder is chosen, and returns how it ended,
    each step's numbers, the files' paths and the preview's first rows, at most the project's
    `max_rows`. Sources connect read-only, as every report's do. The client's cancellation
    stops the worker.

  The server doesn't run reports itself: the app hands it a runner (`ReportRunner`) built on the
  command line's code, so the MCP crate needs nothing of the worker's.

### Finishing RP6

- **RP6g — A run's origin.** `report_runs` gains `origin` (`app`, `command`, `mcp`; none in an
  older row, which reads as the app's), schema 3 to 4, a nullable column as W4's rule says. The
  Runs table says *from the command line* or *from MCP* under a run's start.
- **RP6h — `dbrover report list`.** Each report in `reports/`: its name for `report run`, its
  title, its parameters with their types, and its problems, against the settings folder's data
  sources. `--json` for one object a report; `--config-dir` as `run`'s. Exits with 0, or 3 when
  the folder can't be read.
- **RP6i — A live progress line.** `dbrover report run --progress` keeps a line on standard
  error that is rewritten each second while a step runs (*Source lines: 120,000 rows, 3.1 MB,
  4 s*), only when standard error is a terminal; with a pipe, cron's mail or `--json` it prints
  nothing more. The finished steps' lines are as before. RS1d adds throughput and what a step
  waits on to the same line.
- **RP6j — A worker that dies with its command.** On Linux the worker asks the kernel for
  SIGKILL when the process that started it dies (`PR_SET_PDEATHSIG`), so a command killed
  outright takes its worker; a worker whose parent is already gone by then exits at once. The
  signal is sent when the starting *thread* ends, so the worker is started from a thread that
  lives as long as the run: the command's main thread, and the app's own. On every system, a
  worker whose events can't be written any more (its reader is gone) stops its run and removes
  its staging folder, which also covers macOS and Windows within a second of its next event.

### RP7 — Manual

The **Reports** chapter covers RP1 to RP6. RP7 adds PRQL (a section, the `[combine]` table's
`prql`, and an example, `sales-by-week.toml` on the shop, with its checked-in CSV file), quiet
hints, where a limit came from, **Rename…**, `report list`, `--progress`, a picture of **Runs**
(a screenshot scene with two runs in the history), and the MCP tools in the MCP chapter, and
removes "What's next".

### Layers

Each layer is one pull request in a stack, useful and tested on its own, with synthetic data
only.

- **RP5a — Plan** (built, #469): this section, its decisions and risks, and the roadmap's entries.
- **RP5b — PRQL** (built, #470): in `dbrover-report`, `prql` read, compiled, its parameters rewritten, its
  errors pointing into it; the plan and the run use the compiled SQL.
  **Exit:** a PRQL combine and its SQL twin write the same files; a PRQL error names its line
  and column; `$since` is bound and a `$` in a string isn't; `prql` beside `query` is a problem.
- **RP5c — The compiled SQL, shown** (built, #471): the tab's **Combine SQL**, the combine line's fold-out,
  `combine_sql` in `started`, and the command's SQL under a combine error.
  **Exit:** kittest finds the compiled SQL in a PRQL report's tab and none in a SQL report's;
  the command prints the numbered SQL under a combine error and `--json` carries it.
- **RP4g — Quiet hints** (built, #472): ids, `quiet_hints` read and checked, the tab's hints filtered.
  **Exit:** a report that silences `source-reads-more` shows no such hint where an identical one
  without it does; an unknown id is a problem listing the ids.
- **RP4h — Where a limit came from** (built, #473): `set_in` from the plan to the error, the tab and the
  command.
  **Exit:** a run stopped at a file's `max_rows` and one stopped at Settings' say which; older
  events still read.
- **RP4i — Renaming a report** (built, #474): **Rename…**, the file renamed and its runs moved.
  **Exit:** a report with runs renamed in the app lists them under its new name; a name in use,
  or one that isn't a file name, is refused.
- **RP6g — A run's origin** (built, #475): the column, the record and the Runs table.
  **Exit:** a version 3 history migrates; runs from the app and the command read back with
  their origin; kittest shows *from the command line*.
- **RP6h — `dbrover report list`** (built, #476).
  **Exit:** on the real binary, a folder with a good report and one with problems lists both,
  in lines and in JSON.
- **RP6i — `--progress`** (built, #477).
  **Exit:** table tests of the line's text; on a pipe, `--progress` prints nothing more.
- **RP6j — The worker dies with its command** (built, #478).
  **Exit:** on Linux, the real binary SIGKILLed during a slow source leaves no worker; on every
  system, a worker whose events' reader closes stops and removes its staging folder.
- **RP4j — MCP clients running reports** (built, #479): `list_reports`, `run_report`, the permission, and the
  app's runner.
  **Exit:** through the MCP test client, a project without the permission lists reports and is
  refused a run; with it, the shop's report runs and its files exist; a report on another
  project's data source isn't listed.
- **RP7 — Manual and demo notes** (built, #480): the chapter and the MCP chapter as above, the example, the
  picture, and RP4's to RP7's roadmap entries marked built with their demo notes.

**Exit:** a report whose combine step is PRQL runs from the tab, the command line and an MCP
client, its compiled SQL shown in each; its runs say where they came from, survive a rename, and
can silence a hint.

### RP5's decisions

Taken on 2026-11-04 when RP5 started; each can be overturned in review.

| Question | Chosen | Consequence |
| --- | --- | --- |
| Where PRQL compiles | **When the file is read,** everywhere it is read. | A PRQL error is a file problem the explorer marks, before any run; the worker compiles again, with the same `prqlc`, so it runs what the tab showed. |
| Parameters | **PRQL's `$name`,** rewritten to `:name` after compiling. | Reads as PRQL; the run binds them as it binds SQL's. |
| A `prql target:` line | **Ignored:** always DuckDB's dialect. | A file copied from elsewhere runs in the only dialect the combine step has. |
| Where the compiled SQL shows | **The tab, the run's combine line, the command's error and `started`.** | Visible where a person needs it; a SQL report's tab doesn't repeat its file. |
| `prqlc`'s version | **Pinned to 0.13,** default features off. | A minor version of PRQL can change compiled SQL; moving it is a change of its own. |
| Hint ids | **RS1's,** in `dbrover-report`. | RS1e adds rules to the list RP4g starts. |
| Where a limit came from | **The file or Settings,** kept by the plan, carried in `LimitHit`. | A built-in default reads as Settings', since Settings › Reports shows it. |
| Renamed reports | **Rename… in the app** moves the runs; renaming outside it doesn't. | No id in the file to keep in step; the manual says which keeps runs. |
| MCP running reports | **Yes, behind a project permission off by default,** files in Downloads. | An assistant runs what the person wrote, never a report of its own; it can't choose where files go. |
| Origin of a run | **A nullable column,** schema 4. | Older DbRovers keep writing runs, which read as the app's. |
| The progress line | **`--progress`, on a terminal only.** | Cron's mail stays short; RS1d extends the same flag. |
| A command killed outright | **`PR_SET_PDEATHSIG` on Linux; a closed pipe stops the worker everywhere.** | No worker outlives its command on Linux; elsewhere it stops at its next event. |

### RP5's risks

- **`prqlc`'s SQL is DuckDB's but not always the shortest.** PRQL makes CTEs (`table_0`) that
  DuckDB plans away; the compiled SQL shown is what ran, so a slow step can still be read.
- **PRQL changes.** 0.13 is not 1.0; a later minor version may refuse a file 0.13 compiled. The
  pin keeps it still until the move is tested on the examples.
- **The parent-death signal follows a thread.** A worker started from a thread that ends early
  would be killed with it; both callers start it from a thread that lives as long as the run,
  which a comment at the call says.
- **An MCP client running a large report** runs under the report's own limits, which the person
  set; the permission is off until they turn it on.
- **An older DbRover and `reports = true`.** `projects.toml`'s MCP settings refuse unknown keys,
  so a DbRover from before RP4j can't read a file with the permission set; the MCP chapter says
  to switch it off before going back.
- **A killed command's files on Linux.** The parent-death signal kills the worker outright, so
  the staging folder of a command killed with SIGKILL stays in the output folder, hidden; on
  macOS and Windows the worker removes it once it finds nobody reading its events.

## RP6 — The command line

Started and built on 2026-11-03, after RP4. RP6 runs a report without the app's window, so cron, a
scheduled task or a CI job can run it with new values and act on how it ended:

```sh
dbrover report run sales-by-category --param since=2026-10-01 --param until=2026-11-01 \
  --output-dir /srv/reports/2026-10
```

The report file, the worker and its guardrails stay as RP1 to RP4 built them. What is new is
the command around the worker: finding the report and what it names with no window to ask in,
printing what happens, an exit code per outcome, stopping on a signal, and keeping the run in
the history so the app shows it.

### The command

```text
dbrover report run <report> [--param <name>=<value>]… [--output-dir <folder>] [--json]
                            [--config-dir <folder>]
```

- **`<report>`** is a report's name in the settings folder's `reports/`, as the explorer lists
  it (`sales-by-category`), or the path to a report file anywhere (`./sales-by-category.toml`,
  `examples/reports/sales-by-category.toml`). A name ending in `.toml` or holding a folder
  separator is a path; anything else is a name.
- **`--param <name>=<value>`**, once per parameter, in the form the report's tab takes (a date
  as `2026-10-01`, a timestamp as `2026-10-01 12:00`). Everything after the first `=` is the
  value. A parameter given twice, or without `=`, is a mistake in the command line; one
  missing, of the wrong type, or one the report doesn't take is the report's. Either stops the
  command before anything runs.
- **`--output-dir <folder>`** (`-o`): where the files go, made if it doesn't exist. Without it,
  the current folder, as other command-line tools write theirs; a cron job sets it.
- **`--json`**: the worker's events on standard output instead of the human lines (below).
- **`--config-dir <folder>`**, or `DBROVER_CONFIG_DIR`: the settings folder to read, as for the
  app and `dbrover mcp`. Without either, the one the app uses, including a folder chosen in
  Settings › Files.

Arguments it doesn't know are refused, as `dbrover mcp`'s are: a cron line is typed by hand, and
a typo should fail loudly. `dbrover report run --help` prints the usage.

### What it reads

From the settings folder, as the worker does: the report (by name), `data_sources.toml`, whose
data sources the report names by name or id, `exports.toml`'s presets, and `settings.toml`'s
`[reports]`, the limits' defaults, which the app sends in the request and the command reads
itself. Projects play no part: reports are in one folder for every project (RP1's decisions).

Passwords come as the worker gets them, and nobody is asked:

- **Saved in the keychain:** macOS's keychain, Windows' Credential Manager, or a Secret Service
  on Linux. A cron job on macOS reads the login keychain while that user is logged in; a
  headless Linux server usually has no Secret Service, and needs a password command.
- **A password command** that the data folder's `trusted_commands.toml` trusts, as the app
  records it the first time the command runs. On a machine without the app, the manual shows
  the file's entry to write by hand.
- **A cloud sign-in** already on the machine: AWS's credentials and profiles, Google's
  application default credentials, or `az`'s sign-in. DbRover never opens a browser for one;
  a sign-in that has expired fails the run with the provider's message, and signing in again is
  done with the cloud's own command (`aws sso login`, `gcloud auth application-default login`,
  `az login`).

Every source connects read-only, as in the app's runs (W11's mode, the statement checked to only
read, the cost guard refusing what it would ask about).

### Running it

The command reads and plans the report itself, with the settings folder's data sources and
`[reports]`, and checks the values: a problem there exits with 3 before a process starts or a
data source is opened. It then starts `dbrover report-worker` from its own executable, as the
app does, through the same `WorkerRun`: one request on the worker's standard input, events back
as JSON Lines. So a run from the command line has every guardrail a run from the app has: the
sources' caps and timeouts, DuckDB's limits, the outputs' sizes, a lower priority, Linux's
`RLIMIT_DATA` and `RLIMIT_CPU`, and the run's time limit, past which the worker is killed and its
staging folder removed. Files are written in a staging folder inside the output folder and put
in place only when every one is complete; a run that fails or is stopped leaves the folder as
it was.

**Ctrl-C**, **SIGTERM** (what cron, systemd, `timeout` and CI runners send to stop a job) and
**SIGHUP** (a terminal closing), or Ctrl-Break on Windows, cancel the run: the worker is killed,
its staging folder removed, and the command exits with 6. A signal that comes before the worker
starts cancels it as soon as it does. On Unix the worker gets a process group of its own, so a
terminal's Ctrl-C, which goes to the whole foreground group, reaches only the command, which
then cancels the run; in the command's group the worker would die of the same SIGINT, and the
run would end as a worker that stopped without saying why or as cancelled, whichever the
command saw first.

The app's "one run at a time" holds within one app. The command doesn't wait for the app or for
another command: each run has its own staging folder, and the history takes writes from several
processes (W4's busy timeout).

### What it prints

By default, for a person reading a terminal or cron's mail:

- **Standard error:** a line as the run starts, a line as each source and the combine step
  finish (rows, bytes, time), each warning, and the end: the total time, or the error with the
  limit to raise, as the report's tab says it.
- **Standard output:** each file written, a line each, as its full path, so a script can pipe
  them on (`| xargs -I{} cp {} …`).

```text
$ dbrover report run sales-by-category --param since=2026-10-01 --param until=2026-11-01
Sales by category (since = 2026-10-01, until = 2026-11-01)
Source lines: 246 rows, 6.5 KB in 0.0 s
Combine: 13 rows, 509 bytes in 0.0 s
/home/me/sales-by-category-2026-10.xlsx
/home/me/sales-by-category-2026-10.csv
Finished in 0.0 s.
```

Progress lines aren't printed once a second, to keep a cron mail short; RP6i's `--progress`
keeps a live line on a terminal.

With **`--json`**, standard output is the worker's events, a line each, as the worker wrote them
(`started`, `progress`, `source_done`, `combine_done`, `preview`, `output_written`, then
`finished`, `failed` or `cancelled`), the same types the app reads, and standard error is empty
unless the command line is wrong or the history can't be written. A run that can't start is one
`failed` event in the report step, as the worker would say it. A CI step can read them with
`jq`.

The command's own log, and the worker's, go to `logs/dbrover-report.YYYY-MM-DD.log` in the data
folder, beside the app's and `dbrover mcp`'s, never to the terminal.

### Exit codes

| Code | Outcome | The run ends with |
| --- | --- | --- |
| 0 | The report ran and wrote its files. | `finished` |
| 1 | It failed otherwise: the combine query's error, an output that couldn't be written, the worker stopping without saying why. | `failed`, in the combine step, an output or the worker |
| 2 | The command line is wrong: an unknown option, no report named, `--param` without `=` or given twice. | — |
| 3 | The run couldn't start: the report file isn't found or has problems, a data source it names isn't found, a parameter is missing, unknown or of the wrong type, or the settings folder can't be used. | `failed` in the report step |
| 4 | A source failed: connecting to its data source (a password, the network, a sign-in) or its query (the database's error, a statement that doesn't only read, the cost guard). | `failed` in a source, no limit |
| 5 | A guardrail stopped a step: `max_rows`, `max_bytes`, a source's or the combine step's `timeout`, `memory_limit`, `max_temp_size`, `max_size`, Excel's rows. | `failed` with the limit, in a step |
| 6 | The run was stopped from outside: its own time limit (`[report] timeout`), Ctrl-C, SIGTERM or SIGHUP. | `failed` in the worker with the limit, or `cancelled` |

A step's limit (5) is told from the run's (6) by where the run ended: the worker stops itself at
a step's limit and says which, while the run's time limit is enforced by killing the worker.

### Run history

A run of a report in the settings folder's `reports/` is kept in `history.sqlite` as the app
keeps its runs: the same row, with the same steps, the newest `runs_kept` of the report kept. The
report's tab shows it among the others when it next lists the runs. A report file elsewhere (in
a repository's checkout, run by CI) isn't kept: the app couldn't list it, and a run under a
report of the same name in `reports/` would mix two files' runs. A run that couldn't start isn't
kept either, as in the app. A history that can't be written is a warning on standard error; the
run's exit code stays its own.

### Windows

`dbrover.exe` is built as a console program, which is why `dbrover --version` prints in the
release workflow's smoke test and `dbrover mcp` serves over standard input and output. So the
command prints, and returns its exit code, in PowerShell, `cmd.exe`, Task Scheduler and CI, as on
macOS and Linux, with nothing added. The worker it starts has no console of its own
(`CREATE_NO_WINDOW`), so a Ctrl-C or Ctrl-Break reaches only the command, which stops the
worker.

### Layers

Each layer is one pull request in a stack, useful and tested on its own, with synthetic data
only.

- **RP6a — Plan** (built, #437): this section, RP6's decisions and risks, and the roadmap's
  entry in layers.
- **RP6b — Running a report** (built, #438): `dbrover report run`: its arguments, finding the report, the
  settings folder's data sources, presets and `[reports]`, the values checked, the run in the
  worker through `WorkerRun`, the human lines and the exit codes.
  **Exit:** table tests of the arguments and of the exit code for each way a run ends; on the
  real binary with a SQLite shop made from `examples/reports/shop.sql`, `sales-by-category`
  writes both files into `--output-dir`, its CSV file matching the checked-in one, and prints
  their paths; a missing or unknown parameter and an unknown data source exit with 3, a source's
  error with 4, `max_rows` with 5 and the run's time limit with 6, each leaving no file.
- **RP6c — Stopping** (built, #440): Ctrl-C, SIGTERM and SIGHUP cancel the run, the worker in a
  process group of its own on Unix.
  **Exit:** on Unix, the real binary sent SIGINT to its process group, as a terminal sends it,
  SIGTERM or SIGHUP during a slow source exits with 6 and leaves neither a file nor a staging
  folder.
- **RP6d — JSON** (built, #441): `--json`.
  **Exit:** the lines on standard output read back as the worker's events, ending with
  `finished`, or `failed` with the same exit code as without `--json`.
- **RP6e — Run history** (built, #442): the command keeps a run of a report in `reports/` as the app does.
  **Exit:** a run by name, a failing run and a cancelled one are each read back from
  `history.sqlite` with their outcome and steps; a file outside `reports/` adds nothing.
- **RP6f — Manual and demo note** (built): a section of the **Reports** chapter on the command line,
  with a cron line and a CI step that run the examples, and RP6's demo note.

**Exit:** from a terminal, `dbrover report run` writes a report's files with its guardrails, and
its exit code says how it ended; the app's tab lists the run beside its own.

### RP6's decisions

Taken on 2026-11-03 when RP6 started; each can be overturned in review.

| Question | Chosen | Consequence |
| --- | --- | --- |
| A command of its own, or the app's binary | **`dbrover report run`,** a subcommand of the one binary, as `dbrover mcp`. | Nothing more to install or package, and the worker that runs is the binary's own. `report` is the word for its subcommands, so `dbrover report list` can come later. |
| In the worker, or in the command's own process | **In the worker,** through the app's `WorkerRun`. | One way to run a report: the guardrails, the killing at the time limit and the staging folder's removal are the app's, tested once. A second process per run is the cost. |
| Naming the report | **A name in `reports/`, or a path,** told apart by `.toml` or a folder separator. | The app's reports run by the names the explorer shows; a repository's report runs from its checkout, with a throwaway settings folder for its data sources. |
| Checking before the worker | **The command reads and plans the report,** as the tab does. | A wrong parameter or data source exits with 3 without starting a process; the command knows the run's time limit for the watchdog. The worker reads the file again, so it runs what is saved. |
| Unknown parameters | **Refused,** as the command line's typos are. | `--param sinse=…` fails rather than running a report whose `since` is missing for another reason. |
| The limits' defaults | **`settings.toml`'s `[reports]`,** read by the command and sent in the request, as the app sends them. | A run from cron and one from the tab run under the same limits. |
| Where files go | **`--output-dir`, else the current folder.** | As other command-line tools; the app's Downloads default is a desktop's. |
| Passwords and sign-ins | **The worker's: saved, a trusted password command, a cloud sign-in on the machine;** never a prompt or a browser. | A data source whose password is typed each session can't run from cron; a headless Linux server needs a password command, trusted in `trusted_commands.toml`. |
| What it prints | **Steps on standard error, files' paths on standard output;** `--json` for the worker's events. | Cron mails a few lines; a script gets the paths; CI gets events it can parse, of the types the app reads, with no format of the command's own to keep. |
| Exit codes | **0 to 6, one per outcome** (the table above). | A cron wrapper or a CI step tells "raise a limit" (5) from "the database is down" (4) from "fix the file" (3). Not `timeout(1)`'s 124 or the shell's 130, which name one cause each, where 6 covers every stop from outside. |
| Signals | **Ctrl-C, SIGTERM and SIGHUP cancel,** as **Cancel** does. | The output folder is left as it was, and the command exits with 6. |
| The worker's process group | **One of its own on Unix,** so a terminal's Ctrl-C reaches only the command. | The command alone decides how the run ends, so the exit code doesn't depend on which process saw the signal first. Killing the command's process group no longer reaches the worker; a signal to the command does. |
| Run history | **Kept for reports in `reports/`,** the app's rows; nothing marks a run as the command's. | The tab shows cron's runs with no schema change. RP6g marks them, in a column of their own. |
| Logs | **The data folder's `logs/dbrover-report.*.log`.** | The terminal shows only the run; a failure's details are where the app's are. |
| Windows | **Nothing added:** `dbrover.exe` is a console program. | `--version`, `mcp` and `report run` print and return codes there as elsewhere. If the app is ever built as a windowed program, the command line needs a console attached first. |

### RP6's risks

- **A command killed outright** (SIGKILL, a CI runner's hard stop) can't kill its worker,
  which runs on until it ends or reaches Linux's `RLIMIT_CPU`; its staging folder stays in the
  output folder. (RP6j: the worker now dies with it on Linux, and stops at its next event
  elsewhere.) The worker is in a process group of its own, so killing the command's group
  doesn't reach it either; SIGTERM to the command stops both, and the worker's group can be
  killed after the command's.
- **Keychains away from a desktop.** A Linux server without a Secret Service, or a macOS user
  logged out, can't read saved passwords; the error names the data source, and the manual points
  at password commands.
- **Two writers of one output folder.** Two runs of a report with the same values, from cron
  and the app, write the same files; the one that finishes last wins, each file whole.
- **A report outside `reports/`** is never in the history, so its runs can't be compared in the
  app; CI keeps its own logs.

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

- ~~Should MCP clients be able to run a report?~~ Decided in RP4j: yes, behind a project
  permission that is off by default ([Finishing RP4](#finishing-rp4)).
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
