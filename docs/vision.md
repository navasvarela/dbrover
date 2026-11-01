# Vision

## Why build this

1. **Learning.** A database client touches most of what makes Rust interesting in practice:
   async I/O, cancellation, streaming, a non-trivial UI, trait-based plugin points, and
   careful error handling at system boundaries.
2. **Personalisation.** The established tools (DBeaver, DataGrip, TablePlus, pgAdmin) are
   capable, but heavy, closed, or configurable only within the limits their makers allow.
   DbRover should be small, fast to start, and driven by plain config files I can version and
   tweak.

## Goals

- Start in well under a second and stay responsive while queries run. The UI thread never
  waits on the network.
- Handle large result sets without loading them fully: stream rows and page through them.
- Make every query cancellable.
- Keep all configuration (data sources, keymaps, custom actions) in human-editable files.
  Secrets are the exception and live in the OS keychain.
- Keep the engine-specific code behind one driver abstraction, so adding an engine is a new
  crate rather than edits scattered across the app.
- Speak the databases developers use, SQL or not. SQL engines came first; Redis, MongoDB and
  the other NoSQL stores in [NoSQL databases](nosql.md) were out of scope until 2026-10-29, and
  now come to the same explorer, console and grid, each in its own query language.

## Non-goals (for now)

- Replacing each NoSQL vendor's own tool (RedisInsight, MongoDB Compass, Kibana). DbRover
  browses, queries and edits; monitoring, profiling and cluster administration stay with them.
- Feature parity with full database IDEs such as DBeaver or DataGrip. In particular:
  refactorings, VCS integration, and a full SQL language server. ER diagrams and schema
  diffing were on this list until 2026-10-29; both are now planned, though not soon (ER1 and
  W13 in the [roadmap](roadmap.md)).
- A plugin system that loads native code. Extension happens through files, external commands
  and, later, sandboxed WebAssembly plugins (see [Hackability](hackability.md) and
  [ADR 0005](adr/0005-customisation-files-commands-then-wasm.md)). New database engines are
  still crates in the workspace.
- Multi-user or server deployment. This is a single-user desktop app.
- Business intelligence: dashboards, scheduled reports on a server, sharing. Reports that
  combine data sources into files, run by one person from the app or the command line, are in
  scope since 2026-11-01 (the RP track in the [roadmap](roadmap.md), designed in
  [Reports](reporting.md)).
- Windows as a first-class target. macOS first; Linux and Windows should work because
  `eframe` does, but nobody tests them until someone needs them.

## Terminology

Database tools often call the SQL editor a "console", which collides with "a console for logs".
DbRover uses these terms everywhere (docs, code, UI):

| Term | Meaning |
| --- | --- |
| **Data source** | A saved connection configuration (host, port, user, driver, options). |
| **Session** | A live connection to a data source. A data source can have several sessions. |
| **Query console** | An editor tab bound to one session, in the engine's query language: SQL, Redis commands, MongoDB's shell syntax, and so on. |
| **Log console** | The panel that shows connection, query and server messages. |
| **Catalog** | The tree of objects (databases, schemas, tables, keys, collections, …) a data source exposes. |

## What "personalised" means concretely

These are the hooks that make the tool mine rather than a clone of an existing IDE. The keymap
is built (M5); the rest is planned in [Hackability](hackability.md):

- **Keymap file** (`keymap.toml`): every UI action has a stable id, bindings are data.
- **Custom tree actions** (`actions.toml`): templated SQL attached to catalog node kinds,
  e.g. "row count", "table size", "last 100 rows ordered by `created_at`".
- **Command actions** (`actions.toml`): run your own programs on a table or the selected rows.
- **Snippets**: named SQL fragments with placeholders, expandable in the query console.
- **Themes** (`theme.toml`): your own colours on top of the light or dark theme.
- **Per-data-source colour and safety flags**: e.g. production data sources get a red
  header and confirm before running `UPDATE`/`DELETE` without a `WHERE`.
- **Own engine as a data source**: a driver for the storage engine built in
  `db-data-structures`, once it speaks a query language.
