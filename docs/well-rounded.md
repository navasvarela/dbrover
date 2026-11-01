# A well-rounded app

Status: **W1 to W7 built** (see the [roadmap](roadmap.md#well-rounded-track); W5's
design is in [Power in the grid](grid-power.md), W7's in
[Exports and your own formats](exports.md)); W8 planned, designed there too. W10, the visual
explain plan, designed on 2026-10-31 [below](#w10--a-visual-explain-plan) and built. W11,
read-only on every engine, designed on 2026-10-31 [below](#w11--read-only-on-every-engine) and
built. W12, charts from results, designed on 2026-11-01 [below](#w12--charts-from-results) and
built but for its export, which waits for ER1's.
Proposed on 2026-10-24.

DbRover browses, queries and edits PostgreSQL, SQLite, MySQL and MariaDB, and ships as a
download. This plan gathers the gaps that still keep it from being the database tool someone
uses every day without missing another one. Many come from follow-ups that earlier milestones
left open in the [roadmap](roadmap.md).

The work is in eight milestones, W1 to W8. Each is useful on its own.

## Order and why

1. **W1 — Portable configuration.** Asked for first. It needs P0's projects, from the
   [MCP plan](mcp-server.md), so the export can carry them.
2. **W2 — Safety per data source.** It turns the colour that marks production into rules, and
   reuses the read-only enforcement MCP1 builds.
3. **W3 — Connections you can rely on.** SSH tunnels make reachable the many databases that
   only accept connections through a jump host. Noticing dropped connections fixes the
   "connected" status that can be wrong.
4. **W4 — History as an audit trail.** It extends the query history every statement already
   goes to, rather than adding a second log.
5. **W5 — Power in the grid.** Inserting and deleting rows, following foreign keys, sorting and
   filtering in the database, CSV import, and exporting every row.
6. **W6 — Files and parameters.** Opening `.sql` files from disk, and `:name` parameters.
7. **W7 — Exports.** Added on 2026-10-29 and built on 2026-10-30: getting rows out in the formats
   people use, from everywhere rows are. It comes before ER diagrams.
8. **W8 — Your own export formats.** Formats of the user's own in `exports.toml`, once W7's
   formats have settled.

Signed installs, which remove macOS's "could not verify" warning and the repeated Keychain
prompts, are already planned as R2 in [Release binaries](releasing.md).

## W1 — Portable configuration

`data_sources.toml` already holds every data source without secrets. This milestone makes it
portable, so a configuration can be recreated on another machine or shared with a team.

- **File › Export data sources…:**
  - pick data sources or whole projects;
  - writes one TOML file in the same format as `data_sources.toml`, with a format version, the
    chosen projects and their MCP settings;
  - never a password, a token or a command action (see [Hackability](hackability.md)).
- **File › Import data sources…:**
  - **A preview first.** Each entry shows as **new**, **unchanged**, or **differs** (with what
    differs), and you choose **update**, **skip** or **import as a copy** for each.
  - **Same id, same data source.** The stable ids already in the file match entries up, so
    importing twice never duplicates them. A copy gets a new id.
  - **Projects** are matched by id, then by slug.
- **Paths that travel:**
  - **On export,** SQLite files and TLS certificate files are written as `~/…` when under the
    home folder, or relative to the exported file when beside it.
  - **On import,** they are resolved again, and a path that doesn't exist on this machine is
    shown in the preview.
- **`${VARIABLE}` placeholders** in host, port, database, user and paths, resolved from the
  environment when connecting. One shared file can then serve a team whose machines differ.
  - The form shows the placeholder and, beside it, what it currently resolves to.
  - An unset variable stops the connection with a clear error.
- **A password prompt on first connect.** A data source that needs a password and has none saved
  asks for it, with **Save to keychain** ticked. This closes the M1 follow-up "prompting for a
  password when none is saved". It also covers a keychain that is locked or unavailable.

**Exit:**
- I export two projects on one Mac, import them on another, enter each password once, and
  connect.
- Importing the same file again changes nothing.
- A data source using `${PGHOST}` connects wherever that variable is set.

## W2 — Safety per data source

- **A safety level per data source,** next to its colour in the dialog:
  - **Normal:** today's behaviour. `UPDATE` or `DELETE` without `WHERE` asks first.
  - **Careful:** every change asks first (DML, DDL, and grid edits), naming the data source. New
    consoles start in manual commit mode.
  - **Read-only:** changes are refused. The statement check from the MCP plan classifies each
    statement, and the console runs in the database's read-only mode (`BEGIN READ ONLY`,
    `START TRANSACTION READ ONLY`, or a read-only SQLite connection), exactly as MCP1 does.
- **The data source's colour** may suggest a level (red suggests Careful), but the level is its
  own setting. Colour stays a look.
- **The console toolbar** shows the level, and a read-only console says why a statement was
  refused.
- This closes the M1/M3 follow-up "per-data-source safety levels".

**Exit:**
- A read-only data source refuses an `INSERT`, a `CREATE TABLE`, and a write hidden in a
  function.
- A careful one asks before a grid edit, naming the data source.

## W3 — Connections you can rely on

- **SSH tunnels**, set per data source in a new **SSH** section of the shared server form:
  - **Settings:** host, port and user.
  - **Login:** with the SSH agent, a key file (with a passphrase stored in the keychain), or a
    password stored in the keychain.
  - **Built on [`russh`](https://crates.io/crates/russh)** (pure Rust, so nothing to install),
    as a local port forward the driver connects through.
  - **Host keys** are checked against `~/.ssh/known_hosts`. An unknown key asks once, showing
    its fingerprint. A changed key is refused.
  - **One tunnel per data source,** shared by its sessions, and closed with the last of them.
  - **Tested** against an SSH server container in `docker-compose.yml` that forwards to the
    PostgreSQL service.
- **Noticing dropped connections:**
  - **How it's noticed.** A session that has been idle for a while pings before its next
    statement. An error that means the connection is gone (each driver says which errors do)
    marks the session **Disconnected**, with the reason. The explorer's dot and tooltip stop
    claiming "connected".
  - **What happens next.** The explorer reconnects on its next request. A console reconnects
    only when asked, with **Reconnect** in its toolbar, because its transaction, temporary
    tables and session settings are gone and it says so.
  - This closes the M1/M2 follow-up "detecting dropped connections".

**Exit:**
- I connect to the test PostgreSQL through the SSH container with a key, and an unknown host key
  asks first.
- Restarting the database server shows the console as disconnected, and **Reconnect** works.

## W4 — History as an audit trail

The design, layer by layer, is in [History as an audit trail](history-audit.md).

The query history already records every statement:
- the data source, time and SQL;
- the duration, the rows, and the outcome.

MCP1 adds where each came from. This milestone makes it answer "what changed, when, and from
where" without a second log:

- **Where each statement came from:** `source` becomes one of console, grid edit, object helper
  ([E0–E4](entity-helpers.md)), action ([H1](hackability.md)) or MCP. **History** can filter by
  it.
- **Grid edits with their before values:** each `UPDATE` the grid submits is stored with the row's
  key and the values it replaced. The history window can then show, and copy, the statement that
  would put them back. This doesn't undo by itself: putting values back is a statement you review
  and run.
- **Retention:** keep the history for N days, or the last N entries per data source, set in View
  › Settings. Old entries are pruned at start-up. This closes the M4 follow-up.
- **Searching every data source at once,** and **exporting** the history (or a search) as CSV or
  JSON.
- **Sensitive statements** stay out, as the object helpers' plan requires for passwords.

A tamper-proof audit belongs on the server (for example `pgaudit` on PostgreSQL), not in a
client. The manual says so.

**Exit:**
- I edit two cells, and the history shows them as grid edits with their old values, and the
  statement to put them back.
- A search across data sources finds a statement from MCP.
- Entries older than the retention go at the next start.

## W5 — Power in the grid

The design, layer by layer, is in [Power in the grid](grid-power.md).

- **Inserting and deleting rows** in a table's data, next to today's cell editing:
  - **Add row** opens an empty row with each column's default shown;
  - **Delete** marks rows;
  - **Submit** runs the `INSERT`s and `DELETE`s with the `UPDATE`s, all or none, as edits do now.
  - Deleting needs the same primary key editing does.
- **Following foreign keys:**
  - a value in a foreign key column is a link that opens the referenced row;
  - **Referencing rows ›** on a row lists the tables that point at it, and opens the matching
    rows.
- **Sorting and filtering in the database.** A **Sort and filter in the database** switch in the
  grid re-runs the query with the sort and a `WHERE` built from the filter, wrapped as a
  subquery. The sort and filter then cover every row, not only those loaded. Today's
  client-side sort stays the default, because it needs no new query. This closes the M4
  follow-up.
- **Exporting every row.** Export offers **All rows**, which re-runs the query and streams rows to
  the file without holding them in memory. This closes the M4 follow-up.
- **Importing a CSV into a table:**
  - map the file's columns to the table's, with types checked against a preview;
  - insert in batches in one transaction, and roll back on the first failure, which is shown
    with its line number.

**Exit:**
- I add, edit and delete rows in one submit.
- I follow an order's `customer_id` to the customer, and back to their orders.
- I sort 100,000 rows in the database, export them all, and import a CSV of 10,000 rows into a
  table.

## W6 — Files and parameters

- **Opening `.sql` files from disk:**
  - **File › Open…**, recent files, and dropping a file on the window;
  - the file opens in a console bound to a data source you pick (remembered per file);
  - **Save** writes back to the file;
  - a file changed on disk while open asks whether to reload.

  Console files in the config folder keep working as now.
- **Query parameters.** `:name`, `?` and `$1` (whichever the dialect uses) in a statement open a
  small form before it runs, with each value's last use remembered. Values are sent as bound
  parameters where the driver supports them, never pasted into the SQL.
- **Find and replace** in the editor, with regular expressions.

**Exit:**
- I open a `.sql` file from a project folder, run it against a data source, edit and save it.
- A statement with `:since` asks for the date and remembers it for next time.

## W7 — Exports

The design, layer by layer, is in [Exports and your own formats](exports.md).

- **Formats:** CSV with options (separator, quoting, header, `NULL` text, line ending, a BOM)
  and a **CSV for Excel** preset, TSV, Excel `.xlsx` with typed cells, Markdown, SQL `INSERT`
  and `UPDATE`, JSON, JSON Lines, HTML and Parquet (through DuckDB).
- **Everywhere rows are:** an export dialog with a preview from the grid, **Copy as ›** a format
  and **Copy with headers**, **Export data…** on a table in the explorer, **Run to file…** without
  loading the grid, and a `format` for MCP's `query` tool.
- **Any size:** every file export streams.

**Exit:**
- A PostgreSQL query's rows open in Excel as numbers and dates, with a zip code's leading zero
  kept.
- A selection pastes into a pull request as a Markdown table.
- A ten-million-row table exports to Parquet without DbRover's memory growing.

## W8 — Your own export formats

- `exports.toml` beside `actions.toml`: named presets of the built-in formats, templates (a
  header, a row and a footer, in MiniJinja) and programs that get the rows as JSON Lines on
  stdin, trusted as H2's commands are.
- No grouping, totals or charts: those are SQL's job, or a program's.

**Exit:** a preset, a template and a program each appear in the export dialog and **Copy as ›**.

## W10 — A visual explain plan

Today **Explain** (⌥⌘E) puts the database's plan in the grid as text: one line per row on
PostgreSQL, one XML value on SQL Server. Reading it means knowing each engine's format and
doing arithmetic on nested costs. W10 draws the plan as a tree of steps instead, the same way
on every engine that has a structured plan, and points at the steps that cost the most.

- **The plan of the statement at the cursor, as a tree** in the results panel, in place of the
  grid:
  - each step shows what it does (`Hash Join`, `Seq Scan on orders o`), its estimated cost and
    rows, and how much of the whole plan it accounts for, as a bar;
  - its other facts (the join condition, the filter, the sort key, the index) show when the step
    is hovered or selected;
  - steps fold and unfold, as the explorer's nodes do.
- **The costliest steps highlighted.** A step's own cost is its cost minus its children's,
  since each engine reports a step's cost with everything under it. The step whose own cost is
  the largest share of the plan is marked **Costliest**, and any other step with at least a
  quarter of it is tinted too. Once the plan is analysed, time decides instead of the estimate.
- **Explain analyse** (⌥⇧⌘E, and a button above the plan) runs the statement and shows, beside
  each step's estimate, the rows it actually returned, how many times it ran, and how long it
  took. A step whose actual rows are ten times more or fewer than estimated is flagged, since a
  wrong estimate is the usual reason for a bad plan.
- **The text plan stays one click away:** **Tree** and **Text** switch the view. The text is
  written from the same plan, laid out as PostgreSQL's `EXPLAIN` lays it out (a line per step,
  indented under its parent, with its facts below it), so it reads the same on every engine and
  switching never runs the statement again.
- **Both can be copied:** **Copy text** copies the text plan, and **Copy JSON** (or **Copy XML**
  on SQL Server) copies the plan as the database returned it, for tools that read that format.
- **Engines:**
  - **PostgreSQL** first, from `EXPLAIN (FORMAT JSON)` and `EXPLAIN (ANALYZE, FORMAT JSON)`.
  - **MySQL 8.3 and later**, from `EXPLAIN FORMAT=JSON` with `explain_json_format_version = 2`,
    whose steps are the iterators MySQL runs, with estimated cost and rows and, under
    `EXPLAIN ANALYZE`, actual rows, loops and time. The setting is a session variable: it is
    switched on before the statement and put back to what it was after, the way SQL Server's
    `SHOWPLAN_XML` is switched today. MySQL 8.0 and MariaDB answer only the older format
    (`query_block`, nested loops of tables), which is read too, with MariaDB's
    `ANALYZE FORMAT=JSON` for actuals.
  - **SQL Server**, from the XML showplan: `SET SHOWPLAN_XML` for the estimated plan and
    `SET STATISTICS XML` for the actual one, which SQL Server sends as a further result after
    the statement's rows, so the driver keeps that result rather than the rows while the
    setting is on.
  - SQLite, DuckDB, ClickHouse and MongoDB keep today's plan in the grid: their plans are text
    or documents without costs to compare.
- **Safety (W2):** Explain analyse runs the statement, so it runs it as **Run** would: in the
  console's transaction mode, so a manual-commit console keeps a change open to roll back, and
  through the same checks. On a **careful** data source it always asks first, saying that
  Explain analyse runs the statement, even for a `SELECT`, which is easy to forget. On a
  **read-only** one, a change is refused before it is sent and the rest runs inside the
  read-only transaction, so the database refuses a write hidden in a function.

### How it's built

- **One plan type,** in a new crate, `dbrover-plan`, with no database and no UI: a `Plan` is a
  tree of `Step`s, each with an operation, what it reads, an estimate (cost, rows), an actual
  (rows, loops, time) once analysed, and the rest of the database's facts as labelled values.
  It weighs its steps (each one's share of the plan, and the costliest) and writes the text
  plan, so the view only draws.
- **One parser per format,** pure functions from the database's text to a `Plan`, each tested
  against plans recorded from the test servers and kept as fixtures beside it: PostgreSQL's JSON,
  MySQL's two JSON formats, SQL Server's XML (with `roxmltree`, which the build already has
  for SVG icons).
- **Asking for the plan** stays where explaining is today: the dialect (`dbrover-sql`) says what
  to put before the statement, which session setting to switch around it, and which format
  comes back, for an estimated or an analysed plan and the server the console is connected to.
  The console sends it as a run whose purpose is **Explain** or **Explain analyse**; the engine
  switches the setting around it, binds no parameters (they are written in, as for Explain
  today), and for **Explain analyse** opens transactions and applies the read-only rules as for
  **Run**.
- **The console** parses the run's one value into a `Plan` when the statement finishes. If the
  value can't be read as a plan, the grid shows it as before, with the reason above it.

### Layers

Each layer is a pull request of one stack, and each works on its own:

1. **W10a — The plan type and PostgreSQL:** `dbrover-plan`, its weighing and text, and the
   PostgreSQL parser with recorded plans; this design.
2. **W10b — The plan view:** ⌥⌘E draws PostgreSQL's plan as a tree, with the costliest step
   highlighted, **Tree** and **Text**, and the copy buttons.
3. **W10c — Explain analyse:** the action and its key, actual rows, loops and time, flagged
   estimates, and the safety rules, with engine tests on PostgreSQL's read-only transaction.
4. **W10d — MySQL and MariaDB:** both JSON formats, the session setting, `EXPLAIN ANALYZE` and
   MariaDB's `ANALYZE`.
5. **W10e — SQL Server:** the XML showplan, estimated and actual.
6. **W10f — The manual:** a chapter on reading plans, and the roadmap's demo note.

**Exit:** on the PostgreSQL test server, ⌥⌘E on a join of `orders` and `customers` shows a tree
whose costliest step is highlighted, and **Explain analyse** adds the actual rows.

## W11 — Read-only on every engine

A read-only data source (W2) and the MCP server refuse a change twice: DbRover's statement
check refuses anything that doesn't only read, and the database refuses what the check can't
see, such as a function that writes. The second half needs the engine's own read-only mode,
which DbRover uses today on PostgreSQL, MySQL, MariaDB and SQLite only. Elsewhere a read-only
data source has the check alone, and the MCP server serves it only through an MCP login whose
grants allow nothing but reading (N0a, N1 to N3), or not at all (DuckDB).

W11 gives ClickHouse and DuckDB files their read-only modes, and records, for every other
engine, why the login stays the guard.

### Each engine's guard

| Engine | What makes the database refuse a write | Read-only data source | MCP server | Since |
| --- | --- | --- | --- | --- |
| PostgreSQL | `BEGIN READ ONLY` around each statement | the check, then the transaction | served | W2, MCP1 |
| MySQL, MariaDB | `START TRANSACTION READ ONLY` | the check, then the transaction | served | W2, MCP1 |
| SQLite | the file opened read-only | the check, then the file | served | W2, MCP1 |
| **ClickHouse** | **`readonly = 2` on every request** of the connection | the check, then the setting | **served**, an MCP login optional | **W11** |
| **DuckDB, a file** | **the file opened with `access_mode = 'READ_ONLY'`** | the check, then the file | **served**, the file held only during a call | **W11** |
| DuckDB, in memory | none: an in-memory database can't be opened read-only | the check alone | not served | — |
| SQL Server | none per session: the MCP login's grants | the check alone, or a read-only login | served only through an MCP login | N0a |
| Redis, Valkey, Dragonfly | none per session: an ACL user with only `+@read` | the check alone, or such a user | served only through an MCP login | N1 |
| MongoDB | none per session: a user with only the `read` role | the check alone, or such a user | served only through an MCP login | N2 |
| Elasticsearch, OpenSearch | none per session: a user or API key with read privileges | the check alone, or such a user | served only through an MCP login | N3 |

**What was weighed and left, per engine:**

- **SQL Server** has no read-only transaction. `ApplicationIntent=ReadOnly` only routes a
  connection to a readable secondary of an availability group; on a primary or a lone server it
  changes nothing. A database set `READ_ONLY` is read-only for everyone. N0a already serves SQL
  Server through an MCP login, for reading, and the dialog already says so: the **MCP login**
  row is marked required, its tooltip says why, and a read-only level names the login to use.
  Nothing is left to build; the `login` MCP test binary queries the test server through one.
- **Redis, Valkey and Dragonfly:** `READONLY` only lets a cluster replica answer reads; it
  refuses nothing on a primary. There is no per-connection read-only switch, so an ACL user is
  the guard (N1).
- **MongoDB:** a read preference chooses which member answers reads; writes still go to the
  primary, so it refuses nothing. Transactions have no read-only form. A user with the `read`
  role is the guard (N2).
- **Elasticsearch and OpenSearch:** an index's `index.blocks.write` setting refuses writes for
  every client, not for one connection. A user or API key with read privileges is the guard
  (N3, and #306 for the MCP server).

### ClickHouse

- **The setting:** a read-only connection sends `readonly = 2` with every request, beside the
  output settings each request carries already. The server checks a request's settings against
  the user's own `readonly`, then applies them together, so the setting can't be lifted later in
  the request, or by a later one.
- **2, not the roadmap's 1.** Both refuse `INSERT`, DDL, `OPTIMIZE`, `SYSTEM` and any change to
  `readonly` itself, with ClickHouse's own `READONLY` error. `1` also refuses changing any
  other setting, which breaks reads in two ways tried on the test server:
  - a `SELECT … SETTINGS max_threads = 1` fails, though it only reads;
  - a user whose profile already has `readonly = 2`, as the manual will now suggest for an MCP
    login, can't be sent `readonly = 1` at all ("Cannot modify 'readonly' setting").

  What `2` allows besides, `SET` and temporary tables, the check refuses before it is sent.
- **The MCP login on ClickHouse becomes optional,** as on PostgreSQL: the setting makes the
  server refuse writes. The manual's suggested login changes from `SETTINGS readonly = 1` to
  `SETTINGS readonly = 2`: DbRover sends its output settings on every request, which a user
  with `readonly = 1` is refused, so such a login can't connect today.
- **No transaction is opened,** so a read-only ClickHouse connection is opened read-only for its
  whole life (`ReadOnlyMode::Connection`, as a SQLite file). `execute` stays refused for MCP
  clients, since ClickHouse has no transaction to run changes in, so the MCP server opens
  ClickHouse read-only even in a project that allows changes.
- **Nothing to hide a write in:** ClickHouse has no procedures, and its functions don't write.
  The exit test therefore sends an `INSERT` on a connection opened as a read-only data source's
  are, past the check, and gets ClickHouse's error; and a read-only data source's console runs
  `SELECT … SETTINGS readonly = 0`, which the check reads as a `SELECT` and ClickHouse refuses.

### DuckDB

DuckDB opens a database file read-only or read-write when it opens it, and a read-only
database refuses every change to that file, including one inside a function the check reads as
a `SELECT`: `SELECT nextval('orders_id')` advances a sequence, and is refused.

**The decision: a DuckDB file is open in one configuration at a time, in DbRover as between
programs.** DuckDB's own rule between processes is one writer, or any number of readers: a
process that has the file open for writing locks out every other, even one that only reads. The
driver applies the same rule within DbRover:

- A read-only data source's sessions (its explorer and every console) share one database,
  opened read-only, as a writable data source's already share one opened read-write.
- **Opening a file read-only while this process has it open for writing, or the other way
  round, is refused** with an error naming the file and saying which way it is open, so the
  user disconnects the other data source first.
- **Why not two databases:** DuckDB 1.5.6 does let one process open a file twice, once each
  way, through the C API DbRover links (tried on 2026-10-31). But a database keeps the file's
  blocks and its log of changes in memory: the read-only one doesn't see the writer's later
  commits, and may read blocks the writer's next checkpoint has already reused. The roadmap's
  note that DuckDB "refuses a second configuration" is true of its command-line client and
  Python's, which share databases through a cache; DbRover keeps its own, by file and mode.
- **Why not a process of its own** for a read-only session: rows, cancels and errors would
  cross a process boundary DbRover doesn't have, for no gain, since a separate process still
  can't open a file another holds for writing.

**The MCP server** is a process of its own, so DuckDB's lock decides there:

- It serves a DuckDB file data source, opened read-only.
- **It holds the file only while a call runs,** closing the connection when the call ends
  rather than after a few idle minutes as for a server: a read-only open locks out writers, so
  holding it idle would keep the app from opening the file for writing for minutes after an
  assistant's last question.
- While another program, DbRover's app included, has the file open for writing, a call fails
  with DuckDB's lock error, which names the process holding it. The manual says so.

**In memory:** DuckDB refuses to open an in-memory database read-only, and one has nothing to
protect: everything in it is gone when it disconnects. What it could still change are files,
through `COPY … TO` and `ATTACH`, which the check refuses as administration. A read-only
in-memory data source keeps the check alone, the dialog's warning says so, and the MCP server
doesn't serve it, as before.

**ATTACH:** a read-only database can still attach another file for writing
(`ATTACH 'other.duckdb' (READ_WRITE)` works in one), so another file is guarded by the check
alone, which refuses `ATTACH` and `COPY` outright.

### How it's built

- **A guard per data source, not per engine:** `read_only::mode` and `read_only::guard` take
  the connection's settings rather than the driver, since a DuckDB file has a read-only mode
  and an in-memory DuckDB database doesn't. The dialog asks its form the same question.
- **`read_only::connection_config`** sets the new `read_only` of a ClickHouse configuration,
  which is never saved: the safety level is the setting users see. For a DuckDB file it sets
  the `read_only` its **Read-only** box already saves.
- **The MCP server opens read-only** every data source whose engine has no read-write
  transaction, whatever the project allows, and only engines with one are ever opened writable.
  It closes a DuckDB connection when the call ends.

### Layers

1. **W11a — The plan:** this design, and the roadmap's entry.
2. **W11b — ClickHouse:** `readonly = 2` on a read-only connection; its read-only data sources
   and MCP use it; driver and engine tests on the test server, an MCP test of a ClickHouse data
   source served without a login.
3. **W11c — DuckDB:** read-only files for read-only data sources and MCP, one configuration of
   a file at a time, the MCP server holding a file only during a call; driver, engine and MCP
   tests on files, which need no server and run on every OS.
4. **W11d — The manual:** each engine's guard in the safety levels and MCP chapters, the
   ClickHouse login's `readonly = 2`, and the roadmap's demo note.

SQL Server and the document, key and search engines need no layer: their guard stays the login,
which N0a to N3 built.

**Exit:** a read-only ClickHouse and a read-only DuckDB data source refuse a write the check
can't see, with the database's own error: on DuckDB a `SELECT` of a function that writes, on
ClickHouse a `SELECT` that lifts `readonly`, and an `INSERT` sent past the check. An MCP client
can query a SQL Server data source that has an MCP login (N0a's `login` test).

## W12 — Charts from results

A result often answers a question better as a picture: totals by month, counts by status, one
measure against another. Today the rows have to be copied into a spreadsheet to see that. W12
draws a chart of a result's columns in the results panel, from the rows already fetched,
without running anything again, and exports it as SVG or PNG, as ER1's diagrams are
exported.

- **A tab beside the grid:** **Grid** and **Chart** switch above any result with rows, the way
  **Grid**, **Tree** and **JSON** switch for documents (a result of documents gets **Chart**
  at the end of that same row). Switching never runs the statement again. The chart charts
  the rows the grid holds, in the grid's order and through its filter, so sorting or
  filtering the loaded rows changes the chart too, and **Load more rows** adds to it.
- **Three kinds:** **Bars**, **Lines** and **Scatter**, picked above the chart.
- **Columns:** **X** is one column, picked from a list of every column; **Y** is one or more
  columns of numbers, ticked in a menu. Each Y column is a series in a colour of its own, with
  a legend once there are two.
- **What it shows:** axes with ticks and labels, the X column's name under the chart and the
  Y column's name (or the legend) beside it, and a gap, never a zero, wherever a value
  is missing. Hovering a bar or a point shows its X value and every series' value there.
- **A line under the chart** says what was charted and what wasn't: "Charting 12 rows",
  "Charting the first 250 of 1,200 rows", "3 rows left out: no month value".
- **Export ▾** writes **SVG…** or **PNG…** at 1×, 2× or 3×, at the size the chart has on the
  screen, in the light theme's colours on white, through ER1's export (see
  [Export](#export-shared-with-er1)).

### Picking the columns

Values arrive as text (ADR 0004), so each column is first given a role, from the type its
driver declares when there is one (`Dialect::value_kind`, as exports do) and otherwise from its
values (PostgreSQL's text protocol declares none):

| Role | Declared kind | Undeclared: every value that isn't null… |
| --- | --- | --- |
| **Number** | integer, decimal, float | …reads as a finite decimal number, with an optional exponent |
| **Time** | date, timestamp, timestamp with a zone | …reads as an ISO date or timestamp, with or without an offset |
| **Text** | text, boolean, anything else | …anything else, and a column whose values are all null |

A value with a leading zero (`007`, `00501`) makes an undeclared column **Text**, as the Excel
export reads it: it is a code, not a quantity. A boolean is **Text**, two categories.

**What the chart starts with,** when the tab opens on a result:

- **X:** the first **Time** or **Text** column; if every column is a **Number**, the first
  column.
- **Y:** every other **Number** column, up to eight (the palette's colours); the rest can be
  ticked by hand, beyond eight with colours reused.
- **Kind:** **Lines** when X is **Time**, **Scatter** when X is a **Number**, otherwise
  **Bars**. A `GROUP BY` of months and totals opens as bars when the month is text
  (`to_char(…, 'YYYY-MM')`) and as lines when it is a date or timestamp (`date_trunc`); the
  other kind is one click away.
- **Nothing to chart:** a result with no **Number** column but X shows "A chart needs a column of
  numbers" in place of the chart, and the switch stays, so the grid is one click away.

The choice is kept for the console's next run while that result has the same column names,
so running a query again after changing its `WHERE` keeps its chart.

### X and Y on the axes

- **Bars** always place X as categories: one slot per row, in the grid's order, labelled with
  the value as the grid shows it. Several series are bars side by side within the slot. The Y
  axis always includes zero, and a negative value draws a bar down from it.
- **Lines** and **Scatter** place a **Number** or **Time** X by its value, on a continuous
  axis: lines join points in X's order (the rows sorted by X for the line, whatever the grid's
  order), so an unsorted result doesn't zigzag. A **Text** X is placed as categories, in the
  grid's order, as for bars. The Y axis spans the values with a margin; zero is not forced.
- **Ticks:** numbers at 1, 2 or 5 times a power of ten, enough for one every 80 points or so,
  labelled with a thousands separator and only the decimals the step needs; times at a unit
  picked from the span (years, months, days, hours, minutes, seconds) and labelled to that
  unit (`2026`, `Mar 2026`, `14 Mar`, `14:30`); categories labelled every *k*-th slot, *k* the
  smallest that keeps labels from overlapping, and every label cut to 20 characters with an
  ellipsis.
- **A Time X with an offset** (`timestamptz`) is placed by the instant it names and labelled in
  the offset of its first value, so a result in UTC reads in UTC.

### Nulls, values that aren't numbers, and many rows

- **A null X, or one that doesn't read as the column's role** (an undeclared value when the
  column was declared): the row is left out, and the line under the chart counts it.
- **A null Y, or one that isn't a number** (`NaN`, `Infinity`, text in a declared numeric
  column): no bar, no point, and a break in the line; counted in the same line, per series.
- **Many rows:** bars chart the first **250** rows, since past that a bar is narrower than a
  point on a wide screen and the labels say nothing; lines and scatter chart the first
  **50,000** rows, which the grid reaches only after a hundred pages of **Load more rows**. The
  line under the chart says how many were charted, and suggests grouping in the query. Both
  numbers are a first guess, to be confirmed by measuring the frame time in W12c.
- **Many series:** a result with dozens of numeric columns starts with eight ticked.
- **An empty result** shows "No rows to chart".

### Drawn with egui's shapes, not `egui_plot`

`egui_plot` 0.37 is the release built on egui 0.36, a pure-Rust crate whose dependencies the
build mostly has already. It would give pan, zoom and hover for free. W12 draws with egui's own
shapes instead, because a chart is exported, and the export has to show the chart the tab
shows:

- `egui_plot` paints straight to egui's painter and has no way to give its geometry back.
  Exporting would mean laying out the axes, ticks, labels, bars and legend a second time for
  SVG, and the two would drift.
- One pure layout gives both: it turns a chart, a size and a way to measure text into marks
  (rectangles, polylines, circles, text, lines) in points. The tab paints the marks with
  egui; the export turns the same marks into ER1's picture. What was missing from
  `egui_plot` is small: categorical axes with text labels, and time ticks, which it leaves to
  the caller's formatter anyway.
- It adds no crate, and no crate whose version must move in lockstep with egui, as `egui_dock`
  already must.
- Pan and zoom are not needed for a chart of a result: filtering the grid, or the query, narrows
  it. They can come later.

The cost is the layout and its tests: about as much code as the plan view's tree. Hover is a
nearest-mark lookup over the marks the layout returns.

### How it's built

- **A new crate, `dbrover-chart`,** on `dbrover-core`, with no UI and no database, as
  `dbrover-plan` is: column roles, the starting choice, reading a series from rows (with what
  was left out and why), scales and ticks, and the layout into marks. `jiff`, which the build
  has already, reads dates and timestamps. Everything is tested without a window, which matters
  since the app's tests are slower to build and run.
- **The tab** keeps a `ChartView` on each statement's view: the kind, X and Y, and the marks
  of the last layout, computed again only when the rows, the grid's order, the choice or the
  size change, so a large scatter isn't laid out every frame.
- **Painting** turns marks into egui shapes in the theme's colours; the series' colours are a
  palette of eight that reads on both themes.

### Export, shared with ER1

ER1e builds the diagrams' export: SVG written from shapes, and PNG at 1×, 2× or 3× drawn from
that SVG with resvg, whose `text` feature it turns on with the app's bundled Inter and
JetBrains Mono, written by the engine off the UI thread. W12 uses that same code and copies none
of it:

- **The shared part** is the picture (shapes in points: rectangles, lines, paths, circles,
  text), writing it as SVG, drawing it as PNG with the bundled fonts, the engine's command that
  writes the file, and the **Export ▾** menu with its save dialog. It is proposed to ER1 as a
  small leaf crate of its own, so the chart needn't depend on `dbrover-diagram`; it lives
  where ER1e puts it.
- **The chart's part** is turning its marks into that picture, which is a few lines per mark.
- **Order:** W12's export layer is the last of its stack and waits for ER1e to be on `main`.
  Every layer before it works without it, and W12 is useful without export until then.

### Layers

1. **W12a — The plan:** this design, and the roadmap's entry.
2. **W12b — The chart, as a pure function:** `dbrover-chart`: column roles, the starting choice,
   series with nulls and values left out, the row limits, scales and ticks for numbers, times
   and categories, and the layout into marks, with unit tests of each.
3. **W12c — The chart tab:** **Grid** and **Chart**, the kind, X and Y, painting the marks,
   hover, the legend and the line under the chart; kittest tests on the SQLite fixture.
4. **W12d — The manual:** a section on charts in the consoles chapter, and the roadmap's demo
   note.
5. **W12e — Export:** **Export ▾** on the chart, through ER1e's picture, SVG and PNG; the
   manual's section and the demo note say so.

**Exit:** a `GROUP BY` result of months and totals charts as bars and as lines, and exports as
an SVG a browser shows.

## Decisions

Proposed defaults; each is confirmed when its milestone starts.

| Question | Proposed | Consequence |
| --- | --- | --- |
| Export format | **The same TOML as `data_sources.toml`**, plus a format version and the projects | Hand-editable, and diffable in git. |
| Matching on import | **By id**, then the user decides each entry | Re-importing is safe. A copy is an explicit choice. |
| Read-only data sources | **The same checks as MCP's read-only projects** | One way to enforce read-only, tested once. |
| SSH | **`russh`, with host keys checked against `known_hosts`** | Nothing to install. An unknown host asks, and a changed key is refused. |
| Grid edits in the history | **Store the values they replaced**, and offer the statement to put them back, never undo silently | It explains every change, without DbRover ever writing anything you didn't review. |
| Database-side sort and filter | **Off by default**, a switch per grid | Today's behaviour stays. Re-running a query is a deliberate step. |
| Where plans are parsed | **In the app, by a pure crate**, from the explain's one value | The engine and the drivers stay as they are; each parser is tested from recorded plans without a server. |
| The text plan | **Written from the parsed plan**, in PostgreSQL's layout, rather than asked for again | One layout on every engine, and Explain analyse never runs the statement twice. |
| What "costliest" means | **A step's own share**: its cost minus its children's, by time once analysed | The step to look at first, rather than the root, whose cost includes everything. |
| Explain analyse on a careful data source | **Always asks**, even for a `SELECT` | That it runs the statement is easy to forget; a run of the same `SELECT` doesn't ask. |
| A change under Explain analyse | **Runs as Run would**, not rolled back on its own | One rule for running; a manual-commit console keeps it open to roll back. |
| ClickHouse's read-only setting | **`readonly = 2` on every request** of a read-only connection | Writes, DDL and lifting the setting are refused by ClickHouse; a `SELECT`'s own `SETTINGS` still run, and so does a login whose profile has `readonly = 2`. |
| A DuckDB file open read-only and read-write at once | **Refused within DbRover,** as DuckDB's lock refuses it between programs | One writer or any number of readers, everywhere. A read-only data source and a writable one on the same file can't be connected together. |
| How long the MCP server holds a DuckDB file | **Only during a call** | The app can open the file for writing as soon as a call ends; each call opens the file again, which takes milliseconds. |
| Engines without a read-only session (SQL Server, Redis, MongoDB, Elasticsearch) | **The login stays the guard,** as N0a to N3 built it | Nothing on those engines changes in W11; each one's reason is recorded above. |
| How a chart is drawn | **egui's own shapes**, from one pure layout into marks, not `egui_plot` | The export draws the same marks, so the SVG is the chart the tab shows; no crate tied to egui's version. Pan and zoom are given up, and can come later. |
| Which rows a chart shows | **The grid's loaded rows, in its order and through its filter** | No query runs again; what the grid shows and the chart shows agree. A chart of every row needs **Load more rows**, or grouping in the query. |
| A column's role with no declared type | **Read from its values**, as the Excel export does, leading zeros making it text | PostgreSQL declares no types over its text protocol, and a code like `00501` isn't a quantity. |
| A missing or unreadable value | **A gap, counted under the chart**, never a zero | A zero would be a value the database didn't return. |
| Where the export lives | **ER1e's**, shared, with W12 turning its marks into ER1's picture | One SVG writer, one PNG renderer with the bundled fonts, one export command. W12's export waits for ER1e. |

## Risks

- **Secrets leaking through an export.** Exports are built from an allow-list of fields, not by
  copying the file minus some. A test exports every kind of data source and searches the result
  for every saved password.
- **SSH host keys.** Accepting an unknown key without asking would invite man-in-the-middle
  attacks. The fingerprint is always shown, and a changed key is always refused.
- **Re-running queries.** Sorting in the database and exporting every row run the query again,
  which on a busy server costs time and may see different data. The grid says which rows it
  shows.
- **History size.** Storing before values makes the history grow faster. Retention bounds it,
  and large values are stored truncated with a note.
- **Plan formats change.** Each engine adds facts to its plans between versions. Parsers read
  the facts they know into the step and keep every other one as a labelled value, so a new key
  shows rather than breaks the plan; a value that can't be read as a plan falls back to the
  grid.
- **A user who can't open the file read-only.** A read-only DuckDB data source fails to
  connect while another data source, or another program, has its file open for writing. The
  error names the file and says it is open for writing elsewhere, so what to close is clear.
- **What a read-only mode doesn't cover.** ClickHouse's setting and DuckDB's access mode
  guard the server's tables and the opened file; DuckDB can still write other files (`COPY`,
  `ATTACH`), and only the check stops that. The manual says so, as it does for SQL Server's
  procedures.
- **A chart that misleads.** A column read as numbers when it holds codes, or a chart of the
  first 250 rows taken for all of them. Leading zeros keep codes as text, X and Y can always be
  changed, and the line under the chart says how many rows were charted and left out.
- **A slow frame on a big scatter.** Fifty thousand points are laid out once, not every frame,
  and painted as one mesh of small circles; W12c measures it, and the limit moves if needed.
- **Waiting on ER1.** W12's export can't land before ER1e. Charts are useful on screen
  meanwhile; if ER1 is delayed, the export layer waits rather than writing a second SVG and
  PNG path.
