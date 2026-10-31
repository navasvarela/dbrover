# MCP server

Status: **built**: P0 (projects), MCP1 (the read-only server), MCP2 (permissions) and MCP3
(in the app). What remains is under Later.
Proposed on 2026-10-24.

The [Model Context Protocol](https://modelcontextprotocol.io) (MCP) lets AI assistants and
coding agents call tools that an application provides. This plan gives DbRover an MCP server, so
an assistant can browse the catalog and query the databases DbRover already knows. It does so
through the same saved data sources, drivers and keychain as the app.

Two ideas shape the plan:
- **Projects.** Data sources are grouped into projects, and an MCP server exposes one project at
  a time. A client working on the analytics code sees the analytics databases and nothing else.
- **Permissions per project, with global defaults.** What an assistant may do is set per project:
  read, change data, change the schema, administer. Anything a project doesn't set comes from the
  global defaults, which only allow reading.

## What the user sees

1. In the app, data sources are grouped into **projects** in the explorer. A data source belongs
   to one project, and data sources in no project sit under **No project**.
2. **Project settings…** on a project shows its MCP permissions. Each one is a checkbox that
   starts at the global default and can be overridden. **View → Settings… → MCP** holds the
   global defaults.
3. **Copy MCP configuration** on a project copies the snippet an MCP client needs, e.g.:

   ```json
   {
     "mcpServers": {
       "dbrover-analytics": {
         "command": "/Applications/DbRover.app/Contents/MacOS/dbrover",
         "args": ["mcp", "--project", "analytics"]
       }
     }
   }
   ```

4. The client starts `dbrover mcp --project analytics` as a background process and talks to it
   over standard input and output. The app doesn't need to be open.
5. Everything an assistant runs shows up in the query history, marked as coming from MCP, and in
   the log.

## Projects (P0)

Projects are useful in the app on their own, so they come first and don't depend on MCP.

- **`projects.toml`** next to `data_sources.toml`, owned by `dbrover-store`:

  ```toml
  [mcp_defaults]          # the global defaults
  read = true
  write = false
  schema = false
  admin = false
  max_rows = 500
  statement_timeout = "30s"

  [[project]]
  id = "5b0c…"            # stable, like data source ids, so renaming never breaks a client
  name = "Analytics"
  slug = "analytics"      # what `--project` takes; unique, lower case
  mcp_enabled = false     # a project is invisible to MCP until this is switched on

  [project.mcp]           # overrides; anything left out comes from mcp_defaults
  write = true
  ```

- **Membership:** each data source gains an optional `project` id in `data_sources.toml`.
  Older files load unchanged, with every data source in no project.
- **Explorer:**
  - project headers that fold, with the data sources under them;
  - New project…, Rename…, Delete… (which moves the data sources to No project and drops
    nothing);
  - Move to project › on a data source;
  - the filter box also matches project names.
- **Data source colour still means the environment** (red for production). A project may mix
  environments, and the permissions below decide what an assistant can do in each.

**Exit:** I group my data sources into two projects, rename one and restart the app, and the
grouping survives. A `data_sources.toml` from 0.2.0 loads unchanged.

## Permissions

Each project has these, each falling back to `mcp_defaults`:

| Permission | Allows | Default |
| --- | --- | --- |
| `read` | Browsing the catalog, `SELECT`, `EXPLAIN`, `SHOW`, describing tables | on |
| `write` | `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `TRUNCATE` | off |
| `schema` | `CREATE`, `ALTER`, `DROP`, `COMMENT ON` and other DDL | off |
| `admin` | `GRANT`, `REVOKE`, roles and users, `CREATE DATABASE`, `VACUUM`, server settings, `COPY` to or from files, killing sessions | off |
| `max_rows` | The most rows one call returns; the reply says when it is cut short | 500 |
| `statement_timeout` | How long one statement may run before it is cancelled | 30 s |
| `schemas` | Optional allow-list of schemas the catalog tools show and statements may name | all |

Overrides per data source, inside a project, are left for later (see the end of this plan).
Until then, a production database that needs tighter rules goes in its own project.

### How a permission is enforced

Checking the text of a statement is not enough on its own. A `SELECT` can call a function that
writes. A `WITH` clause can hold a `DELETE`. `SELECT … INTO` creates a table. So each statement
goes through up to three layers, and the strongest layer the engine offers decides:

1. **The parser.** `dbrover-sql` parses each statement with the dialect's `sqlparser` dialect.
   It classifies the statement as read, write, schema or admin, looking inside CTEs,
   `SELECT INTO`, `COPY`, `CALL` and the rest. A statement that needs a permission the project
   lacks is refused before it reaches the database.
   - A statement the parser can't read is refused unless the project allows `admin`, because
     what it does can't be known.
   - The classifier lives next to the existing `safety` and `transaction` modules, with a corpus
     test per dialect.
2. **The database's own read-only mode**, whenever a project allows only `read`:
   - **PostgreSQL:** every call runs in `BEGIN READ ONLY` and is rolled back. The server itself
     then refuses writes, including writes from functions the parser can't see into.
   - **MySQL and MariaDB:** `START TRANSACTION READ ONLY`, then a rollback.
   - **SQLite:** the connection is opened read-only, as the existing `read_only` option already
     does.
   - **ClickHouse and DuckDB files** (W11, [well-rounded.md](well-rounded.md#w11--read-only-on-every-engine)):
     every ClickHouse request carries `readonly = 2`, and a DuckDB file is opened read-only and
     held only while a call runs. Both are served for reading only.
   - **SQL Server and later engines:** each engine's plan states its equivalent before its MCP
     support is switched on. For an engine without a read-only mode, the equivalent is the MCP
     login itself (N0a, [nosql.md](nosql.md)): SQL Server, Redis, MongoDB and
     Elasticsearch data sources are
     served for reading only, and only through an MCP login whose grants allow nothing but
     reading; on Redis, an ACL user with `+@read` (N1); on MongoDB, a user with the `read`
     role (N2), and `list_objects` lists its databases and collections; on Elasticsearch and
     OpenSearch, a user or API key with the `monitor` cluster privilege and `read`,
     `view_index_metadata` and `monitor` on its indices (N3), and `list_objects` lists the
     explorer's folders, indices and fields. `list_objects` pages a level too large
     to list at once, 1,000 objects at a time, with a filter the database applies.
3. **The database's own grants: the recommended setup for anything beyond reading.**
   - A data source can carry a **separate MCP login**: another user name and password, used only
     by the MCP server. Point it at a database role that has exactly the grants the project
     should have. The database then enforces them, whatever the parser misses.
   - Project settings say so when `write`, `schema` or `admin` is on but no MCP login is set.

Every call also gets `statement_timeout` applied in the session (or the driver's cancel when the
engine has no such setting). Results are limited to `max_rows`.

## The MCP server (MCP1 and MCP2)

- **One binary.** `dbrover mcp --project <slug>` runs the same executable without opening a
  window, next to the existing `--version`. This means:
  - there is no second download, and the client config points at the installed app;
  - it reuses `dbrover-store` (data sources, projects, keychain), `dbrover-core` and the drivers,
    `dbrover-sql` (classification, splitting) and the engine's history log.
- **SDK:** [`rmcp`](https://crates.io/crates/rmcp), the official Rust SDK (3.7 when this was
  written). Check its maintenance and its API before starting, as with every new dependency.
- **Transport:** standard input and output, which every MCP client supports. The client starts
  the process and stops it, so nothing listens on the network. An HTTP transport (MCP's
  "Streamable HTTP") is listed under Later, because it needs authentication.
- **Tools**, each with MCP's hints (`readOnlyHint`, `destructiveHint`), so clients can ask the
  user before the risky ones:

  | Tool | Needs | Does |
  | --- | --- | --- |
  | `list_data_sources` | read | The project's data sources: name, engine, environment colour, server version once connected |
  | `list_objects` | read | One level of the catalog, as the explorer loads it (`Connection::children`) |
  | `describe_table` | read | Columns, types, keys and indexes |
  | `get_definition` | read | A view's, function's or index's source |
  | `query` | read | Runs one read statement and returns its rows (up to `max_rows`), as JSON or, with `format`, as CSV, Markdown or JSON Lines text (W7i) |
  | `explain` | read | The plan of one statement, without running it |
  | `execute` | write, schema or admin | Runs statements that change something, in one transaction where the engine allows |

  Reading and changing are separate tools on purpose. A client can then allow `query` freely
  and ask before every `execute`.
- **Results** come back as JSON: column names and types, rows as text cells (as in the app;
  see ADR 0004), a `truncated` flag, the elapsed time and rows affected. Errors carry the
  database's message, SQLSTATE and position, so an assistant can fix its SQL.
- **Connections** open on first use and close after a few idle minutes. The process can live as
  long as the client session without holding connections the whole time.
- **Passwords** come from the OS keychain, like the app's. Until macOS builds are signed (R2),
  macOS asks once whether the process may read each one. This is the same prompt the app gets
  after each update.
- **Audit:**
  - every call goes to the log as an info line, with the project, data source, tool and
    statement kind;
  - every statement goes to the query history with a new `source = "mcp"` column, so the app's
    History window shows it marked;
  - refusals are logged too, with the permission that was missing.

### MCP1 — Read-only server

`dbrover mcp --project …` with the read tools, read-only transactions, `max_rows`,
`statement_timeout` and the audit trail. A project must have `mcp_enabled` switched on, and
every permission beyond `read` is ignored. This is safe to ship first.

**Exit:**
- An MCP client lists a project's tables, describes one and queries it, on PostgreSQL, MySQL and
  SQLite.
- A `DELETE` hidden in a CTE is refused by the parser.
- `SELECT` on a function that writes is refused by the database's read-only transaction.
- Both refusals appear in the log.

**As built** (2026-10-24):
- **rmcp 3.7.0**, the official SDK under the `modelcontextprotocol` organisation, released two
  days before this milestone. It is very actively maintained: seven minor releases in the two
  and a half months before (3.1 to 3.7), with small API churn between them (3.7 deprecates
  `ServerInfo` for `ServerConfig`). Only its `server`, `macros` and `transport-io` features are
  used; the tests add `client` and `transport-child-process`. Its `schemars` dependency brings
  `chrono` along. Nothing in it changed the plan; expect to adjust a few names on upgrades.
- The server is the `dbrover-mcp` crate; `dbrover mcp --project <slug>` in the app's binary
  parses its arguments strictly and runs it on a tokio runtime, with no window.
- Statements are classified by `dbrover_sql::classify`, which walks every nested statement and
  query in `sqlparser`'s tree (its `visitor` feature), so a `DELETE` inside a `WITH`, `SELECT …
  INTO` and `FOR UPDATE` are caught. Unknown statement types count as admin, and `EXPLAIN` of a
  write is refused even though it wouldn't run, to keep the rule simple.
- `DriverError::Statement` gained a `sqlstate`, which PostgreSQL and MySQL report and SQLite
  doesn't have.
- Projects and data sources are read once, when the server starts; restarting the server picks
  up changes.
- `history.sqlite` gained a `source` column (`app` or `mcp`) and switched to WAL mode with a
  busy timeout. The migration runs in an immediate transaction, so two processes opening an
  old file at once is safe; a test writes from two processes at the same time.
- On PostgreSQL the time limit is also set as `SET LOCAL statement_timeout`; on every engine,
  DbRover's watchdog cancels through the driver.
- SQL Server (DB2) landed during MCP1. It has no read-only transaction, so `dbrover mcp` leaves
  its data sources out, with a warning in the log, until its equivalent is chosen (a read-only
  login, most likely, with MCP2's separate login).

### MCP2 — Permissions

The `execute` tool, the parser's classification, and the `write`, `schema` and `admin`
permissions with their fallback to the defaults. Also the separate MCP login per data source.

**Exit:**
- A project with `write` on inserts a row, and is refused a `CREATE TABLE`.
- Switching `schema` on in the project lets the `CREATE TABLE` through.
- With an MCP login whose role lacks `INSERT`, the database refuses the insert even though the
  project allows it.

**As built** (2026-10-24):
- `execute` takes one or more statements separated by semicolons. Each is classified before
  anything runs; one the project doesn't allow refuses the whole call, and the log line names
  the missing permission (`missing_permission="schema"`). Transaction control is always
  refused, since every call is a transaction of its own. A project that only reads doesn't list
  `execute` at all, so it looks to a client exactly as in MCP1.
- The statements run in one transaction (`BEGIN READ WRITE` on PostgreSQL, `START TRANSACTION
  READ WRITE` on MySQL and MariaDB), committed only if all succeed. MySQL's DDL commits on its
  own, so a call holding some reports `atomic: false`, and its error says what stayed
  committed. Each statement gets the time limit and returns at most `max_rows` rows.
- SQLite files are opened read-only only when the project only reads. Otherwise every call sets
  `PRAGMA query_only` for itself: on for reads, off for `execute`.
- The transactions are the engine's: `dbrover_engine::read_only`, which the consoles of
  read-only data sources share (W2), gained `ReadOnlyMode::QueryOnly` for that SQLite case,
  and `dbrover_engine::read_write` beside it opens, commits and rolls back `execute`'s.
- Classification, beyond MCP1's: `CALL` is admin, because a procedure can do anything;
  `LOAD DATA` and extensions are admin; `DROP DATABASE`, roles and users are admin. Built-in
  functions that end sessions, change settings, touch the server's files or reach other servers
  (`pg_terminate_backend`, `set_config`, `pg_read_file`, `lo_export`, `dblink`, SQLite's
  `load_extension`…) make any statement admin, `query`'s included: some of them work inside a
  read-only transaction. A corpus per dialect (`crates/dbrover-sql/tests/classify/`) pins all
  this.
- **`schemas`** is `"all"` or a list, in `[mcp_defaults]` and each project. It hides other
  schemas from `list_objects`, refuses paths, tables and statements that name them, and refuses
  statements whose names can't all be read (grants, procedures, anything unparseable). On
  PostgreSQL each call sets `search_path` to the list, so unqualified names resolve there;
  elsewhere unqualified names need the connection's default schema in the list.
- **The MCP login** is `mcp_login = { user = "…" }` on a data source, with its password in the
  keychain entry `<id>/mcp`. `dbrover mcp` always signs in with it when it is set, and fails
  rather than falling back when its password is missing. When a project allows changes, the
  server warns at start-up for each data source without one.
- SQL Server stays unserved: a read-only login, now possible, is its likely equivalent, but
  isn't switched on yet. *N0a update:* it is now, for SQL Server and ClickHouse, for reading only
  (see [nosql.md](nosql.md)). *W11 update:* ClickHouse and DuckDB files are served in their own
  read-only modes, without an MCP login ([well-rounded.md](well-rounded.md#w11--read-only-on-every-engine)).

### MCP3 — In the app

- The Project settings window with its checkboxes and overrides, and the MCP section of
  View › Settings for the defaults.
- **Copy MCP configuration**, filled in with this machine's path to the DbRover executable.
- The history marks MCP statements and can filter by source.
- A manual chapter: setting up a client, the permissions, and the separate login.

**Exit:** starting from a fresh install, I set up an MCP client for a project using only the
app and the manual.

**As built** (2026-10-24):
- **Project settings…** on a project header holds the MCP switch and every permission and
  limit. Each starts at the default, and once changed shows **Reset**. It warns when write,
  schema or admin is on and a data source that signs in with a password has no MCP login. It
  also notes that SQLite files have no logins, and that SQL Server isn't served yet.
- The data source dialog has **MCP login** and **MCP password** for network engines. Clearing
  the login also removes its keychain entry.
- **View › Settings… › MCP** edits `[mcp_defaults]`. Unlike the rest of the window, it has its
  own Save button, so a limit typed digit by digit doesn't rewrite `projects.toml` at each
  keystroke.
- **Copy MCP configuration** uses `std::env::current_exe` with symbolic links resolved: inside
  the `.app` bundle on macOS, the installed `.exe` on Windows. On Linux it uses `$APPIMAGE`
  when set, because the running executable sits in the AppImage's temporary mount.
- The History window marks MCP statements and filters by source. The search became a
  `HistoryQuery` (data source, text, source), carried by the engine's command and event.
- The manual's "Connecting an AI assistant" chapter covers the setup, the permissions and how
  they are enforced, the MCP login with an example role, and where query results go.

## Later

- **The Streamable HTTP transport**, so one long-running server can serve several clients. It
  needs a token per client and binds to localhost only.
- **Per data source overrides** inside a project.
- **MCP resources** for the catalog, so a client can read a table's description without a tool
  call.
- **Confirmations through MCP's elicitation**, where the client supports it: the server asks the
  user before a destructive `execute`, whatever the client's own settings.
- **A row filter or column masking per project**, for data an assistant should not see.

## Decisions

Proposed defaults; each is confirmed when its milestone starts.

| Question | Proposed | Consequence |
| --- | --- | --- |
| One project per server, or all projects in one | **One project per server** (`--project`) | Each client sees only what its work needs, and its configuration names the project. Several projects means several entries in the client's config. |
| A project's MCP access when created | **Off** (`mcp_enabled = false`) | Nothing is exposed by accident; switching it on is a deliberate step. |
| Global defaults | **Read only**, 500 rows, 30 s | The first thing an assistant can do is look, not change. |
| Where projects live | **`projects.toml`**, with membership on each data source | Data sources stay one file. Projects can be edited by hand like the other files. |
| A separate binary or a subcommand | **A subcommand** of the app's binary | One download and one place the client config points at. The app's start-up must keep working without a display when it runs as `mcp`, as `--version` already does. |
| Enforcement | **Parser, plus read-only transactions, plus an optional MCP login** | Defense in depth. Only grants on a dedicated login are airtight beyond reading, and the docs say so. |

## Risks

- **The parser missing a write.** Functions, procedures and engine-specific statements can change
  data behind an innocent-looking `SELECT`. For read-only projects, the database's read-only
  transaction catches what the parser misses. Beyond reading, only a dedicated login with
  limited grants is airtight, and the UI and manual say so plainly rather than promising more.
- **A broader blast radius than the user expects.** An assistant works fast and can loop. The
  defaults only read, `execute` is a separate tool that clients can gate, and the timeout and row
  limits cap the cost of a runaway query.
- **Data leaving the machine.** Whatever `query` returns goes to the assistant's model. The
  manual says this next to the setup steps, and suggests separate projects (or the `schemas`
  allow-list) for sensitive data.
- **Two processes sharing files.** The app and the MCP server both read the data source files and
  write the history. Reads are already safe while the app writes atomically. The history is
  SQLite, which handles several writers when opened in WAL mode; this is checked with a test
  before MCP1 ships.
