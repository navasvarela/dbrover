# BigQuery

DbRover connects to Google BigQuery over its REST API, signed in with the Google Cloud sign-in
already on your machine. BigQuery bills a query by the bytes it scans, whatever it returns, so
DbRover asks BigQuery what a statement will scan before it runs it, shows you, and keeps to the
limit you set.

## Signing in

A BigQuery data source keeps no password and no Google secret. Add one with **New data source ›
Google Cloud › BigQuery**.

| Field | Default | Notes |
| --- | --- | --- |
| Sign in with | Google Cloud | **Google Cloud**: Application Default Credentials, as `gcloud auth application-default login` writes them, or the file `GOOGLE_APPLICATION_CREDENTIALS` names ([Cloud sign-in](cloud.md#google-cloud)). **Credentials file**: a service account's key file of this data source's own. **Emulator**: no sign-in, for the BigQuery emulator. |
| Project | required | The project queries run in and are billed to. It is listed first in the explorer. |
| Dataset | none | The dataset unqualified table names resolve in. |
| Location | BigQuery's choice | Where queries run (`US`, `EU`, `europe-west2`, …). Empty: BigQuery picks the location of the tables a query reads. |
| Other projects | none | Projects to show in the explorer besides yours, separated by commas, such as `bigquery-public-data`, whose public datasets Google's own listing never shows. |
| Ask above | 1 GB | In GB scanned; see [What a query costs](#what-a-query-costs). Empty: no limit. |

Each field takes `${NAME}` placeholders. In `data_sources.toml` the project is written
`project_id`, as Google names it, since `project` is the data source's own key.

**Test connection** lists one dataset of the project, which needs the sign-in and the project
to work: a project the account may not read is an error there, with Google's reason.

## The explorer

The explorer lists your project, then the **Other projects**, then every other project your
account can see; under each, its datasets with their location; under a dataset, its tables,
views, materialized views, external tables and snapshots, a table's partitioning beside it
(*partitioned by day on placed*); and under a table its columns, with their GoogleSQL types. A
record's fields follow it as `address.city`, and the columns a table is partitioned or clustered
on are marked *partitioning* and *clustering 1*. Each level loads a thousand at a time, and the
filter box filters what has loaded.

All of it comes from BigQuery's listing calls, which are free: browsing never runs a query.

- **Open data** opens a console with ``SELECT * FROM `my-project.sales.orders` LIMIT 1000`` and
  runs it. On a table, DbRover reads those rows with BigQuery's free table preview rather than
  running the query, which BigQuery would bill for every column of the table, `LIMIT` or not: the
  result line says `0 bytes read`. The rows are the table's first as BigQuery stores them. Change
  the statement in any way (a `WHERE`, an `ORDER BY`, the columns) and it is a query again,
  billed as one. On a view it is always a query.
- **Show definition** shows a view's query as its `CREATE VIEW`, and writes a table's `CREATE
  TABLE` from its columns, partitioning and clustering, both from the table's own description,
  for free. A table's options and descriptions are left out.

## The console

A console runs GoogleSQL. Each statement runs as a BigQuery job in the data source's project,
whose id starts `dbrover_`, so you can find DbRover's jobs in the project's job history. The
status line shows what it has processed while it runs, and each finished statement says so
beside its rows and time: `500 rows · 3.2 s · 1.4 GB read`. Rows come back 500 at a time;
**Cancel** (⌘.) cancels the job in BigQuery.

As you type, each statement is checked with a **dry run**: BigQuery reads it, finds its tables
and columns, and says what it would scan, without running it, for free. A misspelt name or a
syntax error is underlined where BigQuery points, and the toolbar shows what the statement at
the cursor will scan: **Will scan 1.4 GB**.

Values come as BigQuery shows them: arrays and records as JSON (`["a","b"]`,
`{"city":"Paris"}`), a `TIMESTAMP` in UTC (`2026-01-02 03:04:05.123456 UTC`), `BYTES` in base64,
and `NUMERIC` with every digit.

Each statement is a job of its own, so a console keeps no session: `DECLARE`, `CREATE TEMP
TABLE` and `BEGIN TRANSACTION` work only inside one statement that is a whole script. Rows aren't
edited in the grid: BigQuery's tables have no key that tells their rows apart. There is no
**Explain**; a job's plan is in BigQuery's console.

## What a query costs

On-demand, BigQuery bills the bytes a query scans, about $6.25 a TiB, at least 10 MiB for each
table it reads, with the first TiB of each month free. A `LIMIT` doesn't lower it. Before each
statement runs, DbRover asks BigQuery's dry run what it will scan:

- Above **Ask above**, the console asks first (**Scan 1.4 GB?**). **Scan anyway** runs it.
  **Run to file**, exports and AI assistants refuse it instead, saying why.
- Every query DbRover sends carries the limit as BigQuery's *maximum bytes billed*, so a query
  that would bill more than the dry run said (BigQuery's 10 MiB minimum per table can do that)
  is refused by BigQuery before it runs, unbilled, and the statement says so. A statement you let
  through with **Scan anyway** goes without it: you have seen what it scans.
- Where a dry run can say only how much a query scans at least (external tables), the question
  says *at least*. Under row-level security BigQuery's dry run always says 0 bytes.
- With no limit, DbRover still shows each estimate, and asks nothing.

The limit is kept in `data_sources.toml` as `ask_above_bytes`. A project on capacity
(slot) pricing isn't billed by bytes; the limit still holds its queries to what you set.

## Safety and AI assistants

BigQuery has no read-only mode. A **read-only** data source refuses statements that change data
or tables before they are sent; for Google to refuse them, sign in with a service account that
has only the **BigQuery Data Viewer** and **BigQuery Job User** roles. An AI assistant is served
a BigQuery data source through its **MCP credentials file**: another service account's key,
with only those roles, and it is held to the data source's limit.

## The emulator

The community BigQuery emulator (`ghcr.io/goccy/bigquery-emulator`) runs on your machine with
data from a YAML file: choose **Emulator** and its address (`http://localhost:9050`), and the
project it was started with. It browses and queries like BigQuery, but it runs a dry run's query
for real, ignores the byte limit, finishes every query at once and words its errors its own way,
so what this page says about costs can't be tried on it.

## Not tried yet

DbRover's BigQuery support was built and tested against BigQuery's documented API and the
emulator, without a Google Cloud project. These are still to be tried against BigQuery itself:

- signing in with Application Default Credentials and with a service account's key;
- a dry run's estimate, as you type and before a query runs, and the question above the limit;
- a query refused for its *maximum bytes billed*, and its message;
- the bytes processed while a job runs, cancel, and an `UPDATE`'s count of rows;
- error messages and the place they point at;
- **Open data**'s free preview on a large table, and on a partitioned one;
- queries in a single-region location (`europe-west2`) and on public datasets.
