# Reports

A report is a query you run again and again with new values, such as a trial balance for each
month, and the files it writes each time, with the columns, names, types and number formats
you chose once. It lives in a file of its own, runs in a process of its own, and stops at
limits you can see, so a report that reads too much never takes DbRover or your machine with
it.

A report reads one data source, or several, combined in DuckDB SQL, and writes CSV and Excel
files. A source can read by the keys an earlier one found, and can be on a document database
such as DynamoDB or MongoDB, by declaring the columns it makes.

## Your first report

The examples in the repository's
[`examples/reports`](https://github.com/navasvarela/dbrover/tree/main/examples/reports) folder
run on a made-up ledger:

1. Add a SQLite data source named **ledger**, with a new file (see
   [Data sources](data-sources.md)).
2. Open [`ledger.sql`](https://github.com/navasvarela/dbrover/blob/main/examples/reports/ledger.sql)
   in its console (**File → Open…**) and run it all. It makes a chart of accounts and three
   months of journal entries.
3. Right-click **Reports** at the bottom of the explorer and choose **Open the reports folder**.
   Copy `trial-balance.toml` into it, then choose **Reload**.
4. Click **Trial balance**. Its tab opens.
5. Type `2026-10-01` for **since** and `2026-11-01` for **until**, and click **Run**.

The tab shows the run as it goes, then the two files it wrote, `trial-balance-2026-10.xlsx` and
`trial-balance-2026-10.csv`, in your Downloads folder. Click one to open it: each account's
debits, credits and balance, a subtotal for each kind of account, and a total whose debits and
credits are both 309,957.53. Under the files, a preview shows the rows the report wrote.

### The four examples

The examples are the shapes of reports that accounting software makes, on made-up data. Each
shows something of its own:

| Report | Reads | Shows |
| --- | --- | --- |
| [`trial-balance.toml`](https://github.com/navasvarela/dbrover/blob/main/examples/reports/trial-balance.toml) | SQLite | Subtotals with `GROUPING SETS`, and amounts rounded to the cent as they load. |
| [`general-ledger.toml`](https://github.com/navasvarela/dbrover/blob/main/examples/reports/general-ledger.toml) | SQLite and MongoDB | Each entry's counterparty, read from MongoDB by the ids the entries name, and a running balance per account with a window function. |
| [`valuation.toml`](https://github.com/navasvarela/dbrover/blob/main/examples/reports/valuation.toml) | PostgreSQL, DynamoDB and DuckDB | Each transaction valued at its asset's price in the hour it ran, with every digit. |
| [`cost-basis.toml`](https://github.com/navasvarela/dbrover/blob/main/examples/reports/cost-basis.toml) | PostgreSQL, DynamoDB and DuckDB | Each disposal matched against the lots it used up, first in, first out, with its cost, proceeds and gain. |

Their data comes from scripts beside them, each run in the console of the data source the
report names:

- **ledger** (SQLite): [`ledger.sql`](https://github.com/navasvarela/dbrover/blob/main/examples/reports/ledger.sql),
  as above.
- **crm** (MongoDB, on a database you can write to):
  [`ledger-counterparties.js`](https://github.com/navasvarela/dbrover/blob/main/examples/reports/ledger-counterparties.js),
  the ledger's customers, suppliers and vendors.
- **ledger-postgres** (PostgreSQL, on a server you can write a schema to):
  [`valuation-postgres.sql`](https://github.com/navasvarela/dbrover/blob/main/examples/reports/valuation-postgres.sql),
  300 made-up trades of BTC, ETH and SOL in the schema `valuation`, each amount in the asset's
  smallest unit.
- **assets-dynamodb** (DynamoDB):
  [`valuation-assets.partiql`](https://github.com/navasvarela/dbrover/blob/main/examples/reports/valuation-assets.partiql),
  each asset's symbol, unit and chain. PartiQL can't make a table, so make `valuation_assets`
  first, keyed by `asset_id`, as the script's first lines say.
- **prices-duckdb** (DuckDB, with a new file):
  [`valuation-prices.sql`](https://github.com/navasvarela/dbrover/blob/main/examples/reports/valuation-prices.sql),
  three months of hourly prices. Then edit the data source and tick **Read-only**, so a report
  can read the file while DbRover has it open (see [Several sources](#several-sources)).

Copy the reports into the reports folder, **Reload**, and run each for October 2026: `2026-10-01`
and `2026-11-01` for the ledger's two, `2026-10-01 00:00` and `2026-11-01 00:00` for the other
two. The valuation's tab shows **Source tx** loading, **Source assets** *waiting for tx*, then
reading the three assets the month's transactions use, **Source prices** attached, then
**Combine**. Its CSV file keeps every digit of each value, 26 decimal places, while the Excel
file rounds each to the cent.

## The Reports section

**Reports**, under the data sources in the explorer, lists every report in the `reports` folder
by its name. One with a warning sign can't run: hover over it to see how many problems it has,
and click it to see them. Right-click **Reports** for:

- **New report**: writes a commented report to start from, reading the first data source, and
  opens it.
- **Open the reports folder**: shows the folder in your file manager, to add or edit reports
  with any editor.
- **Reload**: reads every report again, after you have changed one.

The folder is `reports` in your settings folder (see [Where DbRover keeps its files](files.md)),
one for all your projects, so a report can read data sources from any of them.

## A report's tab

Click a report to open its tab:

- its name, what it is for, and its file, with **Edit file** to open it in your editor and
  **Reload** to read it again;
- what is wrong with it, if anything, in which case it can't run until you fix the file;
- a field for each parameter, checked as you type: a date that isn't one says so beside it;
- **Save to**: the folder its files go in, Downloads until you **Choose…** another; and the
  names of the files the values make;
- **Run**, and while it runs, **Cancel**.

While a report runs, the tab shows a line for each source and, if it has one, for the combine
step: what each has read so far, and for how long, with a check once it is done. A source loaded
into DuckDB folds out its columns and the types they were given. Only one report runs at a
time; the other tabs say which one. When it ends, the tab shows each file it wrote
with its rows and size, and the preview; or the step that failed and why. Nothing is written
unless every file is: a run that fails or is cancelled leaves the folder as it was, an older
file of the same name included.

The values and the folder are remembered for each report until you quit DbRover.

## A report file

A report is a TOML file, with one `[report]`, one or more `[[source]]`s, a `[combine]` when
there are several, and one or more `[[output]]`s. The simplest reads one source; this trial
balance, without the example's subtotals, does:

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
format = "csv"
file = "trial-balance-{since:%Y-%m}.csv"
columns = "same"
```

### `[report]`

| Key | What it is |
| --- | --- |
| `name` | What the explorer and the tab call it. Needed. |
| `description` | A sentence under the name. |
| `params` | Each parameter and its type, as `{ since = "date", until = "date" }`. |
| `timeout` | How long a whole run may take, as `"30m"` or `"1h"`. 30 minutes unless you say. |

A parameter's type is `text`, `integer`, `decimal`, `boolean`, `date` or `timestamp`. A date is
typed as `2026-10-01`, a timestamp as `2026-10-01 12:00` (seconds and a fraction are optional,
and `T` works in place of the space).

### `[[source]]`

| Key | What it is |
| --- | --- |
| `id` | What the run's progress and errors call it, and the name of its table in `[combine]`: letters, digits and `_`, starting with a letter. Needed. |
| `data_source` | The data source it reads, by its name in the explorer. When two share a name, use the data source's id instead (in `data_sources.toml`). Needed. |
| `query` | One statement that only reads, in the data source's own SQL. Needed, unless `attach = true`. |
| `attach` | `true` for a DuckDB file that `[combine]` reads in place, without a query. See [Several sources](#several-sources). |
| `lookup` | Reads by the values an earlier source found, as `{ from = "tx", column = "asset_id", as = "asset_ids" }`. See [Lookups](#lookups). |
| `columns` | The columns it makes, each with a type. Needed for a document database. See [Declared columns](#declared-columns). |
| `max_rows` | The most rows it may return, all its lookup's batches together. 1,000,000 unless you say. |
| `max_bytes` | The most data it may return, as `"500 MB"` or `"4 GiB"`. 1 GiB unless you say. |
| `timeout` | How long the query may run, all its batches together, as `"30s"` or `"10m"`. 10 minutes unless you say. |

The query is in the data source's own language: SQL, PartiQL for DynamoDB, the shell's
`db.collection.find(…)` for MongoDB, a request for Elasticsearch and OpenSearch, a command for
Redis. It uses parameters as a console does, as `:since` (see
[Files, parameters and find](files-parameters.md#parameters)), and each `:name` must be in
`params`. In SQL, the values are bound as a console binds them; in the other languages they
are written into the query as that language writes a value of their type: a date in MongoDB's
shell is `ISODate("2026-10-01T00:00:00Z")`. A report reads every engine DbRover connects to.

### `[combine]`

A report with several sources combines them with a query in DuckDB's SQL, whose rows the
outputs write:

| Key | What it is |
| --- | --- |
| `query` | One statement that only reads, over the sources' tables, with the report's `:name` parameters. Needed. |
| `timeout` | How long the query may run. 10 minutes unless you say. |
| `memory_limit` | The memory DuckDB may use before it spills to disk, as `"2 GiB"`. 2 GiB unless you say. |
| `threads` | How many of the processor's cores DuckDB may use. Half of them unless you say. |
| `max_temp_size` | The most DuckDB may spill to disk, as `"10 GiB"`. 10 GiB unless you say. |

See [Several sources](#several-sources).

### `[[output]]`

| Key | What it is |
| --- | --- |
| `format` | `"xlsx"` or `"csv"`, with that format's options beside it, as `exports.toml` names them: `sheet_name`, `header`, `freeze_header` and `autofilter` for Excel; `separator`, `quoting`, `header`, `null_text`, `line_ending` and `bom` for CSV. |
| `preset` | Instead of `format`: a CSV or Excel preset of yours in `exports.toml`, by its id, with its options (see [Your own export formats](export-formats.md)). |
| `file` | The file's name. `{since}` puts a parameter's value in it, and `{since:%Y-%m}` formats a date or timestamp parameter. |
| `columns` | The columns it writes, below. Without it, every column of the query, as it comes; with `"same"`, the first output's. |
| `max_size` | The most data it may write, as `"1 GiB"`. 1 GiB unless you say. |
| `continue_on_new_sheet` | For Excel: past a sheet's 1,048,576 rows, go on in a new sheet rather than stop. |

A file name can't hold a folder: every file goes in the folder you choose in the tab. Two
outputs can't write the same file.

### Columns

Each column is `{ name, from, type, format, time_zone, if_invalid }`, of which only `name` is
needed:

- `name`: its header.
- `from`: the query's column it comes from, the same as `name` unless you say.
- `type`: what its values are, which DbRover checks and writes in the type's plain form.
- `format`: Excel's number format, as `"#,##0.00"` or `"yyyy-mm-dd hh:mm"`. CSV files ignore it:
  they are read by programs, which want each type's plain text.
- `time_zone`: for a timestamp column, the zone to convert values with an offset to, as
  `"UTC"` or `"Europe/Madrid"`.
- `if_invalid`: what to do with a value that doesn't fit its type: `"fail"` the run (unless you
  say), write nothing (`"null"`), or write the value as it came (`"text"`).

| `type` | Takes | CSV writes | Excel writes |
| --- | --- | --- | --- |
| `text` | anything | the value | text |
| `integer` | a whole number | its digits | a number |
| `decimal` | a decimal number | its digits, as they came | a number |
| `decimal(18,2)` | a decimal number, rounded to 2 places, at most 18 digits in all | exactly 2 places | a number |
| `boolean` | `t` or `f`, `true` or `false`, `yes` or `no`, `1` or `0` | `true` or `false` | a boolean |
| `date` | `2026-10-01` | `2026-10-01` | a date |
| `timestamp` | a date and time | `2026-10-01 12:00:00`, with its fraction | a date and time, to the millisecond |

Decimals are rounded on their digits, half away from zero (`2.345` to `2.35`, `-2.345` to
`-2.35`), as your database and Excel's `ROUND` do, never through a floating-point number: an
18-decimal token amount keeps every digit. Excel's numbers hold 15 digits, so a cell with more
is written as text, and the run says so for its column: give the column a type that rounds,
such as `decimal(38,2)`, to keep its cells numbers.

A value that doesn't fit stops the run with the output, the column, the row and the value, as
`row 2, column "Debit": "n/a" isn't a decimal number`.

## Several sources

The valuation reads a PostgreSQL table and a DuckDB file, and joins them:

```toml
[[source]]
id = "tx"
data_source = "ledger-postgres"
query = """
SELECT id, wallet, asset, quantity, executed_at
FROM valuation.transactions
WHERE executed_at >= :since AND executed_at < :until
"""

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
```

Each run has a DuckDB database of its own, in memory. Every source with a query runs through
DbRover's own connection to its data source, read-only and under its limits, as a single source
does, and its rows are loaded into a table named after its `id`: `tx` above. Sources load at
the same time. A DuckDB file with `attach = true` isn't copied: it is attached, read-only, as a
database named after its `id`, so `prices.hourly_prices` reads its table in place and DuckDB
reads only what the query needs. The `[combine]` query then runs over them, with all of
DuckDB's SQL: joins of every kind, `ASOF JOIN` for "the latest price at or before",
window functions, `GROUP BY GROUPING SETS` for subtotals, `PIVOT`.

The combine query reads nothing else: no file on your disk, no extension from the internet, no
setting of DuckDB's. A report with one source and no `[combine]` runs without DuckDB, as in the
trial balance.

A DuckDB file can be read by one program while another has it open for writing only if neither
writes. Tick **Read-only** on a DuckDB data source that a report attaches, or disconnect it
before you run the report; otherwise the run says the file is locked.

### Types in DuckDB

Each loaded column gets a type, shown in the tab when you fold out the source:

- **The data source's type,** where it says more than the values: a `decimal(18,2)` stays
  `DECIMAL(18,2)`, a float stays `DOUBLE`, and a text column stays text, so codes such as `0042`
  or `1000` keep their form. Text whose every value is a date or a time, as SQLite keeps them,
  becomes a `DATE` or a `TIMESTAMP`.
- **Otherwise, the values':** PostgreSQL, for one, doesn't say. Whole numbers become `BIGINT`
  (`HUGEINT` past 18 digits); decimal numbers a `DECIMAL` with just the digits they have, so
  PostgreSQL's `numeric` amounts load exactly; `t` and `f` a `BOOLEAN`; dates and times their
  types, with a time zone if they carry an offset. Anything else, or a mix, is text.

A number of more than 38 digits, which no DuckDB `DECIMAL` holds, loads as text, and the tab
warns about its column; cast it in the combine query to what it should be.

### Exact numbers in DuckDB

DuckDB keeps decimals exact through `+`, `-`, `*`, `SUM`, `ROUND` and comparisons: a product's
places are the sum of its factors', up to 38 digits in all, which is why the valuation's
values have 26. Two things give a floating-point `DOUBLE` instead, which keeps about 15
digits:

- **Division:** `a / b` is a `DOUBLE`. Multiply by a decimal instead (`amount * 0.01` rather
  than `amount / 100`), or cast the result: `CAST(a / b AS DECIMAL(38,18))`.
- **`power()`**, `sqrt()`, `ln()` and the like.

The outputs then round each value once, as their column's type says: the valuation's Excel file
writes values as `decimal(38,2)`, and its CSV file every digit.

## Lookups

A source can read by the values an earlier source found, rather than reading everything and
leaving the combine step to throw most of it away. The valuation reads, from DynamoDB, only the
assets the month's transactions use:

```toml
[[source]]
id = "assets"
data_source = "assets-dynamodb"
lookup = { from = "tx", column = "asset_id", as = "asset_ids" }
query = 'SELECT asset_id, "symbol", unit, metadata FROM valuation_assets WHERE asset_id IN :asset_ids'
```

`lookup` takes the distinct values of the column `asset_id` of the source `tx`, once `tx` has
loaded, and puts them where the query says `:asset_ids`, as a list in the query's language:

| Language | The query reads | `:ids` becomes |
| --- | --- | --- |
| SQL | `WHERE id IN :ids` | `('a', 'b', 7)` |
| PartiQL (DynamoDB) | `WHERE pk IN :ids` | `['a', 'b', 7]` |
| MongoDB | `db.assets.find({_id: {$in: :ids}})` | `["a", "b", 7]` |
| Elasticsearch, OpenSearch | `{"terms": {"id": :ids}}` | `["a", "b", 7]` |
| Redis | `MGET :ids` | `a b 7` |

A value from a number column goes in as a number, anything else as text: a key kept as text in
one database and as a number in the other matches nothing, so declare the earlier column's type
(see below) when it matters.

- **`from`** names a source above this one in the file, with a query: an attached DuckDB file is
  joined in `[combine]` instead.
- **`as`** is the parameter's name, which can't be one of `params`. The query must use it.
- **`batch_size`** lowers how many values one statement takes. A lookup with more values than
  that runs its query again for each batch, and every batch's rows go into the one table. A
  statement takes at most 100 values in PartiQL (DynamoDB refuses more) and in CQL, and 1,000
  in the other languages; the tab shows which batch a source is on.

Sources with a lookup run once the source they read is loaded: in rounds, each round's sources
at the same time. When the earlier source found no value, a source with declared columns sends
nothing and its table is empty; a SQL source without them runs its query once with `(NULL)`,
which matches nothing.

On DynamoDB, a lookup on the table's partition key reads each item by its key. One on any other
attribute would scan the table, which the data source's cost guard refuses (see
[Cost guards](cloud.md#cost-guards)). PartiQL keeps some words for itself, `symbol` and `value`
among them: quote an attribute with such a name, as `"symbol"`.

## Declared columns

DynamoDB, MongoDB, Elasticsearch and Redis return documents rather than rows, so a source on one
declares the columns it makes:

```toml
columns = [
  { name = "asset_id", type = "text" },
  { name = "symbol",   type = "text" },
  { name = "unit",     type = "decimal(38,18)" },
  { name = "chain",    from = "metadata.chain", type = "text", default = "unknown" },
]
```

Each column is `{ name, from, type, default }`, of which only `name` is needed:

- `name`: the column in the source's table, which `[combine]` reads.
- `from`: where its value is, the same as `name` unless you say: a field of a document by its
  path, as `metadata.chain` or `lines[0].sku`, or for a source that returns rows, the query's
  column.
- `type`: one of the output types (see [Columns](#columns)), `text` unless you say. Its type in
  DuckDB follows: `integer` is a `BIGINT`, `decimal(38,18)` a `DECIMAL(38,18)`, `timestamp` a
  `TIMESTAMP`.
- `default`: the value a missing field, or `null`, gets.

The table has the declared columns and nothing else: fields you don't declare are left out.
MongoDB's typed values are read as their value: `NumberLong(7)` is `7`, `NumberDecimal("0.1")`
is `0.1`, an `ObjectId` its hex digits, and a date its time in UTC. A time with an offset is
converted to UTC; an object or an array goes into a `text` column as its JSON.

A value that doesn't fit its type stops the run with the source, the item and the value, as
`item 2 (_id "cp-7"): column "orders": "many" isn't a whole number`.

A source on a SQL database may declare its columns too, to fix their types rather than let
DuckDB's typing guess from the values: the examples declare SQLite's amounts `decimal(18,2)`, so
each is rounded to the cent as it loads, and PostgreSQL's raw amounts `decimal(38,0)`.

## Limits

A report runs in a process of its own, `dbrover report-worker`, which DbRover starts for each
run. If it runs out of memory or time, or you cancel it, the process stops and DbRover, its
consoles and their transactions carry on. The limits, cheapest first:

| What | Limit | Change it with |
| --- | --- | --- |
| Each source | its rows, its data, its time, every batch of its lookup together; read-only | `max_rows`, `max_bytes`, `timeout` in `[[source]]` |
| The combine step | DuckDB's memory, threads and spilled data; its time | `memory_limit`, `threads`, `max_temp_size`, `timeout` in `[combine]` |
| Each output | its data; Excel's rows a sheet | `max_size`, `continue_on_new_sheet` in `[[output]]` |
| The run | its time | `timeout` in `[report]` |
| The process | a lower priority; on Linux, half the machine's memory and the run's time on each processor thread | — |

DuckDB spills what doesn't fit in its `memory_limit` to a folder inside the output folder, which
goes with the run. On macOS and Windows, which don't cap a process's memory, `memory_limit` is
what keeps a large combine step from filling your machine's memory: lower it on a small
machine, raise it for a report that combines millions of rows.

A run stopped by a limit names it, its value and where to raise it, as "It stopped at max_rows =
1,000,000 rows. To allow more, raise max_rows in the `[[source]]` "balances" of the report's
file." Before raising one, ask whether the query should read less: a report that returns far
more rows than it writes is better filtered in its query.

The source always reads: a statement that would change anything is refused before it is sent,
and it runs in the database's read-only mode, as a read-only data source's consoles do. A data
source whose cost guard would ask before a statement refuses it instead, since a running report
can't ask you (see [Cost guards](cloud.md#cost-guards)).

The worker can't ask for a password either. A data source whose password you type each time
can't be a report's source until you save the password, or use a password command or a cloud
sign-in (see [Password from a command](data-sources.md#password-from-a-command)).

## What's next

Reports are being built in steps. Coming next: a record of every run to compare them, the PRQL
language for the combine step, and running a report from the command line.
