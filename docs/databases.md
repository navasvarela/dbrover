# More databases

Status: **DB0 to DB3 built**; DB4 onwards planned. Order agreed on 2026-10-23; ClickHouse added
after DuckDB the same day.

DbRover speaks PostgreSQL and SQLite. This plan adds the next engines in order of how many
people would use them, and starts with the groundwork that makes each one cheaper than the
last.

## Which databases, and why in this order

Two popularity measures, as of October 2026:

| Database | DB-Engines rank (Sep 2026) | Stack Overflow survey 2025, share of developers using it |
| --- | --- | --- |
| Oracle | 1 | not in the top five |
| MySQL | 2 | 40.5% |
| SQL Server | 3 | 30.1% |
| PostgreSQL | 4 | 55.6% |
| SQLite | outside the top five | 37.5% |

DB-Engines scores search interest, job offers and discussion; the survey counts what developers
say they use. Further down, the two analytics engines disagree: ClickHouse is 26th on
DB-Engines and DuckDB 42nd, but 2.4% of developers use ClickHouse and 3.3% DuckDB. Sources:
[DB-Engines ranking](https://db-engines.com/en/ranking) (the September 2026 positions of
ClickHouse and DuckDB from
[Bytebase's snapshot](https://www.bytebase.com/database-periodic-table.md) of it),
[Stack Overflow Developer Survey 2025](https://survey.stackoverflow.co/2025/technology/).

The order, with the reason for each place:

1. **MySQL and MariaDB.** Second in both measures, and one driver serves both. Their catalog is
   the standard `information_schema`, close to what the PostgreSQL driver already reads.
2. **SQL Server.** Third or fourth in both measures. Its client is pure Rust, so the downloads
   stay self-contained; its catalog (`sys.*` views) and its SQL dialect are the most different
   from what DbRover knows.
3. **DuckDB.** Small in both rankings but much loved for analytics, and it can query CSV and
   Parquet files directly. It is a file like SQLite, so it fits the file picker; the price is a
   larger download, because its engine is compiled into DbRover.
4. **ClickHouse.** Ahead of DuckDB on DB-Engines, behind it among developers, and the
   column store teams run for analytics on large event and log data. It comes after DuckDB
   because fewer developers use it and, being a server, it is the larger job: a new protocol
   (HTTP), no transactions, and a different idea of updating rows.
5. **Oracle, later.** First on DB-Engines, but far less used by the developers DbRover is for,
   and its Rust client needs Oracle's own client library installed separately, which breaks the
   "download and run" install the [release track](releasing.md) just built. Planned, not
   scheduled.

Planned separately:

- **Redis, MongoDB and other non-SQL stores.** Until 2026-10-29 they were left out on purpose,
  because their data doesn't fit a SQL console with a grid of results. They now have a track of
  their own, [NoSQL databases](nosql.md), which starts with the groundwork that makes them fit:
  query languages beside SQL, nested documents in the grid, and safety checks for each language.
  Redis, Valkey and Dragonfly (N1) are built: the explorer, a viewer per key type, the
  console and MCP through a read-only ACL user. MongoDB (N2) is built: the shell's syntax in
  the console, collections in the explorer, documents edited in the grid, and MCP through a
  user with the `read` role. Elasticsearch and OpenSearch (N3) are
  built: Dev Tools requests in the console, hits as documents paged through a point in time,
  `_sql` as rows, indices and their fields in the explorer, hits edited in the grid, and MCP
  through a user with read-only privileges. Cassandra and ScyllaDB (N4) are built: CQL in the
  console, checked by the server as you type, keyspaces and their objects in the explorer, rows
  edited by their primary key, and MCP through a role with `SELECT` alone.
  through a user with read-only privileges. Amazon DynamoDB (N5) is built, ahead of the cloud
  track: AWS sign-in through a profile, PartiQL in the console with a guard that asks before
  a statement reads a whole table, tables and their indexes in the explorer, items edited in
  the grid, and MCP through a second AWS profile whose IAM policy allows only reading.

Left out on purpose:

- **PostgreSQL-compatible databases** (CockroachDB, Neon, Supabase, TimescaleDB, Amazon
  Aurora). They already connect through the PostgreSQL driver. Trying each and fixing what its
  catalog does differently is a small task of its own, best done after DB0 brings TLS, which
  hosted services require. It is C1 of [Cloud and big data](cloud-data.md); the
  cloud warehouses come later in the same track.

## What a new engine needs

The step-by-step version, for contributors, is [Adding a database](adding-a-database.md).

From the code as it stands (see [ADR 0002](adr/0002-native-drivers-behind-a-trait.md)), every
engine is:

- **A driver crate** implementing `Connection` from `dbrover-core`:
  - `execute` with a lazily read cursor (paging);
  - `children` and `definition` for the explorer;
  - `describe_table` for grid editing;
  - `validate` for the linter;
  - `transaction_status` for manual commit mode;
  - `cancel_handle` for ⌘.
- **A `ConnectionConfig` variant** in `dbrover-core`, with its `DriverKind`, and its form in the
  data source dialog (`data_source_form/`). Since DB0, a server engine's config holds a shared
  `ServerConfig` and its form a shared `ServerFields` section (host, port, database, user,
  password, TLS), so the engine adds only its own extras.
- **A SQL dialect** in `dbrover-sql`. It covers:
  - the lexer's quoting and comment rules, and the statement splitter;
  - the `sqlparser` dialect the linter and completion parse with;
  - the built-in function names for completion;
  - the `EXPLAIN` prefix;
  - the transaction keywords.
- **The conformance suite** (`dbrover-core/src/conformance.rs`) passing against a real server in
  CI.
- **App touches:**
  - the logo (`assets/icons/databases`) and its theme shades;
  - the manual's data source chapter, with regenerated screenshots;
  - the website's list of databases.

`Dialect`, `DriverKind` or `ConnectionConfig` is matched in about 30 source files across
`dbrover-sql`, `dbrover-engine`, `dbrover-store` and `dbrover-app`, plus their tests. The compiler
finds every one, but each new engine repeats the same small edits in all of them. DB0 gathers
those facts in one place first.

## Milestones

### DB0 — Groundwork

Changes that every later engine needs, made once against the two engines DbRover already has, so
each can be tested with no new driver involved:

- **One home for dialect facts.** Each `match dialect` that only looks a fact up (quote
  characters, comment syntax, the `EXPLAIN` prefix, function list, `sqlparser` dialect) moves
  into methods on `Dialect` in one module. A new dialect is then a new variant there plus its
  lexer rules, instead of edits in twenty-odd files. The enum stays closed, as ADR 0002 chose.
- **TLS.** Hosted MySQL, Azure SQL and most managed PostgreSQL require encrypted connections,
  and PostgreSQL TLS is already an M1 follow-up. Add TLS settings (off, preferred, required, and
  verify the certificate) to network data sources, built on rustls (no OpenSSL, per the release
  plan), and switch the PostgreSQL driver over first.
- **A shared form for network servers.** Host, port, database, user, password and TLS, reused
  by every server engine, with each engine adding only its own extras. The PostgreSQL form
  becomes its first user.
- **Conformance per engine in CI.** `docker-compose.yml` and the CI job grow one service per
  engine (each test still skips when its `DBROVER_TEST_*_URL` is unset), so adding an engine's
  container is the only CI change.

**Exit:** the PostgreSQL driver connects to a server that requires TLS, adding a dialect touches
one module plus the lexer, and the suite still passes on both engines.

### DB1 — MySQL and MariaDB

- **Client: `mysql_async`.** Native and asynchronous on tokio, which matches ADR 0002 (the
  sqlx route was rejected there). It streams rows, so paging works as for PostgreSQL.
- **Cancel:** MySQL has no in-protocol cancel. ⌘. opens a second connection and sends
  `KILL QUERY <connection id>`, the id read when the session started.
- **Catalog:** MySQL has no separate schema level. The root lists databases, each holding
  tables, views, routines, triggers and events, all from `information_schema`; definitions come
  from `SHOW CREATE …`, which also gives tables the `CREATE TABLE` PostgreSQL still lacks.
- **Dialect:**
  - `` `backtick` `` identifiers;
  - `#` comments, and `--` only before a space;
  - backslash escapes inside strings;
  - double quotes for strings unless `ANSI_QUOTES` is on;
  - the client-side `DELIMITER` command in scripts, so procedures in a script split correctly.
- **Transactions:** autocommit as on the other engines. DDL commits implicitly in MySQL, so
  manual commit mode warns before running DDL with a transaction open.
- **Explain:** `EXPLAIN FORMAT=TREE` on MySQL 8, plain `EXPLAIN` on MariaDB; the session tells
  them apart from the server version.
- **Validate:** preparing a statement (`COM_STMT_PREPARE`) checks names for data queries and
  changes without running them. DDL can't be prepared, so it gets the syntax check only.
- **CI:** MySQL 8.4 LTS and MariaDB 11 LTS containers, both through the same suite.

**Exit:** I connect to a MySQL and a MariaDB server, browse them, run and cancel queries, edit
a row in the grid, and the conformance suite passes on both in CI.

### DB2 — SQL Server

- **Client: `tiberius`**, pure Rust (the TDS protocol), with rustls for TLS. First task: check
  its maintenance status, and whether it can cancel a running query. The protocol has a cancel
  (attention) message; if the crate doesn't send one, the fallback is `KILL <session id>` from
  a second connection, which needs a permission most users lack, so cancel would be best
  effort.
- **Authentication:** SQL Server logins first. Windows integrated login and Microsoft Entra ID
  (Azure) tokens come later, each a choice in the form.
- **Catalog:** databases, then schemas, then tables, views, procedures, functions, sequences
  and synonyms, from the `sys.*` views; definitions from `OBJECT_DEFINITION`.
- **Dialect:**
  - `[bracketed]` identifiers and `N'…'` strings;
  - `GO` as a client-side batch separator, like `DELIMITER` for MySQL;
  - `TOP` instead of `LIMIT` in what DbRover itself generates.
- **Explain:** `SET SHOWPLAN_XML ON` runs in its own batch and returns the plan as XML. The
  cell viewer shows it pretty-printed until a plan view exists.
- **Validate:** `SET NOEXEC ON` compiles without running. SQL Server only resolves the names
  of tables that already exist when it compiles, so a misspelt table is caught but some
  misspelt columns may not be.
- **CI:** the official SQL Server 2022 Linux container, which runs on the Linux runners only,
  like PostgreSQL.

**Exit:** as for DB1, against SQL Server 2022, plus connecting to an Azure SQL database
(TLS required).

**As built**, where it differs from the plan above:

- **Cancel** is the attention message, sent by the statement's own task, since `tiberius`
  sends one. Its 0.13.0 release stopped reading at the end of the cancelled reply, though, and
  SQL Server 2022 acknowledges in a message of its own, which left the client a reply behind
  (each later query saw the previous one's result). DbRover pins `tiberius` to the commit of
  [tiberius-rs/tiberius#460](https://github.com/tiberius-rs/tiberius/pull/460), which fixes
  it, until a release has it. `KILL` was not an option: it ends the whole session, and the
  engine also cancels to discard unread rows.
- **Validate** asks `sp_describe_first_result_set` instead: `SET NOEXEC ON` defers resolving
  tables that don't exist, so it let a misspelt table through; the procedure catches unknown
  tables and columns alike, and sees the session's temporary tables.
- **Definitions** come from each database's `sys.sql_modules`, which holds what
  `OBJECT_DEFINITION` returns but can be read from another database.
- **`TOP`**: DbRover generates no row limit to translate. **Open data** runs `SELECT *` and
  pages through the result.
- **TLS:** `tiberius`'s rustls support compiles in aws-lc-rs next to ring, so the network
  drivers install ring as rustls's process-wide provider before connecting.
- **Azure SQL** is supported in code (TLS, the gateway's redirect), but hasn't been tried: there
  was no account to try it with.

### DB3 — DuckDB

- **Client: the `duckdb` crate**, run on `spawn_blocking` like SQLite. The `bundled` feature
  compiles DuckDB into DbRover: no install step for users, but a longer build and a larger
  binary. Measure both before committing (see the decisions below).
- **Data sources:** a `.duckdb` file through the file picker (with **New…**, as for SQLite),
  or an in-memory database for querying CSV, Parquet and JSON files.
- **Catalog:** databases (including attached ones), then schemas, tables, views, macros and
  sequences, from DuckDB's `duckdb_*()` table functions.
- **Dialect:** close to PostgreSQL's (DuckDB's parser derives from it), so it probably starts
  as the PostgreSQL rules plus DuckDB's function list, with `sqlparser`'s DuckDB dialect.
- **Cancel:** the crate exposes DuckDB's interrupt, the same pattern as SQLite's.
- **Explain:** `EXPLAIN` returns a text plan, shown as is.

**Exit:** I open a DuckDB file and query a Parquet file from an in-memory data source, and the
release packages stay within the size budget agreed below.

**As built**, where it differs from the plan above or settles what it left open:

- **Bundled, DuckDB 1.5.6.** The `duckdb` crate 1.10506.1, with `bundled`, `parquet` and
  `json` and no default features, which would download a prebuilt library while building.
  Measured on an Apple-silicon Mac, the stripped release binary went from 27.0 MB to 68.0 MB,
  and from 12.7 MB to 26.1 MB with `gzip -9`: the downloads grow by about 13 MB, inside the
  30 MB budget, and the release build takes 4 min 21 s instead of 1 min 44 s. The crate's next
  release, 1.20000.0, bundles a development build of DuckDB 2.0, so it is pinned with `~`
  until a release of the crate bundles a released 2.0.
- **Prebuilt in CI and releases** (since 2026-10-25). Compiling DuckDB took 10–14 minutes per
  OS on CI's runners, twice per macOS release. CI and the release workflow now link DuckDB's own
  libraries from its GitHub release instead, checksum-pinned: static on macOS and Linux, the
  DLL beside `dbrover.exe` on Windows. A plain `cargo build` still compiles it. The downloads
  grow by 2–6 MB more, mostly ICU and autocomplete, which DuckDB's libraries carry. See
  [DuckDB's prebuilt libraries](duckdb-prebuilt.md).
- **Results** are read as DuckDB streams them, in Arrow chunks, and formatted by Arrow, which
  knows every DuckDB type. What DuckDB's Arrow export loses is lost: a `HUGEINT` beyond 38
  digits, a `TIME WITH TIME ZONE`'s offset, and `BIT` arrives as bytes.
- **Statements without rows** answer with a status (one `Success` or `Count` column), which the
  driver reads to its end, since that is what runs the statement, and reports as no result.
- **The crate's gaps:** its `is_autocommit` always answers yes, so the transaction status
  compares two `txid_current()` calls, and the iterator `stream_arrow` returns panics on a
  failed fetch, so the driver steps the statement itself.
- **No savepoints in DuckDB:** grid edits inside a transaction the console opened are refused,
  with a message to commit or roll back first.
- **Macros** are listed under functions, with a definition rebuilt from their parameters and
  body.

### DB4 — ClickHouse

- **Client: the official `clickhouse` crate**, which speaks ClickHouse's HTTP interface on
  hyper, with rustls (its `rustls-tls-ring` feature, so no OpenSSL). Rows normally decode into
  Rust structs known at compile time, which a console can't have; `Query::fetch_bytes` instead
  streams any query in a named output format, so each statement runs with
  `FORMAT JSONCompactEachRowWithNamesAndTypes`: the first two lines are the column names and
  types, then one JSON array per row, read lazily, so paging works as on the other servers.
  The native TCP protocol (port 9000) has no maintained Rust client.
- **Connection:** HTTP is stateless, so a "connection" is a client plus a `session_id`
  setting, which keeps `USE`, `SET` and temporary tables between statements as a console
  expects.
- **TLS:** HTTPS on its own port (8443 by default), not an upgrade of the HTTP one, so
  `prefer` can't fall back on the same port. The form offers `disable`, `require` and
  `verify-full` for ClickHouse, with the port following the choice.
- **Cancel:** each statement runs with its own `query_id`, and ⌘. sends
  `KILL QUERY WHERE query_id = '…'` on a second request. Dropping the HTTP request isn't
  enough: ClickHouse only stops a query when its client disconnects if
  `cancel_http_readonly_queries_on_client_close` is on, and never stops writes that way.
- **Catalog:** databases, then tables, views, materialized views and dictionaries (told apart
  by `system.tables.engine`), columns from `system.columns`, and the database's own functions
  from `system.functions`. Definitions come from `SHOW CREATE`.
- **Dialect:**
  - `` `backtick` `` and `"double-quoted"` identifiers;
  - backslash escapes inside `'…'` strings;
  - `--` and `#` comments;
  - `sqlparser`'s ClickHouse dialect for the syntax check, which reads less of ClickHouse
    than of the others, so expect a longer known-gaps list.
- **Transactions:** none in practice (ClickHouse's are experimental), so
  `transaction_status` is always idle, and manual commit mode isn't offered for ClickHouse
  consoles.
- **Grid editing:** off at first. `ALTER TABLE … UPDATE` is an asynchronous mutation that
  rewrites data parts, nothing like a row edit; the lightweight `UPDATE` of recent servers
  is closer, and worth trying once it is generally available on the versions people run.
- **Explain:** `EXPLAIN` returns the plan as text rows, shown as is.
- **Validate:** `EXPLAIN PLAN` resolves tables and columns without reading data, for data
  queries.
- **CI:** the official `clickhouse/clickhouse-server` container, on Linux runners like
  PostgreSQL, through the same conformance suite. The suite's transaction check doesn't apply
  and is skipped for ClickHouse.

**Exit:** I connect to a ClickHouse server over HTTP and HTTPS, browse its databases and
tables, run a query over millions of rows and page through it, cancel a long one, and the
conformance suite (without transactions) passes in CI.

**As built**, where it differs from the plan above or settles what it left open:

- **TLS through `dbrover-tls`**, not the crate's `rustls-tls-*` features, which trust only the
  public authorities and so can't do `verify-full` with a CA file. The rustls configuration the
  PostgreSQL driver built moved into a crate of its own, and the ClickHouse driver hands it to
  `hyper-rustls`. `prefer` is accepted in a saved file and means plain HTTP, as `disable` does.
- **The session's database is set once, with `USE`**, when the connection opens. The client
  would otherwise send `database` with every request, which undoes the user's own `USE`. A
  session idle for an hour ends on the server, and the next request starts a new one in the
  user's default database, without its temporary tables.
- **Values** stay as ClickHouse writes them in JSON: a `UInt64` or `Int128` keeps every digit
  (ClickHouse quotes 64-bit and larger integers), decimals keep their trailing zeros
  (`output_format_decimal_trailing_zeros`), and arrays, maps and tuples show as JSON. An error
  partway through a result arrives as plain text (`http_write_exception_in_output_format=0`),
  which the client turns into an error rather than a last row.
- **Cancel** is `KILL QUERY … SYNC`, which returns once the query has stopped, so the session
  is free for the next statement. A result abandoned with rows unread is stopped the same way,
  after its response is dropped: a query blocked writing rows nobody reads never sees a kill.
- **Validate** parses every statement with `EXPLAIN AST`, so a syntax error in an `INSERT` or
  `ALTER` is caught too, and then resolves names with `EXPLAIN PLAN` for queries only, the one
  kind ClickHouse plans.
- **Catalog:** functions are left out, since a ClickHouse function (built in or made with
  `CREATE FUNCTION`) belongs to the server, not to a database. Indexes are the data-skipping
  indexes; the primary key is the sort order and shows in the definition, which comes from
  `system.tables.create_table_query` rather than `SHOW CREATE`. The `.inner` table of a
  materialized view without a `TO` table is a system object.
- **Grid editing** stays off: `Dialect::read_only_results` gives the reason, and the console
  doesn't describe the table at all.
- **Conformance:** the suite's error-position check now uses `WHERE id = = 1` (ClickHouse reads
  `WHERE WHERE id` as a column named `WHERE`), and its accepted `DELETE` has a `WHERE`, which
  ClickHouse requires. `check_describe_table` is skipped along with `check_transactions`.
- **A client bug worked around:** `BytesCursor`'s `AsyncRead` can fill part of a buffer and
  then return `Pending`, which tokio's `read_to_end` panics on, so whole responses are read
  with its own `collect` and results line by line through `AsyncBufRead`, which is sound.
- **Size:** measured on an Apple-silicon Mac, the stripped release binary went from 68.0 MB to
  69.1 MB, and from 26.1 MB to 26.5 MB with `gzip -9`: the downloads grow by about half a
  megabyte, the client being little more than hyper, which DbRover already had.

### DB5 — Oracle (planned, not scheduled)

Waits until after R2 (signed installs).

- **Client:** the `oracle` crate needs Oracle Instant Client on the user's machine. It loads the
  library at runtime, so DbRover can still ship without it: an Oracle data source checks for it
  and explains how to install it. Whether DbRover may bundle Instant Client depends on Oracle's
  redistribution terms, to be read then.
- **Logo:** Oracle asked Simple Icons to remove its logo, so DbRover uses a neutral database
  glyph unless Oracle's brand rules allow otherwise.
- **CI:** the Oracle Database Free container is several gigabytes, so its tests may run
  nightly rather than on every pull request.

## Decisions

Proposed defaults; each is confirmed when its milestone starts.

| Question | Proposed | Consequence |
| --- | --- | --- |
| One MySQL driver or two | **One**, with MariaDB differences keyed off the server version. | A single crate and data source type; the form says "MySQL / MariaDB". |
| DuckDB bundled or system library | **Bundled**, if it adds less than about 30 MB to the downloads. | No install step. Otherwise, link a system `libduckdb`, which Linux packages can depend on and macOS users would install with Homebrew. |
| SQL Server authentication | **SQL Server logins first**, Entra ID next, Windows integrated last. | Covers Docker, most on-premises servers and Azure SQL with a SQL login. |
| ClickHouse protocol | **HTTP**, through the official `clickhouse` crate. | Pure Rust, maintained by ClickHouse, works through proxies and ClickHouse Cloud. Slightly more overhead per statement than the native protocol, which no maintained Rust client speaks. |
| Oracle | **After R2**, not scheduled. | Nothing built for Oracle until the release track is done. |
| Logos for MySQL, MariaDB, SQL Server, DuckDB, ClickHouse | From Simple Icons where it still has them, as for the current two; a neutral glyph otherwise. | The `assets/icons/databases` README records each one's source. |

## Risks

- **Dialect drift.** The lexer is hand-written per dialect. Each engine adds rules (MySQL's
  backslash escapes, SQL Server's `GO`) that can break statement splitting in ways tests miss.
  Each dialect gets a corpus of real statements that must split, format and lint identically
  before and after.
- **Cancel that can't be guaranteed.** On MySQL, SQL Server and ClickHouse, cancel can need a
  second connection (or request) and a permission. When it fails, the console says so instead of spinning.
- **CI time.** Each server container adds a minute or two to the Linux job. If the job grows
  past about 15 minutes, engines other than PostgreSQL move to a separate job that runs in
  parallel.
