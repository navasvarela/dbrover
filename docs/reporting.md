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

A report is a TOML file in `reports/` in the settings folder, read the same way as
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
sheet = "Valuations"
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
- **CSV:** delimiter, quoting, header row and line ending, as W7's options.
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
  output schemas, and the Reports tab. A report with one source writes CSV and Excel.
- **RP2 — Several sources:** sources loaded into DuckDB, the combine step, attached DuckDB
  sources, exact decimals end to end.
- **RP3 — Lookups and documents:** lookups with batching, declared columns for document
  sources. The first iteration's exit: the four accounting reports above run and match.
- **RP4 — Metrics:** the run history, the comparison of runs, and the hints.
- **RP5 — PRQL:** `prql` in `[combine]`, with the compiled SQL shown.
- **RP6 — The command line:** `dbrover report run <file> --param since=…`, for cron and CI,
  with the worker's guardrails and an exit code per outcome.
- **RP7 — Manual,** with the examples.

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

- Reports per project, or one `reports/` folder? Projects already hold data sources and MCP
  settings; a report names data sources, so per project may fit better.
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
