# Testing

## Driver conformance suite

The driver traits are only honest if every driver passes the same tests. `dbrover-core` exposes
them as `dbrover_core::conformance` behind the `conformance` feature, which driver crates enable
in their dev-dependencies.

Implemented:
- `check_lifecycle`: server info, two pings, clean close.
- `check_connect_failure`: a bad config is a `DriverError::Connect`.
- `walk_catalog` (M2): walks the tree breadth-first (system nodes skipped) and checks that every
  child's path extends its parent's, labels aren't empty, and every node with
  `has_definition` returns a non-empty definition. It returns the visited nodes so each driver
  test can assert on its own fixture: a schema with one object of every kind.
- `check_invalid_path` (M2): nonsense paths are `DriverError::InvalidPath`, without touching the
  server. (This caught the Postgres driver trying to connect to a "database" named in a bogus path.)
- `check_children_pages` (N0a): paging through a level with `children_page` gives exactly what
  `children` lists, in its order and in the expected number of pages, and a filtered page holds
  exactly the nodes whose label contains the filter. For drivers that page a level themselves;
  SQLite runs it on 2,500 tables, and the engine's `explorer` tests do the same on PostgreSQL.
- `check_execution` (M3): DDL, a 1200-row insert with its count, paging through it in pages
  of at most 500, `NULL` vs empty text, abandoning a cursor halfway, and a syntax error's byte
  position. Uses SQL both engines accept and a temporary table.
- `check_cancel` (M3): a statement that would run far longer than 10 s (`pg_sleep(60)`, an
  endless recursive CTE) stops within 10 s of a cancel from another task, and the connection
  stays usable.

- `check_transactions` (M4): `transaction_status` follows `BEGIN`, `COMMIT` and `ROLLBACK`,
  committed rows stay and rolled-back ones don't, and a failure inside a transaction leaves it
  open (failed, on PostgreSQL) until it is rolled back.

- `check_validate` (SQL tools): preparing runs nothing (an accepted `INSERT` inserts no row),
  temporary tables are visible, unknown names and syntax errors are told apart, syntax errors
  carry their position, and no transaction is left open.

- `check_describe_table` (M5): an unquoted name resolves ignoring case, columns come in table
  order, each with a type and whether it is nullable, and the primary key in key order, a
  keyless table has an empty key, and views and missing tables are `None`.

Races hide in cancellation, so after changing it, run the cancel tests in a loop. A one-off
pass proved nothing in M3: the console's cancel test hung 2 runs in 5 before its fix.

Each driver crate runs the suite against its own backend:

- **SQLite**: a temporary database file, always runs.
- **Postgres**: the containers from `docker-compose.yml`. `postgres` (port 55432) is a plain
  server; `postgres-tls` (port 55443) refuses unencrypted connections, and its certificate is
  signed by the test authority in `docker/postgres-tls/` (`generate.sh` makes both), which the
  `verify-full` tests trust. Tests are skipped with a message when `DBROVER_TEST_POSTGRES_URL`
  or `DBROVER_TEST_POSTGRES_TLS_URL` is unset, so `cargo test` works offline.
- **MySQL and MariaDB**: the `mysql` (8.4, port 55306) and `mariadb` (11.4, port 55307)
  containers, both through the same tests, each skipped when its `DBROVER_TEST_MYSQL_URL` or
  `DBROVER_TEST_MARIADB_URL` is unset. Both make a self-signed certificate on first start, so
  the TLS tests need no fixture of their own. `check_describe_table` asks the server's
  `lower_case_table_names` whether unquoted names match ignoring case: on Linux they don't.
- **DuckDB**: in memory or a temporary file, always runs, like SQLite: DuckDB is inside the
  driver, compiled or, in CI, linked from DuckDB's prebuilt library (see
  [duckdb-prebuilt.md](duckdb-prebuilt.md)); a test checks either way that it is DuckDB 1.5.6
  with Parquet and JSON. It runs the suite with `Spelling::STANDARD`, and reading a Parquet file from an
  in-memory database is a test of its own.
- **ClickHouse**: the `clickhouse` container (25.8, HTTP on port 58123 and HTTPS on 58443,
  with the `postgres-tls` certificate), skipped when `DBROVER_TEST_CLICKHOUSE_URL` or
  `DBROVER_TEST_CLICKHOUSE_TLS_URL` is unset. It runs `check_execution`, `check_validate` and
  `check_cancel` with `Spelling::STANDARD`, but not `check_transactions` (ClickHouse has none
  to speak of) or `check_describe_table` (no key tells a table's rows apart, so results are
  never editable).
