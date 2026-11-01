# NoSQL databases

Status: **N0a, N0b and N1 built** (2026-10-30), **N2 and N3 built** (2026-10-31) and **N4
built** (2026-11-01), in the layers listed under
[N0a](#n0a--groundwork-languages-safety-and-the-explorer), [N0b](#n0b--groundwork-documents),
[N1](#n1--redis-valkey-and-dragonfly), [N2](#n2--mongodb),
[N3](#n3--elasticsearch-and-opensearch) and [N4](#n4--cassandra-and-scylladb). W7 (exports) has
landed.
[N5](#n5--dynamodb), DynamoDB, was planned and built on 2026-11-01, beside N4.

DbRover speaks seven SQL engines. Many of the teams that use them keep a Redis beside their
PostgreSQL, documents in MongoDB, or logs in Elasticsearch, and today they open another tool for
those. This plan adds the most used non-SQL stores to the same explorer, console and grid, with
the same safety levels, history and MCP server. It starts with groundwork that every one of them
needs, because DbRover's console, grid and safety checks all assume SQL today.

[More databases](databases.md) left Redis and MongoDB out on purpose until 2026-10-29: their data
doesn't fit a SQL console with a grid of results. This plan is how it can be made to fit.

## Which databases, and why in this order

Two popularity measures, as of October 2026:

| Database | Kind | DB-Engines rank (Sep 2026) | Stack Overflow survey 2025, share of developers using it |
| --- | --- | --- | --- |
| MongoDB | documents | 5 | 24% |
| Redis | keys and values | 8 | 28% |
| Elasticsearch | search | 11 | 16.7% |
| Apache Cassandra | wide columns | 12 | 2.9% |
| Amazon DynamoDB | keys and documents, cloud only | 18 | 9.8% |
| Neo4j | graph | 20 | 2.6% |
| InfluxDB | time series | 29 | 3.7% |
| OpenSearch | search | 30 | not in the survey's list |
| Couchbase | documents | 45 | not in the survey's list |
| ScyllaDB | wide columns | 65 | not in the survey's list |
| Valkey | keys and values | 102 | 2.4% |

Sources: [Bytebase's snapshot](https://www.bytebase.com/database-periodic-table.md) of the
[DB-Engines ranking](https://db-engines.com/en/ranking) (taken on 2026-09-10), and the
[Stack Overflow Developer Survey 2025](https://survey.stackoverflow.co/2025/technology/).

The order, with the reason for each place:

1. **Redis, with Valkey and Dragonfly.** The most used non-SQL store among developers, and the
   cheapest to add after the groundwork. Its values are flat (strings, hashes, lists, sets,
   sorted sets), so they fit the grid DbRover has; and the server says which of its commands
   write, so the safety checks have something exact to go on. Valkey (the Linux Foundation's
   fork) and Dragonfly speak the same protocol, so one driver serves all three.
2. **MongoDB.** Fifth on DB-Engines and close behind Redis in the survey. It needs the larger
   half of the groundwork: results that are documents, nested and without fixed columns.
3. **Elasticsearch and OpenSearch.** Far more used than Cassandra (16.7% against 2.9%), and
   they return JSON documents too, so they reuse what MongoDB builds. OpenSearch is the fork
   Amazon's managed service runs; one driver serves both.
4. **Cassandra and ScyllaDB.** CQL looks like SQL and returns rows, so it fits the console and
   grid with the fewest changes. It comes after Elasticsearch because few developers use it.

5. **Amazon DynamoDB.** Planned for the [cloud track](cloud-data.md) after C0 until
   2026-11-01, when it was moved ahead of it: it exists only on AWS, signs in with an AWS
   account, and charges for what a query reads, so N5 brings C0's AWS sign-in and a cost guard
   with it. It reuses this track's document results, and its PartiQL fits the console.

Later:

- **Neo4j** returns nodes and relationships, which only a graph view shows well. That view could
  share ER1's canvas (pan, zoom, boxes and lines), so Neo4j waits for ER1.

Left out for now:

- **InfluxDB.** Its current major version, InfluxDB 3, is queried in SQL (and InfluxQL), so it
  would be one more SQL engine rather than a NoSQL one. It is small in both measures, and time
  series want charts first (W12).
- **Couchbase.** 45th on DB-Engines and not in the survey's list. Its Rust SDK reached 1.0 in
  March 2026, and its query language (SQL++) is close to SQL, so it could follow MongoDB's
  documents later, if people ask.

## What the shared code assumes today

Every engine DbRover has speaks SQL, and the code says so in many places:

- **Every driver has a SQL dialect.** `Dialect::from(DriverKind)` is how the console, the engine,
  the MCP server and the object dialogs learn what they are talking to: about twenty source files
  in `dbrover-sql`, `dbrover-engine`, `dbrover-app` and `dbrover-mcp`. Splitting a script,
  highlighting, completion, formatting, the linter, explain and parameters are all SQL.
- **Results are text with fixed columns** ([ADR 0004](adr/0004-results-as-text.md)). A cell is
  `Option<String>`, and a statement's columns are known before its first row
  (`ResultCursor::columns`, `Event::StatementStarted`). A MongoDB result has neither: each
  document has its own fields, and a field can hold an object or an array. The cell viewer
  pretty-prints JSON, but there is no way to browse a nested value or turn its fields into
  columns.
- **Safety rests on a SQL parser.** `dbrover_sql::classify` parses each statement with
  `sqlparser` and calls it a read, a write, a schema change, transaction control or
  administration. Three things depend on it: the read-only safety level
  (`dbrover-engine/src/read_only.rs`), the MCP server's permissions (`dbrover-mcp`) and the
  warnings before a risky statement (`dbrover-app/src/hazard_dialog.rs`). A statement it can't
  parse is refused. Behind the parser, the database's own read-only transaction (PostgreSQL,
  MySQL, MariaDB) or read-only connection (SQLite) catches what it misses. None of the NoSQL
  engines here has a read-only session; only a user with read-only grants is enforced by the
  database.
- **The explorer is schemas and tables.** `Connection::children` returns one level of the tree
  at a time, all of it at once. A Redis database can hold millions of keys, which no tree can
  load whole.
- **Editing and generated statements are SQL.** Grid edits become `UPDATE`s (`dbrover-sql`'s
  `dml`), **Open data** runs a `SELECT`, the object helpers write DDL, CSV import writes
  `INSERT`s, and W7 adds SQL `INSERT` and `UPDATE` export formats.

## Goals

- **Browse, query and edit** Redis, MongoDB, Elasticsearch, OpenSearch, Cassandra and ScyllaDB in
  the explorer, console and grid DbRover already has, with each engine's own query language in
  the console.
- **Keep every guarantee:** ⌘. cancels, results page, safety levels hold, every command goes to
  the history, and the MCP server never runs a write a project doesn't allow.
- **Nested results that are easy to read:** documents flattened into columns in the grid, and a
  tree for the whole document. SQL engines' JSON columns get the tree too.
- **Nothing to install:** each client is pure Rust and uses rustls, as the SQL engines' do.

Not goals: monitoring, profiling, slow-query analysis, cluster and shard administration, or
backups. The vendors' own tools (RedisInsight, MongoDB Compass, Kibana and OpenSearch
Dashboards) do those well. DbRover browses, queries and edits.

## When to start

Nothing here was to be built until W7's changes to the grid and results landed, because N0b
changes the same code: what a result holds, how the grid draws it, and how exports read it. W7
landed on 2026-10-30, and N0a started the same day.

## Milestones

### N0a — Groundwork: languages, safety and the explorer

Changes every non-SQL engine needs, made against the SQL engines DbRover already has, so each can
be tested with no new driver:

- **Query languages beside SQL.** A closed `QueryLanguage` enum replaces `Dialect` as what a
  driver speaks: `Sql(Dialect)` for the seven engines, and one variant per new language as each
  engine arrives. Each language provides:
  - splitting a script into commands, and the command at the cursor (⌘↵);
  - highlighting tokens;
  - completion words;
  - classifying a command (below);
  - optionally a formatter, an explain prefix and a local syntax check.

  Features with no meaning outside SQL (formatting with SQL rules, parameters, the object
  helpers, CSV import, SQL export formats) ask the language for its `Dialect` and are hidden when
  it has none, rather than each checking the engine. Same rule as DB0: one place to look a fact
  up, and the compiler pointing at every `match` when a language is added.
- **Every language classifies its commands.** `StatementKind` (read, write, schema,
  transaction, admin) stays the one vocabulary for the read-only level, MCP permissions and
  the warnings, and each language fills it in its own way: SQL with `sqlparser`, as now; Redis
  from the server's command flags; MongoDB from a table of shell methods and pipeline stages;
  Elasticsearch from the request's method and path. The rule that an unclassified command is
  refused (unless the project allows `admin`) holds for every language. Each language gets a
  classification corpus, as each SQL dialect has in `crates/dbrover-sql/tests/classify/`.
- **Warnings per language.** The "`UPDATE` without `WHERE`" check generalises to "this command
  may touch everything": `FLUSHALL`, `FLUSHDB` and `KEYS` (which blocks the server while it
  reads every key) on Redis; `deleteMany({})` and `updateMany({}, …)` on MongoDB;
  `_delete_by_query` with `match_all` on Elasticsearch. Each language lists its own.
- **The database's guard where there is no read-only mode.** For an engine without a read-only
  transaction, a read-only data source and an MCP project rely on the command check plus a
  login with read-only grants. The dialog says so, as W11 plans for SQL Server: the console's
  read-only level refuses writes with the check alone, and the MCP server serves such a data
  source only when it has an MCP login.
- **An explorer that pages.** `children` can return part of a level with a way to ask for the
  next part, and can take a filter the server applies. The tree shows **Load more…** at the end
  of a partial level, and the explorer's filter box passes its text to the server when the level
  is partial. Redis keys need it first; a SQL Server or PostgreSQL schema with tens of thousands
  of tables benefits too.
- **Node kinds for what isn't a table:** `Key` (with its Redis type), `KeyPrefix` (a group of
  keys sharing a prefix), `Collection`, `SearchIndex`, `Alias`, `DataStream` and `Keyspace`,
  added to `dbrover-core/src/catalog.rs` as engines arrive, with their icons and the actions
  each offers. **Open data** opens what fits the node: a key viewer for a key, a `find()` for a
  collection.

**Exit:** every SQL feature works as before through `QueryLanguage::Sql`, with its tests
unchanged; a PostgreSQL schema of 20,000 tables loads in the explorer 1,000 at a time, with the
filter applied by the server; and a test language, built only for the tests, has its commands
split, highlighted, classified and refused on a read-only data source.

#### Layers

Each layer is one pull request in a stack, useful and tested on its own. None adds a driver; each
is exercised by the SQL engines and by the test language.

- **N0a·1 — Plan:** this list, the decisions N0a takes (in [Decisions](#decisions)), and the
  roadmap's status.
- **N0a·2 — Query languages** (built): a new crate, `dbrover-language`, holds `QueryLanguage`:
  `Sql(Dialect)` for the seven engines, and `Test`, a language of one command per line built
  only for the tests, behind the crate's `test-language` feature. Each language splits a script
  into commands, finds the command at the cursor, yields tokens to highlight, lists completion
  words, classifies a command into `StatementKind` (and names its schemas, for the MCP server's
  allow-list), and says which `Dialect` it has, if any. `QueryLanguage::from(DriverKind)`
  replaces `Dialect::from(DriverKind)` everywhere. The console keeps a language rather than a
  dialect, and runs, highlights and completes through it; the read-only check and the MCP
  server classify through it. Formatting, the linter, parameters, explain, manual commit mode,
  the object helpers, CSV import, **Open data**, and what the grid does with a result's rows
  (edits, sorting in the database, copying and exporting) ask for the dialect and are hidden or
  refused without one. The test language has a classification corpus, as each SQL dialect has,
  and console tests that split, highlight and complete its commands.
- **N0a·3 — Safety for every language** (built): the warning before a risky command becomes "this
  command may touch everything", listed by each language (`UPDATE` and `DELETE` without `WHERE`
  for SQL). An engine without a read-only session is guarded by its login: the console's
  read-only level refuses writes with the check alone and the dialog says to use a login with
  read-only grants, and the MCP server serves such a data source for reading when, and only when,
  it has an MCP login. SQL Server and ClickHouse are the first to use it; `execute` is refused on
  them, since DbRover has no transaction to run changes in there. Tests: the test language
  refused by the read-only check and planned by the MCP server through its own
  classification, its warnings, and, on the SQL Server test server, MCP serving a data source
  only through its MCP login and reading as that login.
- **N0a·4 — An explorer that pages** (built): `Connection::children_page` returns part of a
  level, with a token for the next part, and takes a filter the server applies; its default
  lists the whole level with `children` and filters it in DbRover. PostgreSQL and SQLite page
  the tables, views, materialized views and sequences of a schema 1,000 at a time, by name
  (keyset paging), with the filter as a case-insensitive `ILIKE` (`LIKE` on SQLite). The tree
  shows **Load more…** at the end of a partial level, and the explorer's filter box asks the
  server again for a partial level once typing pauses for 300 ms; a level loaded whole is still
  filtered by the explorer alone. Tests: a conformance check that the pages add up to the whole
  level and the server's filter matches DbRover's (SQLite, 2,500 tables); engine tests on the
  PostgreSQL test server, 2,500 tables in three pages and the filter, and an ignored one with
  20,000; and a kittest on SQLite that finds a table not yet loaded through the filter, then
  loads more. SQLite pages too, beyond the plan, so the explorer is tested end to end without a
  server. The MCP server's `list_objects` still lists a level whole; N1 pages it for keys.

The manual changes in the layer that changes what a user sees: N0a·3 (read-only data sources and
MCP logins on SQL Server and ClickHouse) and N0a·4 (**Load more…** and the filter).

### N0b — Groundwork: documents

Results that are documents rather than rows, for MongoDB, Elasticsearch and OpenSearch, Redis's
JSON values, and the JSON columns SQL engines already return. Can be built alongside N1.

- **[ADR 0006](adr/0006-documents-as-extended-json-text.md), documents as JSON text,**
  extending ADR 0004 rather than replacing it:
  - A result is either **rows** (today) or **documents**. A document travels as text, one
    Extended JSON string per document (MongoDB's JSON form for every BSON type), so, as with
    ADR 0004, every type shows correctly with no decoding code for each.
  - The **canonical** form is kept, which records each value's type (`{"$numberLong": "7"}`),
    so an edited document goes back with its types unchanged. It is shown the way the shell
    shows it: `ObjectId("…")`, `ISODate("…")`, `NumberLong(7)`.
  - ADR 0004 stays accepted, with a line pointing to 0006.
- **The grid flattens documents into columns:**
  - the columns are the top-level fields of the documents fetched so far, `_id` first and the
    rest in the order they first appear; a field first seen on a later page adds a column at
    the right;
  - a missing field shows as empty and greyed, which is different from `null`;
  - an object or array shows compactly (`{3 fields}`, `[12 items]`), and **Expand column**
    turns an object's fields into columns of their own (`address.city`, `address.zip`);
  - past 200 columns, the rest stay in the tree, and the grid says how many there are.
- **A tree view of the whole document,** in a tab beside the grid (**Grid**, **Tree**, **JSON**):
  fields folded and unfolded, each value with its type, and copying a value, a path
  (`items[2].sku`) or the whole document. The cell viewer gains the same tree for any JSON value,
  so a PostgreSQL `jsonb`, MySQL `JSON` or SQLite JSON text column can be browsed too.
- **Exports** (W7): JSON and JSON Lines write the documents themselves; CSV, TSV, Excel,
  Markdown and HTML write the flattened columns; the SQL formats aren't offered for documents.
  Copying and exporting the rows of a language without a SQL dialect, which N0a leaves hidden,
  come with them.
- **Paging** is as for rows: 500 documents a page through `FetchMore`.

**Exit:** a PostgreSQL query returning a `jsonb` column opens each value as a tree, and a
result of documents from a test cursor shows in the grid with a column added by a later page,
an expanded column, missing fields told apart from `null`, and a JSON Lines export that reads
back to the same documents.

#### Layers

Each layer is one pull request in a stack, useful and tested on its own, as N0a's were. None
adds a driver: the SQL engines' JSON columns and results of documents made up in the tests
exercise each.

- **N0b·1 — Plan:** this list, [ADR 0006](adr/0006-documents-as-extended-json-text.md), the
  decisions N0b takes (in [Decisions](#decisions)), and the roadmap's status.
- **N0b·2 — A tree for JSON values** (built): a new crate, `dbrover-document`, with no engine in it,
  reads JSON text: its own reader, which keeps an object's fields in the order written and each
  number as written. It knows Extended JSON's types and shows each as the shell does
  (`ObjectId("…")`, `ISODate("…")`, `NumberLong(7)`), names each value's type, and writes a
  value's path (`items[2].sku`). The cell viewer shows a JSON object or array as a tree, beside
  the text it shows today: fields folded and unfolded, each value with its type, and **Copy
  value**, **Copy path** and **Copy all** on each. That is the tree for PostgreSQL `json` and
  `jsonb`, MySQL `JSON`, SQLite JSON text, and DuckDB and ClickHouse `JSON`, and the one Redis's
  JSON strings and nested replies will use in N1. Tests: the reader (order, numbers, escapes,
  depth, errors), each Extended JSON type and plain JSON beside it, paths, and a kittest on
  SQLite that opens a JSON value from a query as a tree and copies a path.
- **N0b·3 — Documents in the grid** (built): a result's shape, rows or documents, from the cursor
  (`ResultCursor::shape`) to the console (`Event::StatementStarted`), with each document a row
  of one cell, as ADR 0006 says. The console flattens the documents into columns as the
  milestone describes: `_id` first, a column added at the right by a later page, a missing
  field greyed and told apart from `null`, objects and arrays shown compactly, **Expand
  column** on an object's column, and at most 200 columns, with a note of how many more the
  tree holds. A **Grid**, **Tree** and **JSON** switch above a result of documents shows the
  selected row's document whole. Tests: the flattening (order, later pages, missing and `null`,
  expanding, the cap) in `dbrover-document`, and a console fed a result of documents in two
  pages, as the engine sends a cursor's, in the test language.
- **N0b·4 — Copying and exporting documents and other languages** (built): what N0a left hidden for a
  language without a SQL dialect: **Copy as** and **Export…** of its rows, in every format but
  the SQL ones. For documents, JSON and JSON Lines write the documents themselves, as they came;
  CSV, TSV, Excel, Markdown, HTML and Parquet write the flattened columns. Copying JSON or JSON
  Lines from a selection copies its rows' documents whole. A result of documents exports what
  is loaded; **All rows** and **Run to file** wait for N2 (see [Decisions](#decisions)), and
  Run to file stays SQL's until a language without SQL has a driver. Tests: a JSON Lines
  export of documents that reads back to the same documents, a CSV of the flattened columns,
  and the test language's rows copied and exported without the SQL formats.

The manual changes in N0b·2, the one layer whose change a user sees with today's engines: the
cell viewer's tree. N0b·3 and N0b·4 change what a user sees only once an engine without SQL
arrives, and its milestone's manual chapter describes them.

### N1 — Redis, Valkey and Dragonfly

- **Client: the [`redis`](https://crates.io/crates/redis) crate** (1.8, October 2026), on
  tokio, with rustls through `tokio-rustls-comp` and no crypto provider of its own, so it uses
  ring like the other drivers. It is a community project (BSD-3-Clause) that releases every few
  weeks, and its CI runs against Redis 6 and later and Valkey 7 and later. Redis Inc lists it as
  best-effort rather than supported. `fred`, the alternative, has had no release since
  February 2025; Valkey's own `valkey-glide` has a Rust client, but it is pre-1.0 and tested on
  Linux only. Its 2.0 is in release candidates; take whichever is released when N1 starts.
- **Data source:** the shared server form (port 6379), with the user optional (ACL users; the
  `default` user otherwise), a password, the database number (0 by default) and TLS. SSH tunnels
  work as for the others. Test connection reports Redis, Valkey or Dragonfly and the version,
  from `INFO server`.
- **Explorer:**
  - the databases that hold keys, with their key counts, from `INFO keyspace`;
  - under each, keys grouped into a tree by a separator (`:` by default, set per data source),
    so `user:42:cart` sits under `user` › `42`;
  - keys are read with `SCAN` in pages of 1,000 (N0a's paging), never with `KEYS`; the filter
    box becomes `SCAN`'s `MATCH` pattern, and a type filter its `TYPE` option;
  - each key shows its type's icon, and its TTL when it has one.
- **A viewer per type**, opened by **Open data** on a key, in a tab like a table's data:

  | Type | Shown as | Edits |
  | --- | --- | --- |
  | String | Text; a tree when it parses as JSON (N0b); hex when it isn't UTF-8 | Set the value, keeping the TTL |
  | Hash | A grid of field and value, read with `HSCAN` | Change, add and delete fields |
  | List | A grid of index and value, read with `LRANGE` in pages | Change an item, push, remove |
  | Set | A grid of members, read with `SSCAN` | Add and remove members |
  | Sorted set | A grid of member and score, read with `ZRANGE … WITHSCORES` | Change a score, add, remove |
  | Stream | A grid of entry id and fields, newest first (`XREVRANGE`) | Add an entry, delete one |
  | JSON (the JSON module) | A tree (N0b), from `JSON.GET` | Set a path's value |
  | Anything else | Its type name, size and TTL | None |

  Every viewer's header has the key's TTL (set it, or remove it with `PERSIST`), its memory use
  where the server reports it, **Rename…** and **Delete…**, which asks first. An edit is one
  command, shown in the log and recorded in the history, and runs only after the safety level
  allows it.
- **Console:** one command per line, quoted as `redis-cli` reads them. Completion offers
  command names and their arguments from `COMMAND DOCS`, which include the server's modules.
  Replies show by shape: a list as one column, a map (RESP3) as field and value, a number or
  text as a single cell, and nested replies as a tree (N0b; indented text until it lands).
- **Classification** from the server's own command flags: each command in `COMMAND INFO`
  says whether it is `readonly`, `write`, `admin` or `dangerous`. A table generated from those
  flags ships with DbRover, so the check works before connecting; the session adds modules'
  commands from the server's answer. `MULTI`, `EXEC` and `DISCARD` are transaction control.
  `EVAL` and `FCALL` are writes; `EVAL_RO` and `FCALL_RO` are reads. `SELECT`, `CONFIG`,
  `CLIENT`, `FLUSHALL` and `SHUTDOWN` are admin.
- **Cancel:** Redis runs one command at a time and can't stop most of them once started; a
  command over a large key or `KEYS` blocks until done. So ⌘.:
  - unblocks a blocking command (`BLPOP`, `XREAD BLOCK`) with `CLIENT UNBLOCK` from a second
    connection;
  - stops a script that hasn't written yet with `SCRIPT KILL` or `FUNCTION KILL`;
  - otherwise stops waiting, closes the console's connection and opens a new one, and says the
    server may still be finishing the command.

  This is the limit of the server, not the client, and the manual says so.
- **No transactions, explain or validation.** `transaction_status` is always idle and manual
  commit mode isn't offered: `MULTI` queues commands rather than opening a transaction that can
  be rolled back. There is nothing to explain, and `validate` checks each command's argument
  count against `COMMAND INFO` without running anything.
- **Safety and MCP:** a read-only data source refuses commands the flags call writes, and the
  dialog suggests an ACL user with only `+@read`. The MCP server serves a Redis data source only
  with an MCP login, for example a user made with `ACL SETUSER mcp on >… ~* +@read`. Its
  `query` tool takes a command; `list_objects` pages keys.
- **Dragonfly:** `SCAN`, `TYPE` and `TTL` work as on Redis; `OBJECT` and `SCRIPT KILL` don't,
  and `MEMORY USAGE` partly. The viewers leave out what the server doesn't answer.
  ([Dragonfly's command list](https://www.dragonflydb.io/docs/command-reference/compatibility))
- **CI:** `redis:8-alpine` (40 MB) and `valkey/valkey:9-alpine` (19 MB), both starting in about
  a second, through a conformance suite for key-value engines: the parts of the shared suite
  that apply (lifecycle, connect failure, catalog walk, cancel) plus key viewers and
  classification. Dragonfly (52 MB, from its own registry; Docker Hub's copy is out of date) is
  tried by hand before release.

**Exit:** I connect to Redis 8 and Valkey 9, over TLS too; browse a database of a million keys
grouped by `:` without the server stalling; open a hash, a list, a sorted set and a stream; edit
a field and set a TTL; run `HGETALL` in the console; ⌘. a `BLPOP`; a read-only data source
refuses `SET`; and an MCP client reads a key through an MCP login.

#### Layers

Each layer is one pull request in a stack, useful and tested on its own, as N0a's and N0b's
were. From N1·3 on, each runs against `redis:8-alpine` and `valkey/valkey:9-alpine` in CI.

- **N1·1 — Plan:** this list, the decisions N1 takes (in [Decisions](#decisions)), and the
  roadmap's status.
- **N1·2 — The Redis language** (built): a `redis` module of `dbrover-language`, with no
  driver yet:
  - a script splits one command per line, and each command reads as `redis-cli` reads it:
    words, `"…"` strings with backslash escapes (`\n`, `\"`, `\x41`), `'…'` strings, and a
    line that starts with `#` as a comment;
  - a table of every command and subcommand of Redis 8 and Valkey 9, with its arity, its
    flags (`readonly`, `write`, `admin`, `blocking`) and its arguments' syntax, generated
    from the servers' own command definitions (what `COMMAND INFO` and `COMMAND DOCS`
    answer). The `dangerous` ACL category isn't kept: it marks `INFO` and `KEYS` as well as
    `FLUSHALL`, so it says nothing the flags and the warnings don't;
  - classification from those flags, with the plan's exceptions: `MULTI`, `EXEC`, `DISCARD`,
    `WATCH` and `UNWATCH` are transaction control; scripts are writes unless read-only
    (`EVAL_RO`, `FCALL_RO`); `SELECT`, `CONFIG`, `CLIENT`, `AUTH`, `HELLO`, `FLUSHALL`,
    `FLUSHDB` and `SHUTDOWN` are administration; a command neither `readonly` nor `write`
    reads only if it is one of a short list (`PING`, `ECHO`, `INFO`, `TIME`, `COMMAND`, …);
  - warnings for `FLUSHALL`, `FLUSHDB` and `KEYS`;
  - completion of command names, each with its arguments' syntax.

  Tests: a classification corpus, as the SQL dialects and the test language have; the
  splitter and tokens against `redis-cli`'s quoting; the warnings. The table holds 477 commands
  and subcommands; until N1·3 checks it against live servers, it comes from the sources'
  definitions alone.
- **N1·3 — The driver and the console** (built): a new crate, `dbrover-driver-redis`, on the `redis`
  crate over a TCP or TLS stream DbRover opens itself, so TLS is `dbrover-tls`'s rustls
  configuration on ring and an SSH tunnel works as for the others:
  - the **Redis / Valkey** data source: the shared server form, port 6379, the user optional,
    the database number in place of a database name, and TLS. Test connection reports Redis,
    Valkey or Dragonfly and its version, from `INFO server`;
  - the console shows a reply by its shape: an array as one column, a map as field and value,
    pairs (`ZRANGE … WITHSCORES`) as two columns, a number or text as one cell, a stream's
    entries as documents (`_id` first), binary text with hex escapes as `redis-cli` quotes it
    (`"\xff\x00"`), and a nested value as JSON text, which the cell viewer shows as a tree
    (N0b). `SCAN`, `HSCAN`, `SSCAN` and `ZSCAN` are followed to
    their end a page at a time, as `redis-cli --scan` does, so the grid's paging reads a
    large keyspace or value without one long command;
  - ⌘. as the plan says: `CLIENT UNBLOCK` for a blocking command, `SCRIPT KILL` or
    `FUNCTION KILL` for a script, otherwise a new connection;
  - `validate` checks each command's argument count against the table (the console only asks
    a SQL dialect's statements to be validated, so it isn't called yet); `transaction_status`
    is always idle, and manual commit mode isn't offered;
  - the explorer lists the databases that hold keys, with their key counts.

  Tests: the parts of the shared conformance suite that apply (lifecycle, connect failure,
  catalog walk, invalid paths, a lost session, cancel), on both servers; `check_execution`,
  `check_transactions`, `check_describe_table` and `check_validate` are SQL's. Besides: replies
  of each shape, a `SCAN` of 2,500 keys followed to its end, each way of cancelling (`BLPOP`,
  a script, and `DEBUG SLEEP`, which nothing stops), TLS and a tunnel on the Redis server, an
  engine test of a console and a read-only data source refusing `SET`, and a check that each
  server's own `COMMAND INFO` classifies every command as the table does. Manual: the data
  source, the console's commands, and the databases in the explorer. That check found every
  core command as the table had it, and the 229 commands of the modules `redis:8-alpine`
  ships (search, JSON, time series, probabilistic structures, vector sets), which joined the
  table from that server's answers: 706 commands and subcommands in all.
- **N1·4 — Keys in the explorer** (built): under each database, keys grouped by a separator
  (`:` by default, set per data source), read with `SCAN … COUNT 1000` and never `KEYS`; the
  filter box as `SCAN`'s `MATCH`; each key with its type's icon and its TTL. Tests: the
  conformance check that pages add up to the whole level on both servers, and the grouping.
  How it came out:
  - the pages are checked to hold every key once, in three pages of at most 1,000, with the
    filter matching the explorer's, but not with `conformance::check_children_pages`, which
    also compares their order with a second listing's: `SCAN` doesn't promise the same order
    twice, and a server still rehashing a freshly filled table answers two passes in
    different orders;
  - a level is listed in `SCAN`'s order, not sorted, since no page of a level that big can be
    sorted against the rest. A page holds exactly 1,000 nodes: one that stops partway through a
    `SCAN` batch keeps the batch's unread keys for the next page, which then goes on from the
    cursor after the batch (sending the batch's cursor again could answer with other keys
    while the server rehashes), and the driver remembers the groups each level has listed so
    far, since a group turns up in many batches.
    A page also stops after 100 `SCAN` calls (about 100,000 keys), for a level whose keys fall
    into a few groups;
  - the filter, as `MATCH`, lists every key under a partial level whose name contains the
    text, ignoring case, by its whole name and without grouping, so a key three levels down
    shows at once; the explorer's own filter then matches those names. A level of fewer than
    1,000 nodes is loaded whole and filtered by the explorer alone, as N0a does for every
    engine;
  - `TYPE` and `PTTL` for a page's keys go in one pipelined round trip; `SCAN`'s `TYPE` option,
    a filter by type, has no place in the explorer's filter box yet, and isn't offered.
- **N1·5 — Key viewers and edits** (built): **Open data** on a key opens a console with its type's
  read command (`GET`, `HSCAN`, `LRANGE`, `SSCAN`, `ZRANGE … WITHSCORES`, `XREVRANGE`,
  `JSON.GET`), run at once, and the driver pages through a large value. Above its result, a
  key bar shows the key's type, TTL and memory, where the server answers, with **Set TTL…**,
  **Persist**, **Rename…** and **Delete…** (which asks first). The grid's edits become one
  command per change (`HSET` and `HDEL`, `LSET`, `RPUSH` and `LREM`, `SADD` and `SREM`, `ZADD`
  and `ZREM`, `XADD` and `XDEL`, `SET … KEEPTTL`, `JSON.SET`), each run as a typed command is:
  checked against the safety level, written to the log and recorded in the history. Tests:
  each type opened and edited on both servers, and the console's key bar and edits fed
  results in kittests. How it came out:
  - the commands that read a key whole are recognised wherever they are typed, so `HGETALL`
    in a console is edited as **Open data**'s `HSCAN` is; the driver reads `LRANGE`,
    `ZRANGE … WITHSCORES` by index and `XRANGE`/`XREVRANGE` without `COUNT` a page at a
    time, a list's items with their index;
  - the grid's edits go through the engine's edits, which run a language without
    transactions one command at a time (`EditsRun::OneByOne`): on a failure, those before it
    stay made, and the key is read again; each is in the history with the command that
    undoes it;
  - the key bar's actions run as typed commands do, followed by the key's read command:
    **Set TTL…** (`EXPIRE`), **Persist**, **Rename…** (`RENAMENX`, so it never replaces
    another key) and **Delete…**, which asks first;
  - left out: the key's TTL and memory use in the bar (the explorer shows the TTL; nothing
    asks the server for either from a console yet), a stream's entries added and deleted in
    the grid (documents aren't edited there yet; `XADD` and `XDEL` work in the console), a
    JSON document's paths (the whole document is set with `JSON.SET key $`), and the size of
    a value of another type, which shows its `TYPE` only. A list item is deleted with
    `LREM key 1 value`, the first item equal to it, since Redis removes list items by value.
- **N1·6 — Safety and MCP** (built): the read-only level's dialog suggests an ACL user with only
  `+@read`; the MCP server serves a Redis data source only through its MCP login, its `query`
  tool takes a command, and `list_objects` pages keys, with a token for the next page. Tests:
  on the Redis test server, an ACL user that may only read serves as the MCP login, reads a
  key, and is refused a `SET` by the server as well as by the check. How it came out:
  - an ACL user with only `+@read` may not run `PING`, `SELECT` or `INFO`: the driver takes
    any answer to `PING` as proof the connection works, names the server without its version
    and lists only the data source's own database (counted with `DBSIZE`) when `INFO` is
    refused, and the manual says to add `+select` for a database other than 0, and `+info`
    to see every database;
  - `list_objects` pages every engine's large levels now, not only Redis's keys, with a
    `filter` and an `after` token, 1,000 objects at a time; a key's path step is `key`, a
    group's `key_prefix`;
  - the `query` tool keeps its `sql` argument, which now takes a Redis command too, so
    clients' calls don't change.

N1 is built. What the exit needs by hand: Dragonfly (`docker.dragonflydb.io/dragonflydb/
dragonfly`), not in CI, tried before release; a database of a million keys, browsed grouped by
`:` (the tests use 2,500); and TLS against a managed server with a publicly signed certificate
(the tests use the compose server's own authority).

The manual changes in the layer that changes what a user sees: N1·3 (the data source and the
console), N1·4 (keys in the explorer), N1·5 (the viewers) and N1·6 (read-only data sources and
MCP logins on Redis).

### N2 — MongoDB

- **Client: the official [`mongodb`](https://crates.io/crates/mongodb) driver** (3.9,
  Apache-2.0), on tokio, with rustls on ring by default, and `bson` for documents, which reads
  and writes canonical and relaxed Extended JSON. Replica sets, sharded clusters and
  `mongodb+srv://` addresses (its `dns-resolver` feature) all work through it.
- **Data source:** the shared server form (port 27017) with the authentication database, or a
  connection string pasted whole, which is what MongoDB Atlas hands out. SCRAM passwords first;
  X.509 certificates next; AWS IAM sign-in comes with the cloud track's C0.
- **Explorer:** databases, then collections, views and time-series collections, with their
  indexes. **Show definition** gives `db.createCollection(…)` with the collection's options and
  validator, and a `createIndex(…)` for each index. An estimated document count comes from
  `estimatedDocumentCount`, never a full count.
- **Console: a subset of the shell's syntax**, parsed by DbRover:
  - `db.orders.find({status: "open"}).sort({_id: -1}).limit(50)`, with `projection`, `skip`,
    `hint`, `collation` and `explain()` chained;
  - `aggregate([…])`, `countDocuments`, `distinct`, `findOne`;
  - writes: `insertOne`, `insertMany`, `updateOne`, `updateMany`, `replaceOne`, `deleteOne`,
    `deleteMany`, `findOneAndUpdate`, `bulkWrite`;
  - collections and indexes: `createCollection`, `drop`, `createIndex`, `dropIndex`,
    `renameCollection`;
  - `db.getCollection("name")` for names that aren't identifiers, `db.runCommand({…})`,
    `use <database>`, `show dbs`, `show collections`;
  - arguments are the shell's relaxed JSON (unquoted keys, single quotes, trailing commas,
    comments) with `ObjectId()`, `ISODate()`, `new Date()`, `NumberLong()`,
    `NumberDecimal()`, `UUID()`, `Timestamp()` and `/regular expressions/`.

  No variables, functions or loops: anything else is an error that names what isn't supported.
  No crate parses the shell's syntax, so DbRover writes a small parser, with the
  community `mongosh` crate's parser as a reference rather than a dependency. Commands are
  separated by `;` or by a new line once a call is complete.
- **Results** are documents (N0b), read through the driver's cursor in batches of 500; a cursor
  dropped before its end is closed on the server (`killCursors`) by the driver.
- **Classification:** a table of the methods above, plus the pipeline: an `aggregate` with
  `$out` or `$merge` writes. `db.runCommand` is read only for an allow-list (`ping`,
  `buildInfo`, `listCollections`, `collStats` and similar) and admin otherwise. Users, roles,
  `$function` and `$where` are admin.
- **Cancel:** the driver warns that dropping one of its calls part-way can leave it
  inconsistent, so each command runs in a task of its own, tagged with a `comment` unique to the
  run. ⌘. finds the operation with `$currentOp` on that comment and stops it with `killOp`, from
  the explorer's connection; a user may always kill their own operations.
- **Explain:** ⌥⌘E runs the command through the `explain` command (the driver has no helper),
  with `executionStats`, and shows the plan as a tree.
- **Validate:** for reads, `explain` with `queryPlanner` verbosity plans without running, so it
  catches an unknown operator or a malformed pipeline; writes get the local parse only.
- **Editing:** a field changed in the grid or the tree becomes
  `updateOne({_id: …}, {$set: {"address.city": …}})`, its value written with the type it had
  (canonical Extended JSON keeps it). **Insert document…** and **Delete document** on a
  collection's data. Shown in the log and recorded in the history, like grid edits.
- **Transactions:** none at first: each command commits on its own, and manual commit mode
  isn't offered. Multi-document transactions need a replica set and are left for later.
- **Safety and MCP:** a read-only data source refuses writes by the check, and the dialog
  suggests a user with the `read` role. The MCP server serves a MongoDB data source only with an
  MCP login; its `query` tool takes one shell command, and `list_objects` lists collections.
- **Compatible servers** (Amazon DocumentDB, Azure Cosmos DB for MongoDB, FerretDB) may connect
  through the same driver. Trying each and noting what differs is a small task for later, as
  C1 does for PostgreSQL-compatible databases.
- **CI:** `mongo:8` (302 MB compressed, starts in a few seconds), as a single server, through the
  shared suite's parts that apply plus document results, classification, editing and cancel.

**Exit:** I connect to MongoDB 8 locally and to an Atlas cluster through its connection string;
browse databases and collections; run a `find` with a sort and an `aggregate` and page through
them; expand a nested field into columns and open a document as a tree; edit a field and see it
keep its type; ⌘. a slow aggregation; a read-only data source refuses `deleteMany`; and an MCP
client queries a collection through an MCP login.

#### Layers

Each layer is one pull request in a stack, useful and tested on its own, as N1's were. From
N2·3 on, each runs against `mongo:8` in CI, a single server with a user and TLS on the same
port.

- **N2·1 — Plan:** this list, the decisions N2 takes (in [Decisions](#decisions)), and the
  roadmap's status.
- **N2·2 — The MongoDB language** (built): a `mongo` module of `dbrover-language`, with no
  driver yet:
  - a lexer for the shell's syntax: names, `"…"` and `'…'` strings with JavaScript's escapes,
    numbers, `/regular expressions/flags`, punctuation, and `//` and `/* */` comments;
  - a script splits at `;`, and at a line break once every bracket is closed and the next
    line doesn't go on with `.`, so a chain written over several lines stays one command;
  - a parser into DbRover's own tree of a command: `use <database>`, `show dbs`,
    `show collections`, and `db.<collection>.<method>(…)` or
    `db.getCollection("…").<method>(…)` with cursor methods chained (`sort`, `limit`,
    `skip`, `projection`, `hint`, `collation`, `batchSize`, `explain`), and `db.<method>(…)`
    (`runCommand`, `createCollection`, `getCollectionNames`, …). Arguments are the shell's
    relaxed JSON (unquoted keys, single quotes, trailing commas) with `ObjectId()`,
    `ISODate()`, `new Date()`, `NumberLong()`, `NumberInt()`, `NumberDecimal()`, `UUID()`,
    `BinData()`, `Timestamp()`, `MinKey`, `MaxKey` and regular expressions. Anything else, a
    variable, a function, a loop, `printjson`, is an error that names it and points at it;
  - classification from a table of the methods: reads (`find`, `findOne`, `aggregate`,
    `countDocuments`, `estimatedDocumentCount`, `distinct`, `getIndexes`, `show …`), writes
    (`insertOne`, `updateMany`, `bulkWrite`, `findOneAndUpdate`, …), schema changes
    (`createCollection`, `drop`, `createIndex`, `dropIndex`, `renameCollection`), and
    administration; an `aggregate` with `$out` or `$merge` writes; `$function`, `$where` and
    `$accumulator` anywhere in the arguments, and the users and roles methods, are
    administration; `db.runCommand` reads only for an allow-list (`ping`, `buildInfo`,
    `listCollections`, `listIndexes`, `collStats`, `dbStats`, `count`, `find`, `aggregate`
    without `$out` or `$merge`, …) and is administration otherwise; `use` is a read;
  - warnings for `deleteMany({})`, `updateMany({}, …)`, `remove({})` and `drop()`;
  - completion of the methods, the query and update operators and the pipeline stages, each
    method with its arguments.

  Tests: a classification corpus, as the other languages have; the splitter, the tokens and
  the parser against snippets from MongoDB's documentation; errors that name what isn't
  supported. How it came out:
  - `runCommand`'s writes (`insert`, `update`, `delete`, `findAndModify`, `bulkWrite`) and
    schema changes (`create`, `createIndexes`, `drop`, `dropIndexes`, `collMod`,
    `renameCollection`) are classified as such; any other command outside the read list is
    administration. `db.dropDatabase()`, `db.currentOp()` and `db.killOp()` are
    administration;
  - the users' and roles' helpers (`db.createUser(…)`) aren't read, and the error suggests
    `db.runCommand({…})`, which sends them as administration. So are other methods DbRover
    doesn't run, such as `mapReduce`;
  - numbers are kept as written; the driver stores an integer that fits 32 bits as one and
    any other number as a double, as the shell does;
  - completion inserts the words as listed (`insertOne`, `$match`), since MongoDB's names are
    case-sensitive, rather than in the case being typed as SQL's keywords and Redis's
    commands are.
- **N2·3 — The driver and the console** (built): a new crate, `dbrover-driver-mongodb`, on
  the official `mongodb` driver:
  - the **MongoDB** data source: the shared server form (port 27017) with the authentication
    database, or a connection string pasted whole (`mongodb://` or `mongodb+srv://`, which
    Atlas hands out), whose password moves to the keychain; TLS in the driver's own rustls on
    ring, from the data source's mode; SSH tunnels with the server form. Test connection
    reports MongoDB and its version, from `buildInfo`;
  - each command runs as the parser read it, its arguments turned into BSON: a cursor's
    documents (`find`, `aggregate`, `listCollections`) are read 500 at a time and reach the
    grid as documents (N0b), in canonical Extended JSON; a count is one cell, `distinct` one
    column, and a write's answer one document, as the shell prints it; `use` changes the
    database the console's next commands run in;
  - each command runs in a task of its own, tagged with a `comment` unique to the run, and ⌘.
    finds it with `$currentOp` and stops it with `killOp`;
  - `transaction_status` is always idle, and manual commit mode isn't offered;
  - the explorer lists the databases.

  Tests: the parts of the shared conformance suite that apply (lifecycle, connect failure,
  catalog walk, invalid paths, a lost session, cancel), on the `mongo:8` server; results of
  each kind, a `find` and an `aggregate` paged past 500 documents, each BSON type through to
  canonical Extended JSON and back, a slow aggregation cancelled (`$function` sleeping, or a
  `$where`), TLS and a tunnel, the connection string, and an engine test of a console and a
  read-only data source refusing `deleteMany`. Manual: the data source and the console. How it
  came out:
  - every method is sent as the server command it stands for (`find`, `aggregate`, `count`,
    `distinct`, `insert`, `update`, `delete`, `findAndModify`, `createIndexes`, …), built by
    DbRover, rather than through the driver's typed helpers: one path carries the run's
    `comment`, and `explain()` wraps the very command the method sends. `insertOne` and
    `insertMany` give a document without `_id` a new `ObjectId` first, as the shell does, so
    the answer names it; `bulkWrite` sends each operation as its own command, in order;
  - a cursor is read in a task that owns it, and one document past each page is read ahead,
    so the grid knows exactly when a result ends;
  - `check_lost` doesn't apply: the client is a pool that reconnects by itself, so a lost
    server shows as a failed command, or as a lost connection when no server can be reached;
  - `verify-full` through an SSH tunnel is refused (see [Decisions](#decisions)); the driver
    waits 15 seconds for a server rather than its own 30;
  - CI runs MongoDB 7.0, not 8: every MongoDB 8 refuses to start on Linux kernels 6.19 to
    7.0.13, the GitHub Ubuntu runners' in October 2026 (its TCMalloc breaks on their `rseq`,
    SERVER-121912). The compose file takes the tag from `DBROVER_MONGO_TAG`, 8 unless set, so
    tests run on 8 locally (8.3.13 when this was built) and on 7.0 in CI;
  - the engine's cursor wrapper didn't pass a result's shape on, so every result of documents
    reached the console as rows, Redis's stream entries included; it does now.
- **N2·4 — The explorer** (built): under each database, its collections, views and time-series
  collections, each with its estimated document count (`estimatedDocumentCount`, never a full
  count), and under a collection its indexes. **Show definition** gives
  `db.createCollection(…)` with the collection's options and validator, and a
  `createIndex(…)` for each index. **Open data** on a collection opens a console with
  `db.getCollection("…").find({})`, run at once. A database with many collections pages as
  N0a's explorer does, by name, with the filter as a `listCollections` filter. Tests: the
  catalog walk with each kind, the definitions, and paging 2,500 collections. How it came out:
  - a view and a time-series collection are collections in the tree too (`NodeKind::Collection`,
    as `listCollections` calls them all), told apart by their detail: one node kind with one
    set of actions, **Open data** and **Show definition** among them. A view has no indexes;
  - views and time-series collections show no count: each is a pipeline over another
    collection, so counting one reads every document. Plain collections are counted with
    `count` and no filter, which reads the collection's metadata, all of a page's at once over
    the client's pool;
  - a page lists every name the user may see with `listCollections` and `nameOnly` (cheap even
    for thousands), sorts them, and reads only the page's own collections' options and counts.
    The filter is a case-insensitive regular expression on the name, sent to the server;
  - what **Show definition** gives runs again as written, and the test checks that it does: the
    shell's syntax is written back from BSON with each value's type (`NumberLong("7")`,
    `ISODate("…")`), by a writer N2·6's edits use too;
  - the paging test uses 1,100 collections, enough for two pages, rather than 2,500, since each
    collection is files of its own on the server and CI time is a budget.
- **N2·5 — Explain, validate and results to a file** (built): ⌥⌘E runs the command at the cursor
  through `explain` with `executionStats` and shows the plan as a tree; `validate` plans a read
  with `queryPlanner` verbosity without running it, and checks a write by parsing it alone;
  **Run to file** and **All rows** for documents write every document of a `find` or an
  `aggregate` to JSON or JSON Lines straight from the cursor, which N0b left to N2. Tests:
  an explained `find` and `aggregate`, an unknown operator caught before running, and a run to
  file of more than one page that reads back to the same documents. How it came out:
  - each language says how it explains (`QueryLanguage::explain`): SQL with its dialect's
    prefix, as before, and MongoDB by putting `.explain("executionStats")` after a `find` or an
    `aggregate`. Explain is offered in a MongoDB console, and does nothing on a command with no
    plan to show. The plan is a document, browsed with the result's **Tree**, not a view of its
    own;
  - `validate` plans a `find`, `aggregate`, `count` or `distinct` with `queryPlanner`, and only
    reads a write. The console still asks only a SQL dialect's statements to be validated, as
    for Redis: its checker of problems is written around SQL's, and making it a language's is a
    change of its own. A command DbRover can't read is still shown as an error when it runs;
  - Run to file knows a command's result before it runs through `QueryLanguage::result_shape`:
    documents for a `find` or an `aggregate`, and nothing to write for MongoDB's other commands.
    Documents read again from the database, by Run to file or **All documents**, aren't
    flattened, so they are written as JSON or JSON Lines only, and the dialog says why the
    other formats are greyed out.
- **N2·6 — Editing** (built): a field changed in the grid becomes
  `updateOne({_id: …}, {$set: {"address.city": …}})`, its value written with the type the
  old value had, read from its canonical Extended JSON; a document without `_id` can't be
  edited. **Insert document…** and **Delete document** on a collection's data. Each edit runs
  as a typed command does, one at a time (`EditsRun::OneByOne`): checked against the safety
  level, written to the log and recorded in the history. Tests: a field of each type edited
  and read back with its type, an expanded field edited by its path, a document inserted and
  deleted, and kittests of the console's edits fed results. How it came out:
  - `dbrover-language`'s `mongo` module writes the commands (`CollectionView`), so the console
    sends what the parser reads and the safety check classifies; it reads documents with
    `dbrover-document`, now one of its dependencies. A value is written back in the shell's
    syntax with its type, from its canonical Extended JSON; text typed over it is read as the
    old type (`typed_value`), or as written when it is already a value of the shell's syntax.
    JavaScript code, symbols, `undefined` and DBPointers can't be written that way, so a
    document holding one can't be deleted with an undo, and such a field can't be set;
  - a `find` on one collection is editable, by `_id`; an `aggregate`'s documents aren't, since
    they may not be any collection's;
  - **Insert document…** is the grid's **Add row**, a document of the fields typed into it,
    rather than a dialog of its own; a document is typed whole, in the shell's syntax, into a
    console's `insertOne`. **Delete rows** is **Delete document**;
  - documents are read again after every submit, so each field shows with the type the server
    stored; an array's items aren't columns of the grid, so they are edited by setting the
    whole array, or with `$set` in a console.
- **N2·7 — Safety and MCP** (built): the read-only level's dialog suggests a user with the `read`
  role; the MCP server serves a MongoDB data source only through its MCP login, its `query`
  tool takes one shell command, and `list_objects` lists databases and collections, paged.
  Tests: on the MongoDB test server, a user with only the `read` role serves as the MCP login,
  reads a collection, and is refused a `deleteMany` by the server as well as by the check. How
  it came out: the guard, the advice and `list_objects`' paging were already every engine's
  (N0a and N1), so MongoDB took them as they were; this layer is the test binary `mongodb` of
  `dbrover-mcp`, the tools' descriptions, the manual's `read` user, and the website. The MCP
  login signs in against the data source's **Auth database**, `admin` when it is empty.

N2 is built. What the exit needs by hand, before release: an Atlas cluster through its
`mongodb+srv://` connection string (the tests use a `mongodb://` string to the compose server,
so the DNS lookup of `+srv` isn't run in CI), and TLS against a managed server with a publicly
signed certificate (the tests use the compose server's own authority). CI runs MongoDB 7.0
rather than 8, which won't start on the runners' kernel (see N2·3); every test ran on 8.3.13
locally too.

The manual changes in the layer that changes what a user sees: N2·3 (the data source and the
console), N2·4 (the explorer), N2·5 (explain and results to a file), N2·6 (editing) and N2·7
(read-only data sources and MCP logins on MongoDB).

### N3 — Elasticsearch and OpenSearch

- **Client: their REST API over HTTP, with the hyper client and `dbrover-tls`'s rustls
  configuration** the ClickHouse driver already uses, so `verify-full` works with a CA file. The
  official `elasticsearch` crate is still an alpha whose last release (August 2025) predates the
  current 9.x servers, and the `opensearch` crate's rustls feature brings in aws-lc-rs and lags
  OpenSearch 3. A console sends requests as typed, so the clients' typed APIs add little.
- **Data source:** the shared server form (port 9200) with a user and password, or an API key;
  an Elastic Cloud address. Signing in to Amazon OpenSearch Service with AWS credentials comes
  with the cloud track's C0. Test connection reports Elasticsearch or OpenSearch and the
  version, from `GET /`.
- **Explorer:** indices (system and hidden ones only with **System** ticked), aliases, data
  streams and index templates; under an index, its fields from the mapping. **Show definition**
  gives the index's mapping and settings as JSON.
- **Console:** requests as Kibana's Dev Tools and OpenSearch Dashboards write them: a line with
  the method and path, then an optional JSON body.

  ```text
  GET /orders/_search
  { "query": { "match": { "status": "open" } }, "sort": [{ "created": "desc" }] }
  ```

  Each request runs on its own, as statements do. Completion offers the common paths and the
  index names. The SQL endpoints (`POST /_sql` on Elasticsearch, in its free Basic licence, and
  `POST /_plugins/_sql` on OpenSearch) are requests like any other, and their answers show as
  rows.
- **Results:** a search's hits are documents (N0b), with `_index`, `_id` and `_score` as columns
  before `_source`'s fields; aggregations and every other answer show as a tree. Paging past the
  first page uses a point in time and `search_after`, which both servers have.
- **Classification** by method and path: `GET`, and `POST` to `_search`, `_count`,
  `_msearch`, `_sql` and similar reads, are reads; `_doc`, `_update`, `_bulk`,
  `_update_by_query` and `_delete_by_query` are writes; creating and deleting indices, mappings
  and templates is a schema change; `_cluster`, `_nodes`, `_snapshot` and security paths are
  admin.
- **Cancel:** each request carries an `X-Opaque-Id` unique to the run; ⌘. finds its task with
  the tasks API and cancels it (`POST /_tasks/<id>/_cancel`), then drops the request.
  Elasticsearch also stops a search when its client disconnects.
- **Editing:** a `_source` field changed in the grid or tree becomes
  `POST /<index>/_update/<id>` with that field, and refreshes so the result shows the change.
- **No transactions, and explain** is the search's `profile` and `_explain`, shown as a tree.
- **Safety and MCP:** as for MongoDB: the check, plus a user or API key with read-only
  privileges, which the MCP server requires as its MCP login.
- **CI:** these servers run on the JVM and are the largest images in this plan:
  `elasticsearch:9` 896 MB and `opensearch:3` 1,160 MB compressed, and each wants a few
  gigabytes of memory, `vm.max_map_count` raised, and its security set up or switched off. One
  of them (Elasticsearch, the more used) runs with the pull request suite if it keeps the Linux
  job under about 15 minutes with its heap held to 512 MB; otherwise both run in a separate job
  or nightly. Measured when N3 started (2026-10-31), on a laptop: Elasticsearch 9.5.5 and
  OpenSearch 3.9.0, each a single node with a 512 MB heap, were both healthy 17 seconds after
  `docker compose up`, using about 1 GB of memory each, with no `vm.max_map_count` change (a
  single node skips the bootstrap checks). The Linux job took about 7 minutes then, so **both
  run with the pull request suite** (see N3·3), and the job's time is checked again there.

**Exit:** I connect to Elasticsearch 9 and OpenSearch 3; browse indices and their fields; run a
search with a sort and page past its first hundred hits; see an aggregation as a tree; run a
`_sql` query as rows; edit a field of a document; ⌘. a slow search; and a read-only data source
refuses a `_delete_by_query`.

#### Layers

Each layer is one pull request in a stack, useful and tested on its own, as N1's and N2's
were. From N3·3 on, each runs against both servers in CI (see N3·3).

- **N3·1 — Plan:** this list, the decisions N3 takes (in [Decisions](#decisions)), and the
  roadmap's status.
- **N3·2 — The request language** (built): a `search` module of `dbrover-language`, with no driver
  yet:
  - tokens for the method, the path and its query string, and JSON for the body: strings,
    numbers, `true`, `false` and `null`, punctuation, and comments (`#` and `//` lines, as
    Kibana's console allows), so the editor highlights a body as JSON;
  - a script splits where a line starts with a method (`GET`, `POST`, `PUT`, `DELETE`,
    `HEAD`, in any case), so a body may span any number of lines;
  - a parser into DbRover's own request: the method, the path split into its parts and its
    query string, and the body, read with `dbrover-document` as one JSON value, or as one
    value per line for `_bulk`, `_msearch` and the other endpoints that take NDJSON. A
    missing leading `/` is added, as Kibana does; a body that isn't JSON is an error that
    points at it;
  - classification by method and path, from a table of the endpoints: `GET` and `HEAD` read,
    and so do `POST` to `_search`, `_msearch`, `_count`, `_mget`, `_field_caps`, `_explain`,
    `_validate/query`, `_termvectors`, `_render/template`, the point-in-time endpoints and the
    SQL endpoints when their statement reads; `_doc`, `_create`, `_update`, `_bulk`,
    `_update_by_query`, `_delete_by_query`, `_reindex` and `DELETE` of a document write;
    creating, changing and deleting indices, mappings, settings, aliases, data streams and
    templates (`PUT /<index>`, `_mapping`, `_settings`, `_aliases`, `_index_template`,
    `_rollover`, `_open`, `_close`, …) changes the schema; `_cluster`, `_nodes`,
    `_snapshot`, `_tasks`, `_ilm`, `_ingest`, `_security`, `_plugins/_security` and any path
    the table doesn't know are administration. A `GET` of an administration path still
    reads;
  - warnings for `_delete_by_query` and `_update_by_query` whose query matches everything
    (`match_all`, or none), and for `DELETE` of an index name with a wildcard or `_all`;
  - completion of the methods, the common endpoints and the query DSL's words, each endpoint
    with its methods. Index names join it in N3·4, from the explorer.

  Tests: a classification corpus, as the other languages have; the splitter and the parser
  against requests from both servers' documentation; errors that point at a body that isn't
  JSON. How it came out:
  - a body may start on the request line, after the path, as well as on the line after it;
  - a comment line inside a body isn't sent: it becomes spaces, so the server's line and
    column for an error still match the body on screen;
  - an NDJSON value typed over several lines is sent on one, its line breaks made spaces;
  - a string not yet closed at the end of its line is its quote and a word, so completion
    offers the query's words while a key is being typed; a closed string is highlighted as a
    string;
  - the SQL endpoints are classified by their statement's first word: `SELECT`, `SHOW`,
    `DESCRIBE`, `EXPLAIN` and `WITH` read, anything else (OpenSearch's `DELETE`) writes;
    PPL, ES|QL (`POST /_query`), EQL and async searches read;
  - `DELETE` of one index warns too, as MongoDB's `drop()` does;
  - Kibana's triple-quoted strings are an error that says how to write the string in JSON.
- **N3·3 — The driver and the console** (built): a new crate, `dbrover-driver-elasticsearch`, over
  HTTP with hyper and `dbrover-tls`, as the ClickHouse driver is:
  - the **Elasticsearch / OpenSearch** data source: the shared server form (port 9200) with
    a user and password, or an API key in the password's place, kept in the keychain the same
    way. An Elastic Cloud deployment is its address, port 443 and `verify-full`. TLS from the
    data source's mode: HTTPS is a port of its own, so `prefer` is plain HTTP, as on
    ClickHouse. SSH tunnels. Test connection reports Elasticsearch or OpenSearch and the
    version, from `GET /`;
  - each request is sent as written, with its body as typed. A search's hits are documents
    (N0b): `_index`, `_id` and `_score` first, then `_source`'s fields; a search without a
    `size` reads every hit, a page at a time as the grid asks, through a point in time and
    `search_after` with `_shard_doc` as the tiebreaker, and one with a `size` reads at most
    that many. A search with `from`, `scroll` or aggregations runs as typed and shows its
    answer whole. `_sql` (Elasticsearch, with `format=json`) and `_plugins/_sql`
    (OpenSearch) answer as rows with their columns' names and types, following the answer's
    cursor for more pages. `_cat` answers come as JSON, each line a document. Any other
    answer is one document, browsed as a tree;
  - each request carries an `X-Opaque-Id` unique to the run; ⌘. finds its tasks with the
    tasks API, cancels them, and drops the request. Elasticsearch also stops a search when
    its client goes away;
  - an error answer shows the server's `type` and `reason` (`index_not_found_exception: no
    such index [orders]`), and its line and column for a body the server couldn't read;
  - `transaction_status` is always idle, and manual commit mode isn't offered;
  - the explorer lists the indices.

  Tests: the parts of the shared conformance suite that apply (lifecycle, connect failure,
  catalog walk, invalid paths, cancel), on both servers; a search paged past 1,000 hits with
  a sort, an aggregation as one document, `_sql` as rows on each server, a `_cat` answer as
  documents, a slow search cancelled, TLS with the test authority, an API key, and an engine
  test of a console and a read-only data source refusing a `_delete_by_query`. Manual: the
  data source and the console. How it came out:
  - the client is hyper's, with `hyper-rustls` and `dbrover-tls`, built directly rather than
    through ClickHouse's client crate; `hyper` and `http-body-util` became workspace
    dependencies at the versions already built, so nothing new is compiled;
  - a paged search keeps only the query-string parameters that don't change what a page holds
    (`q`, `_source`, `timeout`, …); any other, such as `routing` or `filter_path`, runs it as
    typed. It sets `track_total_hits` to `false` unless the search says otherwise, since the grid
    doesn't show the count, and reads one hit past each page, so the grid knows when the
    hits end. A point in time the server let go of, after a grid sat longer than its five
    minutes, is opened again and the page asked for again;
  - a hit's document is its `_index`, `_id` and `_score`, its source's fields, then the rest of
    the hit (`highlight`, `fields`, …), without its sort values; should a source field share a
    name with one of those, the source stays whole under `_source`, so nothing is hidden;
  - OpenSearch's SQL plugin is asked for pages of 500 (`fetch_size`) when the statement doesn't
    say, since without it the plugin answers at most 200 rows and no cursor; PPL has no cursor
    and answers whole;
  - a node without security still takes a password, which may be empty, as a Redis or MongoDB
    server without one does: the engine asks for a data source's password when none is saved;
  - listing tasks needs the `monitor` cluster privilege, which a user who may only read lacks;
    ⌘. then drops the request alone, which Elasticsearch takes as a cancel;
  - the explorer lists every index, hidden ones among the system ones, with its number of
    documents; aliases, data streams, templates and fields come with N3·4;
  - CI starts both servers with the others: Elasticsearch with security and HTTPS, so TLS,
    users and API keys are tested; OpenSearch with its security plugin off, so plain HTTP is.
- **N3·4 — The explorer** (built): folders of indices, aliases, data streams and index templates;
  system and hidden indices (a name starting with `.`) only with **System** ticked. An
  index shows its document count and size, and under it its fields from the mapping, an
  object's fields named by their path (`address.city`) with their type. **Show definition**
  gives `PUT /<index>` with its mappings, its settings that can be set again and its aliases;
  `PUT /_index_template/<name>` for a template; the alias's actions for an alias. **Open data**
  opens a console with a `match_all` search of the index, alias or data stream, run at once.
  A cluster with many indices pages them by name, with the filter as a wildcard pattern sent
  to the server. Completion offers the index names the explorer has loaded. Tests: the
  catalog walk with each kind, the definitions run again as written, and paging 1,100
  indices. How it came out:
  - an index, an alias and a data stream are all `NodeKind::SearchIndex`, told apart by their
    folder, so they share **Open data**; a template is `NodeKind::IndexTemplate`. A field is a
    `NodeKind::Column` named by its path; a `nested` field shows before its fields, and a
    field's multi-fields (`status.keyword`) after it;
  - a data stream has no definition: `PUT /_data_stream/<name>` would say nothing its template
    doesn't, so **Show definition** is the template's. An index's definition leaves out the
    settings the server sets (`uuid`, `creation_date`, `version`, `provided_name`, `resize`),
    a template's its dates;
  - Elasticsearch's managed templates (`_meta.managed`) and hidden data streams are system
    objects; OpenSearch refuses `expand_wildcards` on `_data_stream`, so it is asked only of
    Elasticsearch;
  - the paging test pages seven indices three at a time, with a filter in capitals, rather
    than 1,100: a node holds at most 1,000 shards unless told otherwise, and each index takes
    a cluster state update, which would cost CI about a minute per server;
  - completion asks a language which catalog objects its commands name
    (`QueryLanguage::named_objects`), and offers the loaded ones at the tree's top or one
    folder down, so a search console completes index names without a SQL dialect.
- **N3·5 — Explain, validate and results to a file** (built): ⌥⌘E runs the search at the cursor with
  `profile` set and shows the answer, the profile in it, as a tree; `validate` asks the
  server's `_validate/query` about a search's query without running it; **Run to file** and
  **All documents** write every hit of a search to JSON or JSON Lines, through a point in
  time. Tests: a profiled search, an unknown query caught before running, and a run to file
  of more than one page. How it came out:
  - `"profile": true` goes at the end of the body, before its closing brace, so a position
    the server gives in the rest of the body still points where it did; a search without a
    body gets one. A profiled search runs as typed (N3·3), so its answer is one document,
    browsed with **Tree**, as MongoDB's plans are;
  - `validate` sends a search's or a count's `query` to `_validate/query` with `explain`, and
    shows the server's error; any other request is only read. As for MongoDB, the console asks
    only SQL to be validated: its checker of problems is written around SQL's;
  - every search returns documents for Run to file and **All documents**, a search for its
    aggregations among them, whose one document is the whole answer.
- **N3·6 — Editing** (built): a `_source` field changed in the grid becomes
  `POST /<index>/_update/<id>?refresh=true` with that field in a partial document, its value
  written with the JSON type the old one had; **Add row** indexes a new document
  (`POST /<index>/_doc?refresh=true`), and **Delete rows** deletes one
  (`DELETE /<index>/_doc/<id>?refresh=true`). Each runs as a typed request does, one at a
  time: checked, logged and recorded in the history. Each hit's own `_index` is used, so the
  hits of an alias or a pattern are edited too. Tests: a field of each JSON type edited and
  read back, an expanded field edited by its path, a document added and deleted, and
  kittests of the console's edits. How it came out:
  - what the grid's edits of documents need from a language is one type in
    `dbrover-language`, `DocumentView`: MongoDB's `CollectionView` or a search's `IndexView`,
    each naming a document's key columns and writing the command for a deletion, a changed
    field and an added document, with its undo. The app's edits of documents moved there from
    their MongoDB-only form, unchanged for MongoDB;
  - which searches show their hits as documents is decided once, in `dbrover-language`
    (`search::pages_hits`), for the driver's paging and the grid's edits alike;
  - an undo is written for each change: the old value as a partial document; a field the
    document didn't have removed by a script, its names passed as parameters; a deleted hit
    put back whole, unless its document holds one of a hit's own keys (`highlight`, `fields`),
    which can't be told from its source's;
  - a hit indexed with its own routing carries it on every request; an id is percent-encoded
    in the path, so one holding `/` or `?` stays one part;
  - **Add row** indexes into the search's one index or alias, or into the `_index` typed in;
  - a partial update has Elasticsearch write the source again, so a number reads back as the
    server writes it (`2.5` for `2.50`).
- **N3·7 — Safety and MCP** (built): the read-only level's dialog suggests a user or API key
  with only the `read` and `view_index_metadata` privileges; the MCP server serves the data
  source only through its MCP login, its `query` tool takes one request, and `list_objects`
  lists the explorer's levels, paged. Tests: on the Elasticsearch test server, a user with a
  read-only role serves as the MCP login, searches an index, and is refused a
  `_delete_by_query` by the server as well as by the check. How it came out:
  - a user with index privileges alone can't connect: `GET /` needs the cluster's `monitor`
    privilege, and so do the explorer's `_cat` and the tasks API that ⌘. uses, and an index's
    count in the explorer needs `monitor` on the index. Both are read-only, so the advice is a
    role with the cluster's `monitor` and `read`, `view_index_metadata` and `monitor` on the
    indices, which the test's MCP login has;
  - the guard, the advice and `list_objects`' paging were already every engine's (N0a, N1), so
    Elasticsearch took them as they were: this layer is the test binary `elasticsearch` of
    `dbrover-mcp`, the advice's wording, the tools' descriptions, the manual's role, and the
    website;
  - a data source signing in with an API key takes a second API key as its MCP password; the
    MCP login is then only its name;
  - the test runs on Elasticsearch only: OpenSearch's test server has its security plugin off,
    so it has no users to sign in as.

N3 is built. What the exit needs by hand, before release: an Elastic Cloud deployment through
its endpoint with an API key and `verify-full` against the public authorities, OpenSearch with
its security plugin on (its demo certificates, with `require`), and Amazon OpenSearch Service
with a fine-grained access control user. CI runs Elasticsearch 9.5.5 with HTTPS and users, and
OpenSearch 3.9.0 without its security plugin.

The manual changes in the layer that changes what a user sees: N3·3 (the data source and the
console), N3·4 (the explorer), N3·5 (explain and results to a file), N3·6 (editing) and N3·7
(read-only data sources and MCP logins).

### N4 — Cassandra and ScyllaDB

- **Client: the [`scylla`](https://crates.io/crates/scylla) crate** (1.9, ScyllaDB's own,
  MIT OR Apache-2.0), which works with Apache Cassandra too. Its rustls feature (`rustls-023`)
  compiles in aws-lc-rs beside ring, as `tiberius`'s does; the driver hands it `dbrover-tls`'s
  ring configuration and installs ring as rustls's process-wide provider, as the SQL Server
  driver does. Ask upstream for a ring-only feature at the same time.
- **Data source:** the shared server form (port 9042) with the local datacenter, which the
  client needs to route requests. Password authentication first.
- **Explorer:** keyspaces, then tables, materialized views, user-defined types, functions and
  aggregates, with columns (partition and clustering keys marked) and secondary indexes.
  **Show definition** uses the server's own `DESCRIBE` where it has one (Cassandra 4.0 and
  later), and is rebuilt from `system_schema` otherwise.
- **Dialect:** a `Dialect` of `dbrover-sql` (see the decisions): `--`, `//` and `/* */`
  comments, `$$` strings, double-quoted names, `BEGIN BATCH … APPLY BATCH` kept as one statement,
  and CQL's function list for completion.
- **Results** are rows with typed columns from the result's metadata, formatted as `cqlsh`
  does; lists, sets, maps and user-defined types show as JSON, browsable in N0b's tree. Paging
  uses the protocol's own, 500 rows a page.
- **Classification** by a small classifier of CQL's statements: `SELECT` reads; `INSERT`,
  `UPDATE`, `DELETE`, `BATCH` and `TRUNCATE` write; `CREATE`, `ALTER` and `DROP` change the
  schema; roles, users, permissions and `USE` of a system keyspace are admin.
- **Validate** prepares the statement, which checks names in the protocol without running it.
- **Cancel:** the protocol has none. ⌘. stops waiting and frees the console; Cassandra finishes
  the statement or stops at its own timeout. On ScyllaDB, every statement runs with
  `USING TIMEOUT` from the data source's statement timeout, so the server stops it too.
- **Editing:** a cell becomes an `UPDATE` by the full primary key, as on the SQL engines.
- **No transactions** (lightweight transactions are a per-statement condition), so manual commit
  mode isn't offered.
- **Safety and MCP:** no read-only session; a role with only `SELECT` is the guard, required as
  the MCP login.
- **CI:** one of the two runs with the pull request suite. `cassandra:5` is the smaller image
  (168 MB) but starts in about a minute on the JVM, with `MAX_HEAP_SIZE` and `HEAP_NEWSIZE`
  set or a small runner kills it; `scylladb/scylla` (275 MB) starts faster with `--smp 1
  --memory 1G --overprovisioned 1`. Measure both; the other runs nightly.

**Exit:** I connect to Cassandra 5 and ScyllaDB; browse keyspaces and tables; page through a
large table; see a map column as a tree; edit a cell; a misspelt column is caught before
running; and a read-only data source refuses an `INSERT`.

#### Layers

Each layer is one pull request in a stack, useful and tested on its own, as N3's were. From
N4·3 on, each runs against ScyllaDB with the pull request suite, and against Cassandra in a
nightly job (see the CI measurement below).

- **N4·1 — Plan:** this list, the decisions N4 takes (in [Decisions](#decisions)), the CI
  measurement, and the roadmap's status.
- **N4·2 — CQL as a dialect** (built): `Dialect::Cassandra` in `dbrover-sql`, and
  `QueryLanguage::Sql(Dialect::Cassandra)`, with no driver yet:
  - its lexical rules: `--`, `//` and `/* */` comments (not nested), `'…'` strings with `''`
    for a quote and no backslash escapes, `$$…$$` strings, and `"…"` names;
  - splitting at semicolons, with `BEGIN [UNLOGGED | COUNTER] BATCH … APPLY BATCH` kept as one
    statement, so a batch is sent, classified and recorded whole;
  - a classifier of its own, reading a statement's first words, since `sqlparser` has no CQL
    dialect: `SELECT` and `DESCRIBE` read; `INSERT`, `UPDATE`, `DELETE`, a batch and `TRUNCATE`
    write; `CREATE`, `ALTER` and `DROP` of keyspaces, tables, types, indexes, materialized
    views, functions, aggregates and triggers change the schema; roles, users, `GRANT`,
    `REVOKE`, `LIST` and service levels are administration, and so is `USE` of a system
    keyspace, while `USE` of another reads. A statement that starts with anything else is
    unparseable, so the safety checks refuse it. The keyspaces a data statement's table is
    qualified with go to the MCP server's allow-list, as SQL's schemas do;
  - a warning before `TRUNCATE`, which empties a table, as before SQL's `DELETE` without
    `WHERE` (CQL refuses a `DELETE` or an `UPDATE` without one);
  - completion of CQL's keywords and of the functions Cassandra 5 and ScyllaDB share (`now()`,
    `uuid()`, `toTimestamp`, `writetime`, `ttl`, `token`, `toJson`, …), and highlighting of
    CQL's words, without changing any other dialect's;
  - the SQL features that don't apply stay hidden: the syntax check with `sqlparser` (the
    server checks instead, N4·3), explain (CQL has none), sorting and filtering in the database
    (CQL can't select from a query), importing a file (an import runs in a transaction),
    manual commit mode, the grid's edits until N4·5, and the object helpers' DDL dialogs.

  Tests: a classification corpus from Cassandra 5's and ScyllaDB's CQL references, as the
  other dialects have; batches split as one statement; each comment and string form lexed; a
  kittest of a CQL console's completion and batches. How it came out:
  - a dialect says what it lacks rather than being matched on: `Dialect::sqlparser_dialect` is
    `None` for CQL (no syntax check, and a query's columns are read from its tokens),
    `explains` and `writes_ddl` are false, and `can_arrange` refuses it;
  - CQL's own words (`keyspace`, `frozen`, `filtering`, `writetime`, …) are highlighted and
    completed by `Dialect::is_keyword` and `Dialect::keywords` in CQL only: in the shared list
    the formatter would upper-case columns named `map` or `token` in every other dialect;
  - a batch's `BEGIN` opens a block only before `BATCH`, `UNLOGGED` or `COUNTER`, and only
    `APPLY BATCH` closes it, so a column named `end` doesn't;
  - the MCP server's `explain` tool refuses a CQL statement rather than run it with no prefix;
  - `$tag$ … $tag$` is read as a string too, as PostgreSQL's is: CQL has only `$$`, and no
    valid CQL reads differently;
  - until N4·5, the grid says CQL rows can't be edited yet.
- **N4·3 — The driver and the console** (built): a new crate, `dbrover-driver-cassandra`, on
  `scylla` 1.9:
  - the **Cassandra / ScyllaDB** data source: the shared server form (port 9042, 9142 with
    TLS) with an optional user, for a cluster that doesn't ask; a **Keyspace** in the
    database's place, optional, which a console starts in; a **Local datacenter**, optional,
    which requests go to first; and a **Statement timeout**, optional (below). TLS from the
    data source's mode with `dbrover-tls`'s configuration: CQL over TLS is a port of its own on
    most servers, so there is no `prefer`, and `verify-full` checks the certificate against
    the host typed, since the client names each node to rustls by its IP address. SSH tunnels.
    Test connection reports ScyllaDB or Apache Cassandra and the version, from
    `system.versions` (ScyllaDB's alone) or `system.local`;
  - the cluster gives the client its nodes' addresses as they know them, which a client outside
    their network may not reach (a node in Docker, or behind NAT); a node that doesn't answer
    at its own address within a second is reached through the node the data source names,
    which coordinates the request. Through an SSH tunnel every node is reached through it;
  - each statement runs as typed, unprepared, so nothing is cached on the server. Its rows
    page with the protocol's own paging, 500 rows a page as the grid asks; their columns are
    typed from the result's metadata (`map<text, int>`), and values are written as CQL reads
    them: timestamps in UTC to the millisecond, blobs as `0x…`, decimals and varints with every
    digit, durations as `1h30m`. Lists, sets, maps, tuples and user-defined types are JSON, so
    the cell viewer's tree (N0b) browses them; a map's keys are JSON strings;
  - ⌘. stops waiting at once and frees the console; Cassandra finishes the statement or stops
    at its own timeout. On ScyllaDB, a `SELECT` without its own `USING TIMEOUT` gets the data
    source's statement timeout, when one is set, so the server stops too;
  - an error shows the server's message, and its position for a syntax error, which both
    servers give as a line and column;
  - **validate** prepares the statement, which checks it and its names without running it,
    so a misspelt column is caught before running, and pointed at;
  - `transaction_status` is always idle, and manual commit mode isn't offered;
  - the explorer lists the keyspaces and their tables; the rest comes with N4·4;
  - CI: ScyllaDB joins the compose file and the pull request suite; Cassandra joins it under
    a `nightly` profile, which a new nightly workflow starts to run the driver's and the
    engine's tests on it.

  Tests: the parts of the shared conformance suite that apply (lifecycle, connect failure,
  catalog walk, invalid paths), on each server; 1,200 rows paged in clustering order; every
  CQL type written as CQL reads it, with its type's name; a map column as JSON; TLS with the
  test authority; a misspelt column rejected by validate, with its position; a cancelled
  statement; an engine test of a console, and of a read-only data source refusing an `INSERT`;
  a kittest of the data source dialog. How it came out:
  - each console's session keeps one connection per node and doesn't fetch the cluster's
    schema, which the explorer reads from `system_schema` when asked; the shard-aware port is
    left alone, since one connection per node gains nothing from it;
  - `dbrover-tls` gained `client_config_for`, which checks a certificate against a name of the
    caller's choosing, since the `scylla` client names each node to rustls by its IP address;
  - ScyllaDB says "Unrecognized name" for a misspelt column when preparing, and Cassandra
    "Undefined column name"; validate points at the name either way;
  - `check_cancel` needs a statement that runs for seconds, which CQL has none of, so cancel is
    tested through a forwarding port that holds the node's answers back: the statement is
    stopped, and the connection answers again once they flow. That port is a tunnel's local
    end, so it tests reaching every node through a tunnel too;
  - ScyllaDB's test node also takes CQL over TLS on a port of its own, with the test
    authority's certificate, so TLS is tested on every pull request;
  - the grid shows CQL rows read-only until N4·5, and the statement timeout is a field of the
    Cassandra form alone.
- **N4·4 — The explorer** (built): keyspaces (the system ones only with **System** ticked), then
  folders of tables, materialized views, user-defined types, functions and aggregates; under a
  table, its columns (partition and clustering keys marked, in key order, then the rest) and
  secondary indexes. **Show definition** is the server's own `DESCRIBE` where it has one
  (ScyllaDB, and Cassandra 4.0 and later), and is rebuilt from `system_schema` otherwise.
  **Open data** opens a console with `SELECT * FROM ks.t` and runs it. Completion offers the
  keyspaces, tables and columns the explorer has loaded, as for SQL. Tests: the catalog walk
  with each kind, the definitions run again in an empty keyspace, and a column completed. How it
  came out:
  - the tree has the shape of MySQL's and ClickHouse's (a keyspace is a schema, with folders of
    tables and views, each with a folder of columns), so completion reads it as it does theirs,
    with no change; a user-defined type is `NodeKind::UserType` and an aggregate
    `NodeKind::Aggregate`, in folders of their own (`FolderKind::Types`, `Aggregates`);
  - both servers answer `DESCRIBE` as rows, `DESCRIBE ONLY KEYSPACE` included, so no definition
    is rebuilt from `system_schema`: Cassandra before 4.0, which hasn't it, is past its end of
    life, and gets an error rather than a guess;
  - ScyllaDB lists the view behind each secondary index in `system_schema.views`, named after
    the index with `_index`; the explorer leaves those out;
  - the test node of each server runs user-defined functions (Lua on ScyllaDB, Java on
    Cassandra) and, on Cassandra, materialized views, which both leave off by default, so the
    walk meets every kind; the driver's tests that make keyspaces or walk the catalog take a
    lock, since a walk that meets another test's keyspace while it is dropped fails;
  - the explorer's menu offered the SQL object helpers for a CQL table: it now asks
    `Dialect::writes_ddl` for them, and keeps **Open data** and **Export data…**.
- **N4·5 — Editing** (built): a cell changed in the grid becomes
  `UPDATE ks.t SET c = … WHERE` every partition and clustering key column `= …`, when the
  result is a `SELECT` of one table that holds its whole primary key; **Add row** becomes an
  `INSERT`, and **Delete rows** a `DELETE` by the same key. Values are written as literals of
  the column's CQL type; a collection, tuple or user-defined type typed as JSON is written
  with `fromJson()`. CQL has no transactions, so the grid's edits run one at a time, as
  Redis's and MongoDB's do: each is checked, logged and recorded in the history, and one that
  fails leaves the ones before it done, which the grid says. Tests: a cell of each type edited
  and read back, a row added and deleted, a key column refused, and kittests of the console's
  edits. How it came out:
  - the grid's SQL edits took CQL with three changes: `RowSql::for_table` writes each value as
    a literal of its column's type from the table's description (`dbrover-sql`'s `cql_value`),
    `QueryLanguage::edits_run` runs a dialect without transactions one statement at a time, and
    `Dialect::updates_keys` keeps a loaded row's key cells as they are;
  - a value written bare (a number, a UUID, a blob, a duration) is first checked to be one, and
    is sent as a string otherwise, so the server refuses the wrong type rather than read text
    from the grid as CQL;
  - the driver describes a table from `system_schema`, resolving an unqualified name in the
    session's keyspace and an unquoted one in lowercase, as CQL does; a materialized view isn't
    a table to describe, so its rows stay read-only;
  - the edits run as the SQL engines' do, the deletions, then the changes, then the new rows,
    each with the statement that would undo it in the history.
- **N4·6 — Safety, MCP and the manual** (built): the read-only level's dialog suggests a role with
  only the `SELECT` permission; the MCP server serves the data source only through its MCP
  login, its `query` tool takes one statement, and `list_objects` lists the explorer's levels.
  The engine count in `README.md`, `book/src/introduction.md` and `site/index.html` takes the
  new engines in, counted as #316 counts them: one more data source kind, a pair. Tests: on
  ScyllaDB, a role with `SELECT` alone serves as the MCP login, reads a table, and is refused
  an `INSERT` by the server as well as by the check. How it came out:
  - the guard, the advice and `list_objects`' paging were already every engine's, so this layer
    is the test binary `cassandra` of `dbrover-mcp`, the tools' descriptions, the manual's role
    and the counts; the test runs on Cassandra too, in the nightly job;
  - `explain` is refused on a CQL data source, which N4·2 made so; the test checks it;
  - the website lists Cassandra and ScyllaDB with Cassandra's logo, and counts ten databases.

N4 is built. Its exit was checked against the compose servers, ScyllaDB 2026.3.4 and Cassandra
5.0.9, by the tests each layer added: connecting to both (`connects_and_says_which_server_it_reached`);
browsing keyspaces and tables (`lists_every_kind_of_object_with_definitions_that_run_again`);
paging 1,200 rows (`pages_a_large_table_in_order`); a map column as JSON, which the cell
viewer's tree reads (`writes_every_type_as_cql_reads_it`, `runs_cql_and_shows_a_map_as_json`);
editing a cell (`edits_rows_by_their_primary_key`, `applies_cql_edits_one_at_a_time`); a
misspelt column caught by preparing (`validates_by_preparing`); and a read-only data source
refusing an `INSERT` (`a_read_only_cql_data_source_refuses_insert`, and the MCP test). What it
needs by hand, before release: a multi-node cluster across datacenters with **Local
datacenter** set, a cluster reached through an SSH tunnel, ScyllaDB Cloud's CQL endpoint with TLS, and
ScyllaDB's own node certificates with `verify-full`. DataStax Astra DB signs in with a client
certificate from its secure connect bundle, which DbRover doesn't take yet.

The manual changes in the layer that changes what a user sees: N4·3 (the data source, the
console and keyspaces in the explorer), N4·4 (the explorer), N4·5 (editing) and N4·6
(read-only data sources and MCP logins).

**CI**, measured when N4 started (2026-11-01), on a GitHub Linux runner (4 CPUs, 16 GB): the time
from `docker compose up -d --wait` to every service healthy, with the images already pulled.

| Services | Run 1 | Run 2 | Run 3 |
| --- | --- | --- | --- |
| Today's (PostgreSQL to OpenSearch) | 47 s | 48 s | 48 s |
| Today's and `scylladb/scylla:2026.3.4` | 51 s | 56 s | 56 s |
| Today's and `cassandra:5.0.9` | 88 s | 86 s | |
| `scylladb/scylla` alone | 7 s | 6 s | 6 s |
| `cassandra` alone | 70 s | 70 s | |

Pulling the images took 8 s for ScyllaDB and 5 s for Cassandra; ScyllaDB used about 100 MB and
Cassandra, with a 512 MB heap, about 1.2 GB. On a laptop (Docker Desktop), Cassandra answered
`cqlsh` 60 to 62 seconds after `docker run` and ScyllaDB after 3 to 4. So **ScyllaDB runs with
the pull request suite**, adding about 5 seconds to the Linux job, and **Cassandra runs nightly**,
in a workflow of its own that starts it and runs the CQL tests: on every pull request it would
add about 40 seconds. ScyllaDB runs as one node on one core (`--smp 1 --memory 1G
--overprovisioned 1 --developer-mode 1`), with password authentication; ScyllaDB 2026 makes no
default superuser, so the health check creates the test role through the node's maintenance
socket, which takes CQL without a login from inside the container only.

### N5 — DynamoDB

Moved out of "Later" on 2026-11-01, ahead of the rest of the [cloud track](cloud-data.md). It
brings the two parts of C0 it needs, cut down to DynamoDB's: the AWS sign-in, and a cost guard
against statements that read a whole table. Both are built as C0's, so C0 later adds Google
Cloud, Entra ID, byte limits and jobs beside them rather than doing them again. It takes the
next free number of this track, N5, since what it reuses most is the track's: a language beside
SQL (N0a), items as documents (N0b), and N2's and N3's layering. N4 and N5 touch different
crates and can be built at the same time.

- **Client: AWS's own SDK, `aws-sdk-dynamodb` (1.136) and `aws-config` (1.13),** Apache-2.0,
  on tokio. `aws-config`'s default credential chain is what signs in: the environment, the
  shared `config` and `credentials` files with a named profile, IAM Identity Center (SSO),
  `aws login`, `credential_process`, web identity, and container and instance roles, each
  refreshed by the SDK before it expires. Writing that chain again is most of the work and most
  of the risk; DynamoDB's own API is the small part. This departs from C0's proposed "no full
  SDKs" for AWS only, and the cost is measured below.
- **TLS on ring.** Every AWS crate is built without its default features, which choose
  aws-lc-rs. `aws-smithy-http-client`'s `rustls-ring` feature builds the SDK's HTTPS client on
  rustls with ring, and the client is told so by name
  (`tls_provider(Rustls(CryptoMode::Ring))`), so it never relies on rustls's process-wide
  provider. Checked with the current SDK: built on their own, the AWS crates have ring alone, and
  `cargo tree -i aws-lc-rs` finds nothing. aws-lc-rs is in the app's build already, turned on in
  the shared rustls by the SQL Server driver's `tiberius` (see the workspace `Cargo.toml`); no
  AWS crate turns it on, and N5·3 tests that.
- **What it costs,** measured on 2026-11-01 on the Apple M4 Pro of
  [Measurements](measurements.md): clean builds of `dbrover-app` with DuckDB's prebuilt library,
  as CI and releases build it, without and with a spike linked into the app that signs in
  through `aws-config` and calls the ten DynamoDB operations N5 uses:

  | | Without | With the AWS SDK | Difference |
  | --- | --- | --- | --- |
  | Release build, wall clock | 6 min 6 s | 6 min 28 s | +22 s (+6%) |
  | Release build, processor time | 18 min 34 s | 22 min 6 s | +3 min 32 s (+19%) |
  | Debug build (what CI's tests build), wall clock | 67 s | 66 s | none measurable |
  | Stripped release binary | 96.2 MB | 104.4 MB | +8.2 MB (+8.5%) |
  | Binary with `gzip -9` | 37.9 MB | 41.1 MB | +3.1 MB |

  The generated SDK crates compile beside the rest on twelve cores, so the build waits little
  for them; a first pair of release builds, with other work on the machine, took 3 min 41 s and
  3 min 23 s, so a few tens of seconds either way is noise. The downloads grow by about 3 MB
  for each architecture, about 6 MB for the universal macOS disk image: well inside the 30 MB
  budget [More databases](databases.md#decisions) set for DuckDB, which took 13 MB of it.
  `Cargo.lock` gains 46 crates, nearly all AWS's (the SDK's generated crates for DynamoDB, STS,
  SSO and sign-in, and its `aws-smithy-*` runtime); none links a C library.
- **Data source:** a profile, a region and an optional endpoint, and nothing secret:
  - **Profile**, from the shared AWS files, or empty for the default chain (the environment's
    keys, `AWS_PROFILE` or `default`, then a container's or instance's role). A named profile is
    used alone, as the AWS CLI's `--profile` is, so keys in the environment don't override the
    choice. DbRover keeps the profile's name only; the SDK reads the files, the SSO cache and
    the environment;
  - **Region**, or empty for the profile's or the environment's;
  - **Endpoint**, for DynamoDB Local, LocalStack or a VPC endpoint; empty for AWS's own;
  - **Sign in with** an AWS profile, or as a **local emulator**: fixed, made-up credentials
    that DynamoDB Local and LocalStack accept and AWS never would, offered only with an
    endpoint. They are what the tests use, so no test reads anyone's AWS files or the
    environment's credentials.

  Test connection lists one table, and reports DynamoDB with its region, and the endpoint when
  one is set.
- **Console: PartiQL,** DynamoDB's SQL-like language, through `ExecuteStatement`:
  `SELECT … FROM "table"` (or `"table"."index"`) `WHERE …`, `INSERT INTO … VALUE {…}`,
  `UPDATE … SET … REMOVE … WHERE …` and `DELETE FROM … WHERE …`, with DynamoDB's literals:
  `'strings'`, numbers, `{maps}`, `[lists]` and `<<sets>>`. A language of its own,
  `QueryLanguage::PartiQl`, rather than a SQL dialect: its literals, `VALUE` and its single-item
  writes don't parse as any SQL `sqlparser` reads, and SQL's formatter, linter, object helpers,
  CSV import and generated `UPDATE`s would each be wrong for it. Statements split at `;`.
- **Classification:** `SELECT` reads; `INSERT`, `UPDATE` and `DELETE` write; anything else,
  `EXISTS` (only valid in a transaction) included, is unclassified and refused where the safety
  checks apply. DynamoDB's `UPDATE` and `DELETE` act on one item, named by its whole primary
  key, so no write can touch everything and there is no warning to list for them; what can
  read everything is a `SELECT`, which the scan guard catches.
- **The scan guard,** C0's cost guard in DynamoDB's form, set per data source next to its safety
  level: **Ask before reading a whole table**, on by default for DynamoDB. A `SELECT` reads by
  key when its `WHERE` holds, joined by `AND` at the top, an equality (`=`) or `IN` between the
  partition key of the table or index it names and a value; anything else is a Scan, billed for
  every item read, whatever the `WHERE` keeps. Only the database knows a table's keys, so the
  check runs in the engine, through the driver, which describes the table (`DescribeTable`,
  kept for the connection's life) and asks `dbrover-language`'s PartiQL module, a pure function
  of the statement and the keys. Then:
  - a console asks first, naming the table and its partition key: **Scan anyway** runs it,
    **Cancel** doesn't. The statements before it have run; it and those after it run once
    allowed;
  - **Run to file** and **All documents** read every page, so they ask the same way;
  - the MCP server has nobody to ask, so it refuses a Scan with a message saying to name the
    partition key, unless the data source's guard is off;
  - **Open data**, the explorer's Scan, runs without asking, because it never reads past what
    is shown: each page reads at most 500 items, and the grid asks for the next only on
    **Load more**;
  - and whatever runs, a page is bounded: the driver sends `ExecuteStatement` with `Limit`
    500, so each request evaluates at most 500 items, and stops after 10 requests per page
    even when a filter kept fewer than 500. A filtered Scan shows a short page with **Load
    more** rather than reading on unseen.

  With the guard off, nothing asks, and the bound per page still holds. The explorer's other
  reads (`ListTables`, `DescribeTable`) are free. Showing what a statement read, in read units
  from `ConsumedCapacity`, waits for C0's status line for jobs.
- **Results:** items are documents (N0b), each attribute as Extended JSON: strings, numbers (as
  written, up to DynamoDB's 38 digits), booleans, `null`, maps and lists as JSON; binary as
  `$binary`; and string, number and binary sets as DbRover's own `{"$stringSet": […]}`,
  `{"$numberSet": […]}` and `{"$binarySet": […]}`, which ADR 0006 gains and the tree shows as
  PartiQL writes them (`<<'a', 'b'>>`), so an edited set goes back a set. A write's answer is
  its `RETURNING` items, or nothing. Paging is `ExecuteStatement`'s `NextToken`, as the grid
  asks for pages; the explorer pages tables with `ListTables`' `LastEvaluatedTableName`. Every
  read goes through PartiQL, Open data's included, so one path is classified, guarded and
  recorded in the history as typed; DbRover never calls `Scan` or `Query` with a
  `LastEvaluatedKey` of its own.
- **Explorer:** tables, paged by name 100 at a time with the filter applied by DbRover (AWS
  has no filter, and listing names is free), each with its approximate item count and size
  from `DescribeTable` (updated by AWS about every six hours, never counted). Under a table,
  its key attributes (partition and sort, with their types) and its global and local secondary
  indexes, each with its keys and projection. **Show definition** gives the table's
  `CreateTable` input as JSON, the form `aws dynamodb create-table --cli-input-json` takes.
  **Open data** on a table or index opens a console with `SELECT * FROM "table"` (or
  `"table"."index"`), run at once. Completion offers the loaded tables' names.
- **Cancel (⌘.):** DynamoDB has no way to stop a request it has received; each one is short
  (at most 1 MB read). ⌘. drops the request in flight and sends no more, so a Scan stops
  between requests and the result keeps the pages already read. A write dropped in flight may
  or may not have been applied; the log says so.
- **Editing:** an attribute changed in the grid becomes
  `UPDATE "table" SET "attr" = <value> WHERE <whole primary key>`, its value written in
  PartiQL with the type it had (a number stays a number, a set a set); a removed attribute
  `REMOVE`s it. **Add row** is an `INSERT … VALUE {…}`, **Delete rows** a `DELETE` by key. Each
  runs one at a time (`EditsRun::OneByOne`), checked, logged and recorded as typed commands
  are. Only a `SELECT` on one table (not an index, whose items may not be unique) is editable,
  and only items with the whole key.
- **No transactions** in the console (`ExecuteTransaction` takes a list of statements, which
  the console doesn't send), so manual commit mode isn't offered. **No explain and no validate**:
  DynamoDB plans nothing it can show, and can't check a statement without running it. Validate
  is the local parse, as for the other languages without SQL.
- **Safety and MCP:** no read-only session, so a read-only data source refuses writes with the
  check alone, and the dialog suggests a profile whose IAM policy allows only reads
  (`dynamodb:PartiQLSelect`, `dynamodb:DescribeTable`, `dynamodb:ListTables`). The MCP server
  serves a DynamoDB data source only through an **MCP profile**, a second AWS profile in the MCP
  login's place, as N0a's rule asks of every engine without a read-only session.
- **CI:** `amazon/dynamodb-local` 3.3.2 (241 MB compressed), in memory: healthy 3 seconds after
  `docker compose up`, using 120 MB of memory, measured locally. No
  AWS account anywhere: the tests sign in as a local emulator. DynamoDB Local doesn't check IAM,
  so the MCP profile's refusal by AWS is tried by hand, not in CI.

**Exit:** I connect to DynamoDB Local through its endpoint, and to AWS through a profile that
signs in with SSO; browse tables, their keys and indexes; run a `SELECT` by partition key and
page past 500 items; a `SELECT` on another attribute asks before scanning, and Open data reads
one page; an item opens as a tree, with a number set shown as a set; edit an attribute and see
it keep its type; ⌘. stops a Scan between pages; a read-only data source refuses an `UPDATE`;
and an MCP client reads a table through an MCP profile and is refused a Scan by the guard.
Everything but AWS itself is checked against DynamoDB Local; AWS, SSO and the MCP profile's IAM
refusal are tried by hand before release, with an account of the user's.

#### Layers

Each layer is one pull request in a stack, useful and tested on its own, as N2's and N3's were.
From N5·4 on, each runs against DynamoDB Local in CI.

- **N5·1 — Plan:** this section, the decisions N5 takes (in [Decisions](#decisions)), the cloud
  plan's C0 and the roadmap's status.
- **N5·2 — The PartiQL language** (built): a `partiql` module of `dbrover-language`, with no driver
  yet:
  - a lexer: `"names"` and unquoted names, `'strings'` with `''` for a quote, numbers, `?`,
    punctuation with `<<` and `>>`, and `--` and `/* */` comments;
  - a script splits at `;`;
  - a parser into DbRover's own statement: the verb, the table and index it names, and a
    `SELECT`'s `WHERE` as the conditions joined by `AND` at its top;
  - classification as above, and the access a `SELECT` makes given the keys of what it reads
    (`partiql::access`: by key, a query of one partition or more, or a scan), for the guard;
  - completion of the keywords and DynamoDB's functions (`begins_with`, `contains`,
    `attribute_type`, `size`, `exists`, `MISSING`).

  Tests: a classification corpus, as the other languages have; the splitter and parser against
  statements from DynamoDB's documentation; a table of `WHERE`s and the access each makes.
- **N5·3 — AWS sign-in** (built): a new crate, `dbrover-cloud`, C0's, with its `aws` module:
  `AwsSignIn` (a profile, or a local emulator), a region and an endpoint in, the SDK's
  configuration out, through `aws-config`'s default chain and the ring HTTPS client; no secret
  is read by DbRover or written anywhere. The data source types (`DynamoDbConfig`) are in
  `dbrover-core`. Tests: a profile, its region and its credentials read from files the test
  writes and passes in (never the user's), an emulator refused without an endpoint, and a
  check that no AWS crate brings aws-lc-rs.
- **N5·4 — The driver and the console** (built): a new crate, `dbrover-driver-dynamodb`, on
  `aws-sdk-dynamodb`, and `DriverKind::DynamoDb` through the core, the engine, the language and
  the app: the data source form (profile, region, endpoint, sign in with), its logo, each
  statement run through `ExecuteStatement` with the per-page bound and `NextToken` paging,
  items as documents with the set types in `dbrover-document`, ⌘. dropping the request, and the
  explorer listing tables. `docker-compose.yml` and `test-servers.env` gain DynamoDB Local.
  Tests: the parts of the shared conformance suite that apply (lifecycle, connect failure,
  catalog walk, invalid paths, cancel), each attribute type through to Extended JSON, a
  `SELECT` paged past 500 items, a filtered Scan's short pages, an engine test of a console,
  and a kittest of the form. How it came out:
  - the logo is DbRover's own, a cylinder with a bolt through it: Simple Icons carries no
    Amazon logo;
  - each document starts with the keys of the table (or index) it was read from, from
    `DescribeTable`, kept for the connection's life, then the rest by name, since DynamoDB keeps
    no order of attributes;
  - DynamoDB keeps all of a number's 38 digits but not a trailing zero, so `1.10` comes back
    `1.1`, and is shown as it comes;
  - a write answers nothing unless it says `RETURNING`, and then its items are documents;
  - Test connection reports DynamoDB with the region, and the endpoint when one is set;
  - `check_cancel` doesn't fit DynamoDB Local, which answers every request in milliseconds, so
    cancel is tested against a server that takes a request and never answers: the run stops
    at once with an error that says a request already received may still have been applied;
  - a read-only data source refuses `INSERT`, `UPDATE` and `DELETE` by the check alone. Until
    N5·8 gives it an MCP profile, the MCP server doesn't serve a DynamoDB data source: signed in
    by a profile, not a login with a password, it has no guard the server can count on.
- **N5·5 — The explorer** (built): keys, indexes, counts and sizes, **Show definition**, **Open
  data** on tables and indexes, tables paged 100 at a time, and table names in completion.
  Tests: the catalog walk with a table of each shape (partition key only, with a sort key, with
  a GSI and an LSI), the definitions, and paging past 100 tables. How it came out:
  - a table shows `≈ 1,204 items · 2.3 MB`; under it, its key attributes (`partition key · S`,
    `sort key · N`) and an **indexes** folder with its global and local indexes by name, each
    with its keys (`global · status, placed`);
  - a page lists every name (free, 100 a `ListTables` call), filters them, ignoring case, and
    describes only the page's own tables, all at once; a table dropped between the listing and
    its description is left out, and one dropped since the tree showed it is a node that
    disappeared;
  - **Show definition** can't run in a console, since PartiQL has no `CREATE TABLE`: it is the
    input of `aws dynamodb create-table --cli-input-json`, with the billing mode, the
    provisioned throughput when there is one, the indexes and their projections, the stream and
    deletion protection, and nothing AWS sets itself;
  - **Open data** on an index reads it through PartiQL's `"table"."index"`. Each language says
    what **Open data** runs on a node it reads whole in its own words
    (`QueryLanguage::read_all`), so the explorer offers it on DynamoDB's tables and indexes
    without the SQL dialect its tables would otherwise need;
  - the paging test makes 105 tables, filtered by a prefix unique to the run, since other
    tests' tables share the server.
- **N5·6 — The scan guard** (built): the guard on the data source (`CostGuard`, written only
  when it isn't the engine's default), `Connection::full_scan`, the engine's check before each
  statement and Run to file, the console's question, the MCP server's refusal, and the read
  units after each statement. Tests: the guard's answer for each kind of `SELECT` against
  DynamoDB Local's tables, an engine test of a Scan held until allowed, a kittest of the
  question, and the MCP refusal. How it came out:
  - the guard is a setting of the DynamoDB data source, **Ask before reading a whole table**,
    written `cost_guard = "off"` when turned off, rather than one of every data source: only
    DynamoDB bills reads yet, and `ConnectionConfig::cost_guard` is where C0 will find it for
    the engines that come next;
  - the engine asks the driver before each statement of a run (`Connection::full_scan`, which
    every engine but DynamoDB answers with `None`); a statement that scans isn't sent, the
    console learns which (`Event::ScanHeld`) and the run stops there, a failure that says why.
    The question is **Read all of orders?**, with the reason and **Scan anyway**, which runs
    that statement and the ones after it, allowed once (`ScanApproval::AllowFirst`): a later
    statement that scans too is asked about again. **Open data** runs allowed
    (`ScanApproval::Allow`);
  - **Run to file** and **All documents** refuse a Scan, rather than ask, saying to turn the
    guard off to export a whole table: they read every page by design, which is what the guard
    is there to stop;
  - the MCP server's refusal comes with N5·8: until DynamoDB has an MCP profile, the server
    doesn't serve it at all;
  - an unknown table, or a statement DbRover can't read, isn't held: DynamoDB's own error says
    more when it runs. A table whose keys can't be read (no `dynamodb:DescribeTable`) isn't run,
    and the error says why;
  - the read units after each statement wait for C0's status line for jobs, as the milestone
    says.
- **N5·7 — Editing** (built): `DocumentView` gains DynamoDB's `TableView`: keys from the
  table's description, values written in PartiQL with their types, and an undo for each change.
  Tests: an attribute of each type edited and read back with its type, a set edited, an item
  added and deleted, and kittests of the console's edits. How it came out:
  - a `SELECT` of one table is editable once the table is described, as a SQL table's rows are:
    the console asks for the table (`QueryLanguage::documents_table`), the driver's
    `describe_table` answers with its key, and the view is made from it
    (`QueryLanguage::described_view`). An index's items aren't editable, since two may share a
    key;
  - the key's attributes can't be typed over, as DynamoDB can't change an item's key; a changed
    attribute is `UPDATE … SET "attr" = …`, a map's inner attribute by its quoted path
    (`"address"."city"`), and an undo sets the old value back or `REMOVE`s an attribute the item
    didn't have;
  - over a set, a JSON list of its items' type is typed (`["a", "b"]`), and written `<<'a',
    'b'>>`; DynamoDB has no empty set, so an empty one is refused. PartiQL has no literal for a
    binary value, so a binary attribute can't be set from the grid, and an item holding one is
    deleted without an undo;
  - an added row needs its key's attributes typed; the rest are written with the types they
    read as;
  - every statement the view writes was run on DynamoDB Local in the tests, and each attribute
    read back with its type;
  - the result is read again after the edits, and that run isn't asked about by the scan guard:
    it is the statement the user already ran.
- **N5·8 — Safety, MCP and the manual** (built): the read-only dialog's advice, the MCP
  profile, the MCP test binary `dynamodb`, the manual's chapter on DynamoDB, and the engine
  count in the README, the manual's introduction and the website. How it came out:
  - an engine whose data sources sign in as someone whose grants the database enforces has
    logins (`DriverKind::has_logins`): those with a password, and DynamoDB, whose login is an
    AWS profile. Such an engine without a read-only mode is served to MCP clients only through
    an MCP login, so DynamoDB is now served, for reading, through its **MCP profile**, which
    needs no password of DbRover's; the dialog asks only for its name;
  - the MCP server refuses a statement that would read a whole table while the data source's
    guard is on, in `query` and `execute` alike: an MCP client can't be asked. This is the MCP
    half of N5·6's guard;
  - the MCP test runs on DynamoDB Local, both data sources signed in as an emulator: one with an
    MCP profile is served and reads by key, is refused a Scan by the guard and an `UPDATE` by the
    check; one without isn't served. DynamoDB Local doesn't check IAM, so the profile's policy
    refusing a write is tried by hand, with AWS;
  - the manual has a chapter of its own, **DynamoDB**, after the consoles', and the data
    sources and MCP chapters point to it. The engine count is eleven, with N4's Cassandra and
    ScyllaDB, which landed first.

Unlike N2's and N3's, the manual changes in the last layer, in one chapter, since N5 is
reviewed as a stack and the chapter reads better whole.

N5 is built. Its exit was checked against DynamoDB Local 3.3.2 on 2026-11-01, each part by a
test that runs in CI: connecting through an endpoint (the driver's lifecycle test); tables,
keys and indexes (the catalog tests); a `SELECT` by partition key paged past 500 items; a
filtered `SELECT` held until allowed, and **Open data** read a page at a time (the engine's
guard test and the console's); a number set read as `$numberSet` and shown as `<<…>>`
(`dbrover-document`); an attribute of each type edited and read back with its type; a
read-only data source refusing `UPDATE`; and an MCP client reading through an MCP profile and
refused a Scan. ⌘. was checked against a server that never answers, since DynamoDB Local
answers each request too quickly to be stopped part-way. What the exit needs by hand, before
release, with an AWS account: a profile that signs in with IAM Identity Center, and an MCP
profile whose IAM policy refuses a write by AWS itself.

### Later: Neo4j

- **Neo4j**, after ER1: Cypher in the console, results as rows of nodes and relationships, and a
  graph view on ER1's canvas. Its Rust client, `neo4rs`, is a Neo4j Labs project, marked
  experimental and unsupported, which speaks Bolt 4.0 to 4.3 only and has been in release
  candidates since 2025. Check it again before planning this.

## Decisions

The first four were agreed on 2026-10-29; the rest are proposed, and each is confirmed when its
milestone starts.

| Question | Decision | Consequence |
| --- | --- | --- |
| The order | **Redis, MongoDB, Elasticsearch and OpenSearch, then Cassandra and ScyllaDB.** Agreed. | Redis needs only N0a; N0b can be built beside it, ready for MongoDB. Elasticsearch comes before Cassandra because many more people use it. |
| MongoDB's console syntax | **A subset of the shell's, parsed by DbRover** with no JavaScript engine. Agreed. | Snippets from MongoDB's documentation run as they are, and every call can be classified. Scripts with variables, loops or functions don't run, and the error says so. |
| Documents and ADR 0004 | **Extend it with ADR 0006**: documents as canonical Extended JSON text, flattened in the grid, with a tree view. Agreed. | No driver's results change; SQL engines' JSON columns get the tree. Typed sorting and filtering of documents stay text-based, as for rows. |
| MCP from the first milestone | **Yes, through an MCP login only**: a Redis ACL user limited to reading, a MongoDB user with the `read` role. Agreed. | An assistant can read Redis and MongoDB data from N1 and N2, and the database enforces what the command check might miss. Without an MCP login, these data sources aren't served, and the log says why. |
| Valkey and Dragonfly | **The Redis driver**, one data source type named "Redis / Valkey". The connection reports which server it reached. Confirmed when N1 started. | No separate crates. Dragonfly is tried by hand, not in CI. |
| Redis Cluster and Sentinel | **After N1**: a single server, with TLS, ACL users and SSH tunnels, first. Confirmed when N1 started. | Most local and many managed servers are single; a cluster needs keys scanned on every node. |
| Which `redis` release (N1) | **1.8.1**, the latest release; 2.0 was still in release candidates when N1 started. Without its TLS features: DbRover opens the TCP or TLS stream and hands it to the client. | TLS is `dbrover-tls`'s, as for PostgreSQL and ClickHouse, with ring and the same modes, and nothing else in the build picks a crypto provider. 2.0 is taken in a change of its own once released. |
| Where classification comes from (N1) | **A table generated from Redis 8's and Valkey 9's command definitions,** shipped with DbRover, and checked in CI against each server's `COMMAND INFO`. | The check works before connecting, and the console completes from the same table. A module's command the table doesn't list is unclassified, so a read-only data source refuses it; the table is regenerated from a server with the module to add it. |
| What a key viewer is (N1) | **A console with the key's read command,** opened and run by **Open data**, with a key bar above its result, and edits that become commands. | Paging, the cell viewer's tree, copying and exporting, the safety level, the log and the history work as for any command, with no second path to keep in step. |
| Which `mongodb` release (N2) | **3.9.1**, the latest, with `bson` 3 (its `bson-3` feature, without the default `compat-3-0-0` that brings `bson` 2), `rustls-tls` on ring and `dns-resolver` for `mongodb+srv://`. | One `bson` in the build, and no second crypto provider. The driver opens its own sockets, so TLS is its rustls, not `dbrover-tls`'s: `require` allows any certificate, `verify-full` takes the CA file or Mozilla's authorities as `dbrover-tls` does, and `prefer` connects unencrypted, as on Redis, since a client can't try TLS and fall back on the same port. Through an SSH tunnel the driver would check the certificate's name against the tunnel's local end, and its rustls build can't skip that check, so `verify-full` is refused there with a message suggesting `require`. |
| A connection string or the form (N2) | **Either**: the shared server form, or a connection string pasted whole. The string is kept in the data source file without its password, which moves to the keychain when the data source is saved; `${NAME}` placeholders work in it. | Atlas's `mongodb+srv://` strings, replica sets and options such as `replicaSet` or `authMechanism` work as written. SSH tunnels only with the form: a string may name several hosts, or none (`+srv`). |
| Where the shell's syntax is read (N2) | **In `dbrover-language`'s `mongo` module, into DbRover's own tree of a command,** with no `bson` in that crate; the driver turns the tree into BSON. | Classification, warnings, completion and the MCP server read the same tree the driver runs, and the app needs no MongoDB crate. |
| Where a cancel comes from (N2) | **The console's own client**, a pool, which opens a second connection as the same user to send `$currentOp` (own operations only) and `killOp`. | Each console's cancel works without the explorer's session, as on the other engines, and any user may kill their own operations. |
| What an edit is (N2) | **A command per changed field, from the grid,** `updateOne` by `_id` with `$set` on the field's path, run one at a time as Redis's are. | Each edit is checked, logged and recorded as a typed command is. Editing in the tree waits: the grid's flattened columns, an expanded field's included, reach every field but an array's items. |
| One search driver or two | **One**, for Elasticsearch and OpenSearch, with their differences keyed off the server's answer to `GET /`. | One crate and data source type, "Elasticsearch / OpenSearch". Confirmed when N3 started: the two differ in the point-in-time endpoints (`_pit` against `_search/point_in_time`) and the SQL endpoint and its answer, nothing else N3 uses. |
| The HTTP client (N3) | **hyper, through `hyper-util`'s client, with `hyper-rustls` and `dbrover-tls`'s configuration,** as the ClickHouse driver's, HTTP/1.1 only. Bodies with `http-body-util`. No `reqwest`, and neither vendor's crate. | Nothing new is built: every one of these is in the build already, for ClickHouse. TLS has `dbrover-tls`'s modes, a CA file included, with ring. |
| The console's syntax (N3) | **Kibana's Dev Tools requests,** a method and path, then a body, **sent as typed.** DbRover reads the body only to classify it and to find a search's paging; it never writes it again, except to add a point in time, `search_after` and `size` to a search it pages. | Requests copied from either vendor's documentation run as they are, numbers keep every digit, and the body the server reads is the one on screen. Kibana's triple-quoted strings and variables aren't read, and the error says so. |
| How a search pages (N3) | **A search without `size` reads every hit,** as a `SELECT` without `LIMIT` does, through a point in time, `search_after` and `_shard_doc` as the tiebreaker, as the grid asks for pages. With `size`, at most that many; with `from`, `scroll` or aggregations, as typed, the answer whole. | The grid pages any search, and Run to file writes every hit. A search for its aggregations shows them as a tree rather than its first ten hits as rows. Kibana's habit, ten hits unless told otherwise, isn't DbRover's; the manual says so. |
| An API key (N3) | **In the password's place:** the data source says which it holds, and the key, Elasticsearch's encoded form, is kept in the keychain as a password is, and sent as `Authorization: ApiKey …`. | No second secret per data source: the keychain, password commands and `${NAME}` placeholders work for a key as they do for a password. |
| Cassandra's language | **A `Dialect` of `dbrover-sql`**, since CQL lexes and splits like SQL, with its own small classifier, as `sqlparser` has no CQL dialect. Confirmed when N4 started. | The SQL tooling (splitting, highlighting, completion) works for CQL with a new set of rules, not a new language. What only SQL has (the `sqlparser` syntax check, explain, sorting in the database, imports, the object helpers) asks the dialect and stays hidden for CQL. |
| Which `scylla` release (N4) | **1.9.0**, the latest, with its `rustls-023` feature, handed `dbrover-tls`'s ring configuration; ring is installed as rustls's process-wide provider, as for SQL Server. | That feature takes rustls with its default features, aws-lc-rs among them, which tiberius already builds, so no new crypto is compiled. A ring-only feature is still worth asking upstream for. |
| Where the client reaches nodes (N4) | **At the address the cluster gives each node when it answers within a second, and through the node the data source names otherwise;** through an SSH tunnel, every node through the tunnel. | A cluster in Docker or behind NAT works with no setting, and a reachable cluster keeps routing each request to a node that holds its rows. A node out of reach costs a second's probe once per session. |
| How ⌘. stops a CQL statement (N4) | **The client stops waiting; on ScyllaDB, a `SELECT` also runs with `USING TIMEOUT` from the data source's new Statement timeout,** when one is set. The protocol has no cancel. | The console is free at once. Cassandra keeps working until its own timeout (5 s for most reads), and so does ScyllaDB without a statement timeout; writes keep the servers' write timeouts. |
| CI for CQL (N4) | **ScyllaDB with every pull request, Cassandra nightly**, measured as above. | The Linux job grows by about 5 seconds. A change that breaks only Cassandra shows the next morning, not on its pull request. |
| Where languages live (N0a) | **A crate of their own, `dbrover-language`,** on top of `dbrover-sql`. It owns `QueryLanguage`; Redis's and MongoDB's languages become modules of it. `StatementKind` and `Unclassified` stay in `dbrover-sql` and are re-exported, and highlighting keeps `dbrover-sql`'s token kinds. | `dbrover-sql` stays about SQL. Code outside the crate asks a language through its methods and never matches on its variants, so a variant added for a new engine, or the test language, changes no other crate. |
| The test language (N0a) | **A variant behind a `test-language` feature,** turned on only in dev-dependencies. | It is compiled into tests only, yet it runs through the same console, check and warnings as a real language. |
| How a document travels (N0b) | **As canonical Extended JSON text, one row of one cell per document,** with the result's shape (rows or documents) from the cursor. See [ADR 0006](adr/0006-documents-as-extended-json-text.md). | Paging, cancelling, the history and the engine's events stay as they are. The console flattens documents, so no driver repeats it. |
| Where documents are read (N0b) | **A crate of their own, `dbrover-document`,** with DbRover's own JSON reader, which keeps fields in order and numbers as written, rather than `serde_json::Value`. | One tree for documents, SQL JSON columns and Redis's JSON. `serde_json` reads numbers into `f64` or 64-bit integers, which would change `1.10` to `1.1`, and its `arbitrary_precision` would change every crate's JSON numbers. |
| Run to file and table exports of documents (N0b) | **Left to N2,** which built them as JSON and JSON Lines only: they write rows as the cursor gives them, before any column of a document is known. | N0b exports documents from the grid: what is loaded. N2 adds writing every document to JSON and JSON Lines straight from the cursor. |
| Engines without a read-only session (N0a) | **The login is the guard:** SQL Server and ClickHouse data sources are served to MCP clients, for reading, only with an MCP login. Changes through `execute` wait for a transaction DbRover can open on them. | W11's plan for SQL Server, here for every such engine with logins. W11 still gives ClickHouse its `readonly` setting and DuckDB its read-only file, which are stronger. |
| Where DynamoDB goes (N5) | **Ahead of C0, as this track's N5,** with C0's AWS sign-in and a cost guard cut down to what DynamoDB needs, each built as C0's. Agreed on 2026-11-01. | DynamoDB doesn't wait for Google Cloud, Entra ID, byte limits or jobs. C0 later adds them beside `dbrover-cloud`'s `aws` module and the data source's `CostGuard`, rather than replacing them. |
| AWS's SDK or a small client (N5) | **`aws-sdk-dynamodb` and `aws-config`,** without default features, on ring. Agreed on 2026-11-01; what it costs is measured under N5. | The credential chain (SSO, `aws login`, profiles, roles, refreshing) is AWS's own rather than DbRover's. C0's "no full SDKs" stands for Google Cloud and Azure; for AWS, later milestones reuse `aws-config` (Athena's SDK, RDS tokens through `aws-sigv4`, already in the build). |
| How DynamoDB signs in (N5) | **A profile and a region, or the default chain when empty; an endpoint to override AWS's.** For DynamoDB Local and LocalStack, a **local emulator** sign-in with fixed, made-up credentials, offered only with an endpoint. | DbRover stores only names: the profile, the region and the endpoint. The tests sign in as an emulator, so they never read anyone's AWS files or need an account. |
| PartiQL: a language or a SQL dialect (N5) | **A language of its own, `QueryLanguage::PartiQl`,** like Redis's and MongoDB's, not a `Dialect`. | The SQL features that would be wrong for PartiQL (formatting, the linter, object helpers, CSV import, generated `UPDATE`s) stay hidden without each checking the engine. Highlighting reuses `dbrover-sql`'s token kinds. |
| What a Scan is, and who decides (N5) | **A `SELECT` without `=` or `IN` on the partition key of what it reads, at the top of its `WHERE`,** decided in the engine before the statement runs, by the driver from the table's keys and `dbrover-language`'s pure check. An `OR` at the top counts as a Scan. | The guard needs no catalog in the app and works the same for consoles, Run to file and the MCP server. An `OR` of two keys asks needlessly, which is safer than a Scan that doesn't. |
| What the guard does (N5) | **Asks first in a console, Run to file and All documents; refuses in the MCP server; lets Open data run, a page at a time.** Every page is bounded: `Limit` 500 per request, 10 requests per page. On by default for DynamoDB, a setting per data source. | Nothing DbRover runs on its own reads a whole table, and a filtered Scan never reads on unseen. The question is one more click for someone who meant to scan. |
| Sets in documents (N5) | **DbRover's own Extended JSON types,** `$stringSet`, `$numberSet` and `$binarySet`, shown as PartiQL's `<<…>>`. | An edited set goes back as a set, not a list. Other tools reading an export see an object with one `$` field, as they do for MongoDB's types. |

## Risks

- **A shell subset that feels broken.** People paste mongosh scripts with `const`, loops and
  `printjson`. The parser names what it doesn't support and where, and the manual lists what
  runs. If the subset proves too small, a sandboxed JavaScript engine can be weighed again, with
  its cost to the binary and to classification.
- **Commands the check can't see into.** Redis's `EVAL` and `FCALL` run scripts, MongoDB's
  `$function` and `$where` run JavaScript, and an Elasticsearch ingest pipeline can change what
  it indexes. Scripts count as writes unless they use the read-only forms (`EVAL_RO`,
  `FCALL_RO`), and only a login with read-only grants is airtight, as for the SQL engines beyond
  reading. The docs say so plainly.
- **Large keyspaces and collections.** A careless `KEYS *` or a count over a whole collection
  stalls a production server. The explorer only ever scans in pages, counts come from the
  server's own estimates, and the warnings name the commands that read everything.
- **Documents with thousands of fields.** Flattening caps the columns, and the tree shows the
  rest; the grid never grows past what it can draw.
- **CI time.** Redis, Valkey and MongoDB are small and start in seconds. Elasticsearch,
  OpenSearch and Cassandra run on the JVM and take longer; each milestone measures its
  container, and anything that pushes the Linux job past about 15 minutes runs in a separate
  job or nightly instead (see each milestone's CI line).
- **AWS's SDK.** It releases every week, and each release of `aws-sdk-dynamodb` is a large
  generated crate. The workspace pins its minor versions, and moving them is a change of its
  own, checked for aws-lc-rs as N5·3's test does. A bill is the other risk: N5's scan guard is
  on by default, and the manual says what DynamoDB charges a Scan for.
- **Scope creep toward the vendors' tools.** Each engine's own tool has monitoring, profiling
  and administration. This track stops at browsing, querying and editing, so that each engine
  stays a milestone, not a product.
