# A well-rounded app

Status: **W1 to W7 built** (see the [roadmap](roadmap.md#well-rounded-track); W5's
design is in [Power in the grid](grid-power.md), W7's in
[Exports and your own formats](exports.md)); W8 planned, designed there too. W10, the visual
explain plan, designed on 2026-10-31 [below](#w10--a-visual-explain-plan).
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