- **Redis and Valkey**: the `redis` container (Redis 8, port 56379, and TLS on 56443 with the
  `postgres-tls` certificate) and the `valkey` container (Valkey 9, port 56380), skipped when
  `DBROVER_TEST_REDIS_URL`, `DBROVER_TEST_REDIS_TLS_URL` or `DBROVER_TEST_VALKEY_URL` is unset.
  Each test runs on both servers, signing in as the ACL user `dbrover`. They run the parts of
  the suite that apply to an engine without SQL: `check_lifecycle`, `check_connect_failure`,
  `walk_catalog`, `check_invalid_path`, `check_lost` and `check_cancel`, the last with a
  `BLPOP` (ended by `CLIENT UNBLOCK`) and a script (ended by `SCRIPT KILL`). The servers let a
  script be killed after 100 ms rather than 5 s, and allow `DEBUG SLEEP`, for the command
  nothing stops. A test also checks that the command table `dbrover-language` ships classifies
  every command as each server's `COMMAND INFO` flags it, and prints the table's lines for any
  that differ. Dragonfly isn't in CI: it is tried by hand before a release.
- **MongoDB**: the `mongo` container (MongoDB 8 locally and 7.0 in CI, port 57017, with access control on and TLS on
  the same port, `preferTLS`, with the `postgres-tls` certificate), skipped when
  `DBROVER_TEST_MONGODB_URL` is unset. Each test works in a database of its own, dropped first.
  It runs `check_lifecycle`, `check_connect_failure`, `walk_catalog`, `check_invalid_path` and
  `check_cancel`, the last with a `find` and an aggregation that sleep in server-side
  JavaScript, ended by `killOp`. `check_lost` doesn't apply: the driver's pool opens new
  connections by itself, so a connection is never left lost. Besides: paging through 1,200
  documents, each method's result, every BSON type read back as written, TLS, a tunnel and a
  connection string. CI runs MongoDB 7.0 (`DBROVER_MONGO_TAG`): every MongoDB 8 refuses to
  start on Linux kernels 6.19 to 7.0.13, which GitHub's Ubuntu runners have in October 2026
  ([SERVER-121912](https://jira.mongodb.org/browse/SERVER-121912)). Move CI back to 8 once
  the runners' kernel is 7.0.14 or later.
- **Elasticsearch and OpenSearch**: the `elasticsearch` container (9.5, port 59200, with security
  on and HTTPS with the `postgres-tls` certificate) and the `opensearch` container (3.9, port
  59201, plain HTTP with its security plugin off), each a single node with a 512 MB heap,
  skipped when `DBROVER_TEST_ELASTICSEARCH_URL` and `DBROVER_TEST_OPENSEARCH_URL` are unset.
  Each test runs on every server that is set, in indices of its own made again first. They run
  `check_lifecycle`, `check_connect_failure`, `walk_catalog`, `check_invalid_path` and
  `check_cancel`, the last with a count whose script query loops a million times on each of
  60,000 documents. `check_lost` doesn't apply: HTTP keeps no session to lose. Besides: a
  search paged past 1,000 hits in its sort's order, a search's `size`, an aggregation as one
  document, `_sql` and `_plugins/_sql` as rows past their first page, `_cat` as documents,
  errors pointing into the body, TLS and an API key on Elasticsearch.
- **SQL Server**: the `sqlserver` container (2022, port 51433), skipped when
  `DBROVER_TEST_SQLSERVER_URL` is unset. Its image is x86-64 only: native on CI's Linux runner,
  emulated (slowly, and it can crash) by Docker on an Apple-silicon Mac. The image can't
  create a database on first start, so the compose health check creates `dbrover`. The suite
  runs with `Spelling::SQL_SERVER`: temporary tables are `#name`, and a transaction opens with
  `BEGIN TRANSACTION`, since `BEGIN` alone opens a block there. The server makes its own
  certificate, so the TLS tests need no fixture.

The SSH tunnel (`dbrover-ssh`) is tested against the `ssh` service (port 55222): OpenSSH on
Alpine, built from `docker/ssh/` with committed test keys (`generate.sh` makes them), which
signs in `dbrover` with a key or its password and forwards only to the `postgres` service. Its
tests skip when `DBROVER_TEST_SSH_URL` or `DBROVER_TEST_SSH_POSTGRES_URL` is unset. They fail
logins and abandon handshakes on purpose, so the server turns off OpenSSH's penalties for an
address that does that (`PerSourcePenalties`), which would otherwise refuse the next test's
connection.

Each network driver also connects through `conformance::forwarder`, a local port standing in
for a tunnel's end, with `Route::Tunnel`. The PostgreSQL and ClickHouse tests also check that
TLS is still verified against the server's own name: their certificate names `localhost`, so
the same server under another name is refused. The engine's `tests/ssh.rs` connects through the
`ssh` service itself, signing in with the test key or a prompted password.

Every server engine follows the same pattern: a service in `docker-compose.yml` and its URL in
`test-servers.env`, which CI reads too. To run everything locally:

```bash
docker compose up -d --wait
set -a; . ./test-servers.env; set +a
cargo test --workspace
```

Cancellation tests use `pg_sleep(10)` and a recursive CTE for SQLite, and assert the run ends in
well under the sleep time.

## Engine

The engine is tested headless (`dbrover-engine/tests/engine.rs` and `console.rs`, sharing
`tests/common`): send `Command`s, collect `Event`s, assert on the sequence. It uses a temp
directory for `data_sources.toml`, console files, `history.sqlite` and exports, and an
in-memory `SecretStore`. Tests that need PostgreSQL (a commit of a failed transaction, say)
skip without `DBROVER_TEST_POSTGRES_URL`, like the driver tests. Planned: a fake driver (in-memory, scripted responses) covers ordering cases that are hard to trigger
against a real server, such as a cancel arriving after the last row.

### Object helpers' DDL

- `dbrover_sql`'s DDL builder has golden tests per dialect (`ddl/tests.rs`): a table with every
  kind of constraint and names that need quoting, renames, truncates, drops and indexes, written
  out in full for each of the six dialects, and what each dialect refuses.
- `dbrover-engine/tests/ddl.rs` applies DDL through the engine: all or none on SQLite, the
  statements before a failure kept (and counted) on MySQL, refused on a read-only data source,
  and recorded in the history. Its round trip builds a parent and a child table from one
  `TableSpec` on every engine with a test server, describes the child, and compares its
  columns, nullability, default and primary key with the spec, then drops both. Copy DDL on
  PostgreSQL and SQL Server is checked by running what it copied after dropping the table.
- Modify table (E4): the builder's golden tests write each dialect's `ALTER TABLE` for column
  and constraint changes, renames that stay renames (and the same names without a rename,
  which drop and add), what each dialect refuses, SQLite's rebuild in full, and what makes a
  SQLite table unrebuildable. Each driver's `describe_constraints` is covered by the engine's
  round trip, which on every engine with a test server (SQLite and a DuckDB file too) creates
  a table with rows, loads it with `LoadTable`, renames, retypes, drops and adds columns and,
  where the engine can, adds a check and a foreign key, loads it again and compares, reads the
  rows back through a console, then takes the constraints off again. SQLite's rebuild has its
  own engine test, with foreign keys switched on: the rows, an index, a trigger, a view,
  `AUTOINCREMENT` and another table's cascading key survive, and a rebuild that breaks a key is
  undone. The app's kittest tests modify a SQLite table in place and by a rebuild through the
  dialog, and see a `STRICT` table refused.
- The builder's golden tests also write every view and schema statement, and `view_query` is
  checked against a definition as each engine shows it.
- The engine's view round trip creates a view on every engine with a test server, edits its
  query starting from what `view_query` finds in the loaded definition, copies its DDL,
  renames it where the engine can and drops it. A schema holding a table is created, listed
  and dropped (with `CASCADE`, `DROP DATABASE`, or the table first). MySQL's test user may
  not create databases, so its schemas are only in the golden tests. PostgreSQL's
  materialized views have a test of their own.
- Roles (E3): the builder's golden tests write every role statement, with a fixed verifier.
  `ScramVerifier` is checked against a reference SCRAM-SHA-256 computation in the test, which
  itself reproduces the exchange published in RFC 7677. The engine's role round trip runs on
  the PostgreSQL test server only: it creates a group and a login user, lists and describes
  them, copies the user's DDL, logs in as it with `TestConnection` (and is refused with a wrong
  password), alters, renames and drops it with the table it owns given away, then searches
  the history for the password and the verifier, and finds the `CREATE ROLE` recorded with
  `'<redacted>'`. Its roles carry part of the data source's id in their names, and the app
  tests' the process id, so parallel runs on the shared servers don't collide.
- The app's role tests: panel tests of the redacted SQL and the disabled Copy and Open in
  console, and of New user's two password fields; on the PostgreSQL server, one lists the
  roles and copies one's DDL, and one creates, edits, renames and drops a user through the
  dialogs, then searches every byte of `history.sqlite` (and its write-ahead log) for the
  password and for `SCRAM-SHA-256`.

### Power in the grid

- `dbrover_sql`'s row statements (`dml.rs`) and the database sort and filter (`arrange.rs`) have
  golden tests per dialect.
- `dbrover-engine/tests/grid/` is a module of the `console` test binary, not a binary of its own,
  so it adds no link step to CI. Its tests run on every engine with a test server, each in
  tables with the process id in their names so runs sharing the servers don't clash: a submit
  of deletes, updates and inserts built by `RowSql` (and one that fails and changes nothing),
  foreign keys both ways in two described tables, the sort and filter wrapper (ClickHouse
  too), an export of 1,200 rows in pages of 500 in both formats (ClickHouse too), and an
  import of 1,200 rows from CSV that succeeds and one that fails on a known line. SQLite alone
  covers a cancelled export, refusals on a read-only data source, and an import line with too
  few fields.
- The DuckDB driver checks that two connections to one file see each other's changes.
- The app's W5 UI tests are in `app/tests/grid.rs`, against SQLite.

### Exports

- `dbrover-export` has golden tests of every text format on awkward values, the SQL formats in
  every dialect, a SQLite round trip of `INSERT`s, and `.xlsx` files read back with `calamine`
  (each cell's type and value, the header, frozen panes and filter from the sheet's XML, and
  sheets past a full one, with a small sheet). The DuckDB driver's `parquet` tests read each
  column's type and value back through DuckDB, and the engine's `exports_every_row_on_every_engine`
  writes Parquet on every engine with a test server and reads its count and type back. It also
  exports a 1,200-row table with `ExportTable` on the explorer's connection (all but ClickHouse,
  whose tables aren't described); `cancels_a_table_export_and_runs_one_on_a_read_only_data_source`
  covers cancelling and a read-only data source on SQLite.
- The app's export tests are in `app/tests/grid.rs`: the export dialog's formats, options and
  memory, Copy as, Export data… on a table, and Run to file with a parameter. The table-driven typing rules are unit tests of
  `xlsx::typing`.
- `fills_a_whole_sheet_and_starts_another` writes a real sheet's worth of rows plus one; it
  takes a few seconds, so it is ignored by default:

  ```bash
  cargo test -p dbrover-export -- --ignored
  ```

### Files and parameters

- `dbrover_sql::parameters` has a corpus per dialect in `dbrover-sql/tests/parameters/`, read by
  a unit test (no binary of its own), and golden tests of the placeholders and literals each
  dialect gets.
- `dbrover-engine/tests/parameters/` is a module of the `console` test binary, like the grid's.
  It binds a number typed as text, a quote, a repeated parameter and `NULL` on every engine with
  a test server, in a table with the process id in its name; ClickHouse gets literals in a
  query of its own. SQLite alone covers the history's comment, a missing value and an export
  with parameters; PostgreSQL an error position mapped back.
- The engine test binary's `opens_saves_and_watches_a_sql_file` reads, writes and watches a
  file in a temporary folder.
- The app's W6 UI tests are in `app/tests/files.rs`, `parameters.rs` and `find.rs`, against
  SQLite and temporary files. Files changed on disk are noticed by a check every two seconds,
  so that test takes about four.

## Actions, snippets and themes (H1)

- `dbrover_core::SqlTemplate` has unit tests of every placeholder and problem, and of hostile
  names in the three kinds of quoting. `dbrover-store`'s `custom` module tests each file's format
  and every problem message.
- The engine test binary's `reloads_the_customisation_files_with_their_problems` reads them from
  a temporary folder; every start-up test now expects their three events after the settings.
- The app's UI tests are in `app/tests/actions.rs` and `customisation.rs`, against SQLite with
  a temporary config folder: actions from the explorer, the grid, a key and the Actions menu,
  View → Actions…, snippets in completion, and theme overrides.

## Command actions (H2)

- `dbrover_core::CommandTemplate` has unit tests of each command it refuses, the data source's
  fields, and a hostile table name staying one argument. `dbrover-store` tests every problem
  message of command actions, the selection written as JSON, CSV and TSV, the trusted commands'
  file, and that a data source export holding `[[action]]` tables imports none of them.
- The engine's `tests/programs.rs` runs real processes: `dbrover-test-program`, a binary of the
  engine crate (`src/bin/`), which cargo builds for the engine's tests and whose path they get
  from `CARGO_BIN_EXE_dbrover-test-program`. It behaves the same on Linux, macOS and Windows, so
  no test depends on `sh` or `cmd`. Its first argument picks what it does: `dump` writes back its
  arguments, environment, folder and stdin as JSON; `fail` exits with status 3; `wait` sleeps
  until killed; `flood` writes until killed. The tests cover the arguments and environment with
  a hostile table name, a search of everything the program got for the data source's saved
  password and SSH passphrase, a failing exit, the timeout and a stop, the 4 MB limit, trust
  (refused until trusted, again once changed, remembered after a restart) and a missing
  program. None waits for a fixed time: the timeout test's is 300 ms.
- The app's UI tests in `app/tests/commands.rs` run against `test_support::ScriptedPrograms`,
  a `ProgramRunner` that plays `echo`, `print`, `fail` and `hang` without starting processes:
  the question before a first run and Esc to cancel it, each `output`, a failure's window, Stop,
  ⌘. and a 200 ms timeout, and View → Actions…'s trusted column. The question is a modal that
  sizes itself over its first frames, so the tests `run()` before clicking **Run**, and pick the
  last **Run** on screen, since a console's toolbar has one too.

## MCP server

- `dbrover_sql::classify` has a corpus per dialect: reads, writes hidden in CTEs, `SELECT …
  INTO`, locking reads, DDL, transaction control and administration, and statements the parser
  can't read, which must never come out as reads.
- `dbrover-mcp/tests/stdio.rs` starts the server as another process (the test binary again, so
  it has no libtest harness: libtest would print onto the protocol's standard output) and
  drives it with rmcp's client over stdin and stdout. It runs against SQLite always, and
  against PostgreSQL, MySQL and MariaDB when their `DBROVER_TEST_*_URL` is set. It asks `query` for each
  `format` (CSV, Markdown, JSON Lines) and checks the text and the row limit. Each engine gets
  a table and a function that writes, both with a random suffix so runs sharing the servers
  don't clash. The MySQL one needs `log_bin_trust_function_creators`, which
  `docker-compose.yml` sets.
- `dbrover-app/tests/mcp_subcommand.rs` runs the real `dbrover mcp` binary with `HOME` pointed at
  a temporary folder: argument errors, nothing on stdout but the protocol, and one query.
  Unix only, since Windows ignores `HOME`.
- `dbrover-store`'s history test writes from two processes at once, the second being the test
  binary again.

## UI

- `AppState` reducers (applying an `Event` to state) are pure functions with plain unit tests.
- Widgets get smoke tests through `egui_kittest`, which drives egui headlessly. The app tests
  share their harness (a real engine, a temporary data source file, in-memory passwords) through
  `test_support`, and run at the app's default window size.
- `theme::tests::text_colours_meet_wcag_aa` checks every text colour against the backgrounds it
  is drawn on, in both themes, for WCAG AA contrast (4.5:1).

Hard-learned from the M4 UI tests: `egui_kittest` clicks at the centre of a widget's last
known rectangle. The app tests run in a window of the app's default size (`DEFAULT_WINDOW_SIZE`,
1440×900), because egui_kittest's own 800×600 is too narrow for the console with the bundled
fonts. A widget laid
out off-screen, or covered by another one that egui hit-tests first, simply doesn't get the
click, and the test only notices when the expected effect never happens. When a click seems
ignored, print the widget's rect and egui's `interaction_snapshot` before suspecting the code
under test. Modals are measured on their first frame and placed on the next, so run a frame
before clicking inside one.

`Harness::run` steps until egui stops asking to repaint at once, and gives up after four steps.
Two things in the app ask for that while a test still has work to wait for. Each answer from the
engine wakes the app, so after an action the engine is still answering, step until the effect
shows (`test_support::step_until`) instead of `run`. And egui takes a frame's predicted length
(a quarter of a second in kittest) off any delayed repaint, treating what's left of a shorter one
as immediate. So a wait the app measures on the wall clock, such as typing settling, asks for
its repaint through `repaint::request_repaint_after_pause`, which keeps it a delayed one.

### Screenshots

`screenshots::regenerate_screenshots` renders the app with egui_kittest's `wgpu` renderer and
writes every picture in the user manual, at 2x, into `book/src/images`:

```bash
cargo test -p dbrover-app regenerate_screenshots -- --ignored
```

`DBROVER_SCREENSHOTS_DIR` writes them somewhere else, and `DBROVER_SCREENSHOTS_THEMES=light,dark`
renders each scene in both themes (with a `-light` / `-dark` suffix), which is what design
reviews want. Regenerate the manual's images after a visible UI change, and run it with
`DBROVER_TEST_POSTGRES_URL` set so the workspace picture includes the PostgreSQL data source.

It is ignored by default because it needs a GPU adapter and rewrites files. Each scene starts a
fresh app against a SQLite fixture (a small shop database built from SQL in the test) and drives
it the way a user would: expanding nodes, opening a console, typing and running SQL. With
`DBROVER_TEST_POSTGRES_URL` set, a PostgreSQL data source joins the fixture. Engine logs reach the log
console through one global `tracing` subscriber whose layer is swapped per scene.

A scene that needs a test server names its variables (`needs`) and is skipped without them,
leaving its image as it was: `host-key-prompt` connects through the `ssh` service, and
`console-reconnect` ends its own PostgreSQL session. Every scene's log console shows the time,
so regenerating changes every image a little; commit only the images whose scene changed.

### Website pictures and clips

The front page's pictures and its hero clip come from the same machinery. A sibling ignored
test, `screenshots::site::record_site_media`, renders the feature tour's scenes in both themes
at a smaller window (1024 × 720 points, so the interface reads at the size the page shows it;
1280 wide for the query plan, whose columns don't fit in less), cropped to what each row of the
tour is about (a dialog, the console, or its plan), and records each
clip: the scene scripts the pointer, clicks and typing, and a frame is rendered after every
step, with a pointer and a ring around each click drawn in. Then `site/clips.sh` encodes them
into `site/media/`, which is committed:

```bash
cargo test -p dbrover-app record_site_media -- --ignored
```

```bash
site/clips.sh
```

The test writes into `target/site-media` (`DBROVER_SITE_MEDIA_DIR` elsewhere): each still as
`<scene>-<theme>.png`, and each clip as numbered frames in `<clip>-<theme>/` with a
`frames.ffconcat` giving each frame's duration. A clip's time is the script's, not the clock's:
waiting for the engine adds no frames, so a slow machine records the same clip.
`DBROVER_SITE_MEDIA=hero` (a comma-separated list of clips and scenes) renders only those.

`site/clips.sh` needs ffmpeg and ffprobe with libvpx-vp9 and libx264, and cwebp (`brew install
ffmpeg webp`). Each clip becomes a VP9 `.webm`, an H.264 `.mp4` for browsers without VP9, and a
`-poster.png` of its last frame, shown to visitors who ask for reduced motion; each still
becomes a `.webp`. The files keep their names, so regenerating replaces them in place: never
add a second version, since every byte stays in the repository's history. Keep each clip under
about 2 MB and each still under about 200 KB, and regenerate only when the UI visibly changes.
Like the screenshots, nothing here runs in CI.

## User manual

The manual in `book/` is an mdBook. `mdbook build book` builds it into `book/book/html` and,
through the `mdbook-linkcheck2` backend, fails on a link to a page or image that doesn't exist.
Install both with `cargo install mdbook --version 0.5.4` and
`cargo install mdbook-linkcheck2 --version 0.13.0`.

## Website

`site/build.sh` assembles the website (`site/` plus the manual) into `target/site`, writing a
version into the page's download links: `DBROVER_SITE_VERSION` if set (the Pages workflow sets it
to the latest published release), otherwise the workspace version from `Cargo.toml`. The
workflow's deployments also set `DBROVER_ANALYTICS_TOKEN`, which adds Cloudflare Web Analytics'
cookie-less beacon to every page; local builds and pull requests leave it out. Preview it with
`python3 -m http.server -d target/site`. The Pages workflow checks the landing page's links
with `lychee --offline`, which fails on a link to a file the build didn't produce.

## CI

GitHub Actions: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
on macOS and Linux, and the mdBook build with its link check. A separate Pages workflow builds
the website and manual on pull requests that touch them, and publishes them from `main`. On Linux the workflow starts every compose service and exports
the URLs in `test-servers.env`; adding an engine's container changes those two files and not
the workflow. GitHub can't run containers on macOS or Windows runners, so the server tests skip
there.
