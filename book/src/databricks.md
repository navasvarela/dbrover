# Databricks

DbRover connects to Databricks SQL over its Statement Execution API, signed in with a personal
access token, a service principal, the Databricks CLI or the Azure CLI. Databricks bills the
time a SQL warehouse runs, so DbRover browses Unity Catalog without starting your warehouse,
and stops a statement that runs past the limit you set. Reports read Databricks's results as
Arrow.

## Signing in

Add a data source with **New data source › Other cloud services › Databricks**.

| Field | Default | Notes |
| --- | --- | --- |
| Host | required | The workspace's address, as its browser tab shows it: `dbc-a1b2c3d4-e5f6.cloud.databricks.com` on AWS, `adb-1234567890123456.7.azuredatabricks.net` on Azure, `1234567890123456.7.gcp.databricks.com` on Google Cloud. |
| Sign in with | Access token | **Access token**: a personal access token, yours or a service principal's. **Service principal**: OAuth for machines, with the principal's **Client ID** and secret. **Databricks CLI**: the sign-in `databricks auth login --host <workspace>` made, which opens your browser once; DbRover runs `databricks auth token` for a token on every connection. **Azure CLI**: on Azure Databricks, the sign-in `az login` made, in its default directory or the **Tenant** you name. |
| Token / Client secret | none | Kept in the keychain. The CLIs' sign-ins need none. |
| SQL warehouse | required | The warehouse every statement runs on: its ID, or the HTTP path its **Connection details** show (`/sql/1.0/warehouses/…`), which DbRover saves as the ID. |
| Catalog, Schema | the workspace's defaults | What each statement starts in, until a console's `USE` changes it (see [The console](#the-console)). |
| Stop statements after | 10 | In minutes; see [What a statement costs](#what-a-statement-costs). Empty: Databricks's own limit. |
| MCP client ID, MCP client secret | none | The service principal [AI assistants](mcp.md) are served through; see [Safety and AI assistants](#safety-and-ai-assistants). |

Each field takes `${NAME}` placeholders. DbRover never reads `~/.databrickscfg`, nor the CLIs'
token caches: it asks the CLI for a token, as Databricks's and Azure's own tools do.

**Test connection** names the warehouse, its size, whether it is serverless, whether it is
running, and any statement timeout set on it: *warehouse Shared (Small, serverless, stopped;
stops statements after 1 h)*. It reads them from the SQL warehouses' API, which doesn't start a
stopped warehouse.

## The explorer

The explorer lists the catalogs you can see (*shared*, *foreign* and *system* ones marked; the
`system` catalog only when **System** is ticked); under each, its schemas
(`information_schema` only when **System** is ticked); under a schema, three folders,
**tables**, **views** and **volumes**; and under a table or view, its columns with their types.
Tables are marked by format (*Delta*, *Iceberg*, *Parquet*) and kind (*external*, *foreign*,
*streaming*, *shallow clone*); views include metric views and materialized views; a volume
shows its bucket's address, or *managed*. Each level loads a page at a time, and the filter box
asks Databricks too.

All of it comes from Unity Catalog's API, which is no statement: browsing never starts a
stopped warehouse. So do **Show definition** and **Found on the server**:

- **Show definition** shows a view's, materialized view's or streaming table's text, and writes
  a table's `CREATE TABLE` from its columns (types, `NOT NULL`, comments), its format, its
  partition columns and its location. Its properties are left out; `SHOW CREATE TABLE` gives
  them in a console.
- **Found on the server** searches tables and views across every catalog.
- **Open data** opens a console with ``SELECT * FROM main.sales.orders LIMIT 1000`` and runs it.
  This one needs the warehouse: a stopped one starts (in seconds for a serverless warehouse,
  minutes for a pro or classic one) and is billed until it stops again.

## The console

A console runs Databricks SQL. Each statement is sent to the warehouse with the catalog and
schema, tagged `dbrover` so you can find DbRover's statements in Databricks's query history.
Rows come a chunk at a time, each downloaded from where Databricks put it; **Cancel** (⌘.)
cancels the statement in Databricks.

Databricks's API keeps no session between statements, so DbRover keeps each console's context
itself:

- `USE CATALOG`, `SET CATALOG`, `USE SCHEMA`, `USE DATABASE` and `USE schema` change what that
  console's later statements run in, and only that console's. DbRover answers them without
  asking Databricks, so a name that doesn't exist fails on the next statement, with Databricks's
  error.
- `SET` of a setting or a variable, `RESET`, `DECLARE`, temporary views and functions, and
  transactions are refused before they are sent, saying why: each would answer as if it had
  worked and be lost by the next statement. Set a setting on the warehouse instead.

Values come as Databricks writes them. Rows aren't edited in the grid: Databricks's keys aren't
enforced, so nothing tells a table's rows apart. Statements aren't checked with Databricks as
you type, only by DbRover's own syntax check: an `EXPLAIN` is a statement, which would start a
stopped warehouse.

## What a statement costs

A SQL warehouse is billed by the time it runs, in DBUs an hour by its size, and stops itself
after its idle time. Nothing says beforehand how long a statement will run, so DbRover asks
nothing before it runs. What it does instead:

- **Stop statements after**: Databricks's API takes no time limit, so DbRover cancels a
  statement still running past it, within about two seconds, and the statement says which limit
  stopped it. The warehouse is billed until then. A new data source starts at 10 minutes; the
  warehouse's own statement timeout, if your administrator set one, still applies. Clear it, and
  only Databricks's own limit applies (two days, unless your administrator set less).
- Everything DbRover runs by itself reads Unity Catalog's or the warehouses' API, except **Open
  data** and explaining a report, which run on the warehouse.

The limit is kept in `data_sources.toml` as `stop_after_seconds`.

## Reports

A report reads a Databricks source's rows as Arrow, not text: Databricks writes the result as
Arrow files in its own storage, and the run downloads several at once and loads them into
DuckDB with their types (numbers, decimals, dates and timestamps); arrays, maps, structs and
`VARIANT` arrive as text. This is Databricks's own way of handing a large result over, so a
source's `handoff` isn't used on Databricks, and is refused when the report is planned.

**Explain report** explains a Databricks source with `EXPLAIN COST`, and shows the size of the
tables its plan reads as its estimate, *at most*: Delta skips files a filter rules out. This runs
on the warehouse, and starts it if it is stopped.

## Safety and AI assistants

Databricks has no read-only session. A **read-only** data source refuses statements that change
data or objects before they are sent. For Databricks itself to refuse them, sign in as a
service principal granted only `SELECT` on the tables and views, `USE CATALOG` and `USE SCHEMA`
on their catalogs and schemas, and `CAN USE` on the warehouse. An AI assistant is served a
Databricks data source through such a principal: its **MCP client ID** and **MCP client
secret**, which sign the assistant's connection in with OAuth for machines.

## Not tried yet

DbRover's Databricks support was built and tested against Databricks's documented APIs, without
a workspace (there is no emulator of its own). These are still to be tried against Databricks
itself:

- signing in with a personal access token, a service principal, the Databricks CLI and the Azure
  CLI;
- a query over several chunks, and how each type's values are written;
- a statement stopped by **Stop statements after**, and its message;
- `USE CATALOG` and `USE SCHEMA` in a console;
- error messages and the place they point at, and cancel;
- the explorer on a large workspace, `hive_metastore`, and **Show definition**;
- a report reading Arrow chunks, and Explain report's estimate.
