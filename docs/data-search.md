# Searching a database's data

Status: **planned on 2026-11-25** (W16a), its open questions answered on 2026-11-26, all nine
as proposed. The [roadmap](roadmap.md#well-rounded-track) tracks it as W16 in the
well-rounded track. It starts with PostgreSQL, SQLite and DuckDB, then MySQL and MariaDB, SQL
Server and ClickHouse, then the engines that bill each read, which ask first. Each layer's
decisions are confirmed as it is built, and departures go in the demo note.

A support ticket quotes an order reference, `ORD-7731-K`, and nothing else. A customer's email
address has to be found everywhere it is kept before it is deleted. A UUID turns up in a log
line, and no one remembers which table's key it is. Today DbRover can't help with any of this:
finding out means guessing a table, opening its data, filtering it, and doing it again for the
next table, or writing a `UNION ALL` of `ILIKE`s by hand from the column list. DbRover's own
searches find names (the explorer's filter and C0g's catalog search) and statements (the
history's search, W4d), never what the tables hold.

W16 adds **Search data**: pick a data source, a schema or some tables, type a value, and
DbRover finds the rows that hold it, table by table, column by column. The matches are grouped
by table, and each one opens in a console on that table's row. The search only reads, under a
time limit per table, and asks first wherever reading costs money.

## The question

Three questions come up, in this order of frequency:

1. **"Where is this value?"** A reference, an email address, a name, a UUID: which tables and
   columns hold it, and in which rows.
2. **"Show me that row."** Open the row the match is in, with all its columns, editable when
   the table has a key, and follow its foreign keys from there (W5b).
3. **"Everywhere it is."** Every row that holds it in one table, for a clean-up or an export.

Other tools answer them in a similar shape. DBeaver's **Find data in database** is a dialog: the
objects to search on one side, the text and options on the other (case, numbers, large objects,
a cap on results per table, only indexed columns), and the results as a list of tables, each of
which opens the table's data filtered to the matches. DataGrip's full-text search in data
sources takes a text and a set of data sources or tables and lists the hits by table in its find
window, each opening the data editor on the row. TablePlus and DBX search data too, with fewer
options; their details were not read for this plan. DbRover's version follows the shape they
share (a scope, a text, a few options, results by table, a match opening its row) and keeps to
its own rules: the statements are shown, the search only reads and is enforced read-only, a
limit and Cancel keep it from running away, and an engine that bills each read asks first.

## What it looks like

Right-click a data source, a database, a schema or a table → **Search data…** opens a tab,
*Search in* the data source, beside the consoles, as History and Server activity do (D8). One per
data source: opening it again focuses it, with the scope set to what was right-clicked.

```text
+-------------------------------------------------------------------------------------------+
| Search in production                                                                       |
| [ o'brien                              ]  [Search]  [Cancel]                               |
| In: shop › public (38 tables) [Change…]   Match: (•) Contains ( ) Whole value  [ ] Case    |
| Searched 38 tables in 0:41 · 4 with matches · 2 timed out                                  |
+-------------------------------------------------------------------------------------------+
| ▾ customers                     2 rows   in name, email                         [Open all] |
|     id 1042   name  Kate O'Brien         email  kate.obrien@example.com                    |
|     id 2211   name  Liam O'Brien-Hale    email  liam@example.org                           |
| ▾ notes                         1 row    in body                                [Open all] |
|     id 88     body  …called O'Brien about the refund, asked to…                            |
| ▸ audit_log                   100+ rows  in payload                             [Open all] |
| ▸ orders                        3 rows   in shipping_name                       [Open all] |
| ⏱ invoices      timed out after 15 s (searched 4 columns)                       [Retry]   |
| ⏱ events        timed out after 15 s (searched 2 columns)                       [Retry]   |
| · 25 tables with no match · 7 with no column that can hold the value      [Show them]      |
+-------------------------------------------------------------------------------------------+
```

- **The scope** is what was right-clicked: the whole data source (every schema the explorer
  shows, system schemas left out), a database, a schema, or one table. **Change…** opens the
  catalog's tables with tick boxes and a filter, to search some tables only. Views are left
  out, since they read their tables again, often with joins; **Include views** adds them.
- **The value** is typed as it is, with no quoting or wildcards: `%` and `_` are searched for
  as themselves.
- **Match:** **Contains** (the default) or **Whole value**, and **Case** to match case. Numbers,
  UUIDs and dates are always matched as whole values (see [How a search is
  shaped](#how-a-search-is-shaped)).
- **Results by table,** in the order they finish: the table, how many rows matched (*100+*
  past the cap), and in which columns. Expanding a table lists its rows as the key's columns
  and the columns that matched, the match highlighted and long values cut around it.
- **A row opens** with a double-click or **Open row**: a new console with `SELECT * FROM
  "public"."customers" WHERE "id" = 1042`, run, as **Open the row it refers to** does (W5b). The
  grid is editable when the table has a key, under the data source's safety level. A table
  without a key opens its matching rows instead.
- **Open all** opens a console with the table's search statement without its cap, so every
  matching row pages in 500 at a time, can be exported, or counted.
- **What was not searched is said:** tables that timed out or failed (with the server's
  message and **Retry**), tables with no column that can hold the value, and, once the search
  ends, the tables with no match, folded into one line.
- **The statements are shown:** a table's menu has **Copy statement** and **Open statement**
  (in a console, not run).

The tab isn't restored at start-up, and the last search's results stay in the tab until another
search replaces them. A search runs on a connection of its own, opened for the search and
closed when it ends, so it never waits behind the explorer or a console, nor holds them up.

## How a search is shaped

One statement per table, built by `dbrover-sql` from the table's columns, the value, and the
options. A table's statement looks only at the columns whose type can hold the value, reads
only the key and those columns, and stops at the cap:

```sql
SELECT "id", "name", "email", "phone"
FROM "public"."customers"
WHERE "name" ILIKE '%o''brien%' ESCAPE '!'
   OR "email" ILIKE '%o''brien%' ESCAPE '!'
   OR "phone" ILIKE '%o''brien%' ESCAPE '!'
LIMIT 101
```

### Which columns, by what the value can be

The value is read once, before the search, as everything it could be:

| The value reads as | Example | Searched in |
| --- | --- | --- |
| Text, always | `O'Brien`, `ORD-7731-K` | text columns (`text`, `varchar`, `char`, `citext`, `nvarchar`, ClickHouse's `String`, enums), as Contains or Whole value |
| A whole number | `1042`, `-7` | integer and decimal columns too, `= 1042`, where the number fits the column's type |
| A decimal number | `42.50` | decimal columns, `= 42.50`, and float columns, as the database compares them |
| A UUID | `3f2c…-…` in any case, with or without braces | UUID columns too (`uuid`, `uniqueidentifier`, ClickHouse's and DuckDB's `UUID`), `= '3f2c…'` |
| A date | `2026-11-25` | date columns, `= DATE '2026-11-25'`, and timestamp columns on that day, `>= '2026-11-25' AND < '2026-11-26'` |

- **Text is searched in text columns only.** Casting every column to text and searching it
  (as the grid's in-database filter does, `contains_text`) would find `1042` inside
  `21042`, read every number and date as text, and keep any index from being used. Numbers
  and UUIDs are compared as what they are, which an index on them can answer.
- **A number is still text too:** `1042` is also searched in text columns, where phone numbers
  and references are kept.
- **A value that can't fit a column skips it:** `300` isn't compared with a `tinyint`, nor
  `3000000000` with an `int`, so no statement fails on an overflow.
- **JSON and XML** (`json`, `jsonb`, MySQL's `JSON`, SQL Server's `xml`) are searched as their
  text with **Contains**, cast as each engine writes them, since values are often kept inside a
  document. They are the slowest columns to search, and the tab says when a table's only
  match was in one. (Question 3.)
- **Never searched:** binary columns (`bytea`, `blob`, `varbinary`), arrays and maps, geometry,
  booleans (`true` would match every other row), intervals, and columns the driver can't type.
  SQLite's columns hold any type whatever they declare, so a column declared without a type,
  or with one SQLite reads as text, is searched as text.

`Dialect::search_kind(type_name)` maps each engine's type names to these kinds, from the same
spellings as `Dialect::value_kind` (which the exports use), plus UUIDs, JSON and what is never
searched.

### Contains, whole value, and case

| | Ignoring case (default) | Matching case |
| --- | --- | --- |
| **Contains** | `ILIKE '%v%'`, or the engine's case-insensitive equivalent | `LIKE '%v%'` under a case-sensitive comparison |
| **Whole value** | `ILIKE 'v'` (no wildcards), or `=` under a case-insensitive collation | `=` |

- **`LIKE`, not a position function,** for text: PostgreSQL's trigram indexes (`pg_trgm`) answer
  `ILIKE '%v%'`, ClickHouse's `ngrambf_v1` skip indexes answer `LIKE`, Snowflake's search
  optimisation answers `LIKE` and `ILIKE`, and a **Whole value** search with **Case** is a plain
  `=` any index on the column answers. `strpos`, `CHARINDEX` and `LOCATE`, which the grid's
  filter uses, are answered by none of them.
- **Ignoring case follows the engine:** PostgreSQL, DuckDB, ClickHouse, Snowflake and Databricks
  have `ILIKE`; MySQL, MariaDB and SQL Server compare under the column's collation, which is
  case-insensitive by default (`utf8mb4_0900_ai_ci`, `SQL_Latin1_General_CP1_CI_AS`), so plain
  `LIKE` already ignores case there, and accents too on MySQL's `_ai_` collations; Trino and
  BigQuery have neither, so they compare `lower(col)` with the lower-cased value. A column whose
  collation is case-sensitive (`_bin`, `_cs`) is searched with `LOWER()` on both sides on MySQL
  and SQL Server, so **ignoring case** means the same on every column.
- **SQLite folds ASCII only:** its `LIKE` ignores the case of `A` to `Z` and nothing else, and
  its `lower()` likewise, unless the ICU extension is loaded. `o'brien` finds `O'Brien` but
  `élan` doesn't find `Élan`. The manual says so.
- **Matching case:** PostgreSQL, DuckDB, ClickHouse, Trino, BigQuery, Snowflake and Databricks
  have a case-sensitive `LIKE`; SQLite uses `instr(col, 'v') > 0` (Contains) or `=` (Whole
  value), since its `LIKE` can't be made case-sensitive per statement; MySQL and MariaDB add
  `COLLATE <charset>_bin` for the column's own character set; SQL Server adds `COLLATE
  Latin1_General_100_BIN2`.
- **Whole value and padding:** MySQL's and SQL Server's `=` ignore trailing spaces (`PAD SPACE`
  collations), and a `char(10)` column is padded on PostgreSQL. **Whole value** finds `abc` in
  `abc   ` there, as the database itself would.

### Escaping, two steps

The value is written into the statement in two steps, each the dialect's own:

1. **The `LIKE` pattern:** `%`, `_` and the escape character itself are escaped, so they match
   themselves; on SQL Server `[` too, which opens a character class in its `LIKE`. Then `%` is
   added on each side for **Contains**.
2. **The string literal:** the pattern is written as a literal by `Quoting::string_literal`, which
   already doubles quotes and, where the dialect reads backslashes in strings, backslashes.

| Dialect | Escape character | How it is said |
| --- | --- | --- |
| PostgreSQL, SQLite, DuckDB, Trino, Snowflake, Databricks | `!` | `ESCAPE '!'` |
| MySQL, MariaDB | `!` | `ESCAPE '!'` |
| SQL Server | `!` (and `[` escaped as `![`) | `ESCAPE '!'` |
| ClickHouse | `\` | none: ClickHouse's `LIKE` has no `ESCAPE`, and `\` is its escape |
| BigQuery | `\` | none: GoogleSQL's `LIKE` has no `ESCAPE`, and `\` is its escape |

- **Why `!` where there is a choice:** a backslash is also an escape inside MySQL's, MariaDB's,
  Snowflake's and Databricks's string literals (unless MySQL's `NO_BACKSLASH_ESCAPES` is set,
  which DbRover can't know before it asks), so `\` as the `LIKE` escape would mean different
  things on the same engine depending on a server setting. `!` means nothing in any literal.
  SQLite's `LIKE` has no escape at all unless one is named, so naming one is needed there
  anyway.
- **Where there is no choice** (ClickHouse, BigQuery), the backslash added in step 1 is doubled
  by step 2, since both dialects read backslashes in literals: `50%_off` is written
  `'%50\\%\\_off%'`.
- `core::like_contains`, which C0g's catalog search uses with `\` on PostgreSQL and Redshift,
  stays as it is; W16's pattern builder takes the escape character from the dialect and is
  golden tested on every dialect with `%`, `_`, `!`, `\`, `[`, `'` and a value of nothing but
  them.

**Bound values or literals:** the statements are written with literals, not bound parameters,
so the statement shown, copied, opened and kept in the history is the one that ran. The
literal step is the one every dialect's DML builder already uses for values (`RowSql`, W5),
and the value never reaches the statement any other way.

### Native full-text search (later)

Several engines index words, and answer a word search from the index in milliseconds where
`ILIKE '%v%'` reads the whole table: PostgreSQL's `tsvector` columns and `to_tsvector`
expression indexes (`@@ websearch_to_tsquery(…)`), MySQL's and MariaDB's `FULLTEXT` indexes
(`MATCH (…) AGAINST (… IN NATURAL LANGUAGE MODE)`), SQL Server's full-text indexes
(`CONTAINS`), SQLite's FTS5 tables (`MATCH`), ClickHouse's `tokenbf_v1` and text indexes
(`hasToken`), BigQuery's search indexes (`SEARCH(t, 'v')`), Snowflake's search optimisation
(`SEARCH`), and Elasticsearch and OpenSearch, which are nothing else.

They answer a different question: whole words, stemmed (`refund` finds `refunded` on
PostgreSQL's `english` configuration but not `ref`), with a language and stop words, and only on
the columns someone indexed. Mixed into **Contains**, they would make results depend on indexes
the user can't see. So W16 searches with `LIKE`, and a **Words** match that uses the engine's
word index, only on the columns that have one, and says which those are, is left for a
following milestone (question 9). The column reading in W16c already records which columns a
word index covers, so that milestone adds a match kind and statements, not a second catalog
read.

## Limits

A search over a production database must end, and must not take the server with it.

- **One table at a time,** on the search's own connection. A search never runs more than one
  statement on the server at once, whatever the scope's size.
- **Smallest tables first,** by the row estimate the catalog already keeps (`pg_class.reltuples`,
  `information_schema.TABLES.TABLE_ROWS`, `sys.partitions`, `system.tables.total_rows`,
  DuckDB's `estimated_size`), so most matches show in the first seconds and the large tables,
  most likely to time out, come last. Where there is no estimate, by name.
- **A time limit per table, 15 s by default,** set as the engine's own where it has one
  (PostgreSQL's `SET LOCAL statement_timeout` inside the read-only transaction, MySQL's
  `MAX_EXECUTION_TIME` hint, MariaDB's `SET STATEMENT max_statement_time = … FOR`, ClickHouse's
  `max_execution_time`, Trino's `query_max_run_time`, BigQuery's `jobTimeoutMs`, Snowflake's
  `timeout`), and by the driver's cancel after the limit elsewhere (SQL Server, SQLite, DuckDB,
  Databricks), as W15's reads are. A table that runs out is listed as **timed out**, with
  **Retry** (which runs it alone, with no limit, after asking), and the search goes on to the
  next.
- **A cap on rows per table, 100 by default:** the statement asks for 101 rows and shows *100+*
  if the 101st comes. There is no `ORDER BY`, which would make the server find and sort every
  match before returning the first, so the rows shown are the first the server found.
  **Open all** reads the rest.
- **Cancel** stops the statement running (the driver's `cancel_handle`, as a console's Cancel)
  and the search with it; what was found so far stays, and the tables not searched are listed
  as such. Closing the tab cancels too.
- **Progress:** *Searched 31 of 38 tables*, the table being searched, the time so far, and each
  table's result as it comes. The tab's own title shows a spinner while a search runs, as a
  console's does.
- **Settings › Data search** sets the time limit (1 s to 10 min, or none), the rows per table
  (10 to 1,000) and whether views are included by default, saved in `settings.toml` as
  `[data_search] table_seconds`, `rows_per_table` and `include_views`.

## Safety and cost

### Reads only, enforced

Every statement the search sends is one `dbrover-sql` built, a `SELECT`, and it runs in the
engine's read-only mode (W11's `read_only::mode`: a read-only transaction rolled back after,
a connection opened read-only, or ClickHouse's `readonly` setting) **on every data source**, not
only read-only ones. A search can't write even through a function a view calls.

- **Read-only data sources** can be searched: the search changes nothing. Opening a row opens
  it read-only, as every console of theirs does.
- **Careful data sources** ask before a search of more than one table, naming the data source
  and the scope: *Search 38 tables of "production" for "o'brien"? This reads each table, up to
  15 s each.* A search of one table runs at once. (Question 5.)
- **The MCP login**, where a data source has one, isn't used: the tab searches as the user, as
  the explorer browses.

### Engines that bill each read

BigQuery and Athena bill the bytes a statement scans, and a `LIKE '%v%'` scans every byte of
each column it names, whatever it finds and whatever its `LIMIT`. Snowflake and Databricks bill
the time their warehouse runs, and may start a suspended warehouse to answer. Redshift
Serverless bills the compute its statements use. A whole-database search there can cost real
money, so it never starts without a question, whatever the data source's cost guard says:

| Engine | What the question says | How it knows |
| --- | --- | --- |
| BigQuery | *Scan 34.2 GB across 12 tables?* with the largest tables listed | a dry run of each table's statement (C4's `dry_run`), summed; tables whose dry run fails are listed |
| Athena | *Scan up to 12 tables, of unknown size?*, with the workgroup's per-query cutoff if it has one | Athena has no dry run; Glue's table statistics where they exist (*to check*) |
| Snowflake | *Run up to 12 statements on warehouse COMPUTE_WH, up to 15 s each?*, and whether it is suspended | the per-table limit, sent as `timeout` (C5f) |
| Databricks | as Snowflake, naming the SQL warehouse | the per-table limit, kept by cancelling (C6f) |
| Redshift | not searched in W16 (see [Engines](#engines)) | |

- **The scope there starts small:** on these engines **Search data…** is offered on a schema
  (dataset) or a table, not on the whole data source, so a click doesn't scan a project.
- **A data source's cost guard** (C0e) still applies to each table's statement: a statement
  over its `ask_above_bytes` is held, and the question above lists those tables first.
  **Cost guard off** removes nothing from the question: a search reads many tables at once,
  which no setting about one statement was set for. (Question 6.)
- **Partitioned tables** that require a partition filter (BigQuery's `require_partition_filter`)
  refuse a search without one; they are listed as failed with the server's message. A
  partition filter in the search is left for later.
- **The MCP server never asks,** so it refuses a search on these engines (see [MCP](#mcp)).

## Recorded in the history

Each search is recorded **once**, in the query history (W4), with a new origin, **Search**: when,
how long, how it ended (done, cancelled, failed), and, in `detail`, what was searched and where
(*Searched 38 tables of shop.public for "o'brien", contains, ignoring case: 4 tables matched, 2
timed out*). Its statement is the statements of the tables that matched, one per line, so
**Run again** in History finds the same rows. The tables that found nothing aren't kept: they
would bury the statements that matter.

- The value is kept as typed, as a console's statement is: the history is the user's own, on
  their machine, and redacts only passwords (W4b). A search for a password-looking value is
  redacted like one.
- Older DbRovers sharing the file read the row's `source`, `app`, as they read every other.
- The History tab gets the **Search** badge and filter. (Question 7.)

## MCP

One tool, **`search_data`**, in a layer of its own (question 8):

- **Arguments:** a data source, the value, optionally schemas or tables, `match` (`contains` or
  `whole`), `case_sensitive`. **Answers** with each table that matched, its matched columns, and
  its rows (key and matched columns), then the tables that timed out, failed or were skipped.
- **Behind `read`:** `query` can already run each statement, so a new permission would guard
  nothing. The project's **schemas** scope narrows the search, its **row limit** caps the rows
  over all tables, and its **statement time limit** is the per-table limit.
- **Refused** on the engines that bill each read, since there is no one to ask, with the reason
  and the statement a client can run itself through `query` under the cost guard's rules.
- **Through the MCP login,** when the data source has one, as every MCP statement runs.
- **Recorded** once in the history as the MCP client's, with what was searched in `detail`.

## Engines

### First: the SQL engines CI already starts

| Engine | Columns and keys read from | Ignoring case | Time limit | Notes |
| --- | --- | --- | --- | --- |
| PostgreSQL | `pg_attribute`, `pg_type` (enums by `typtype = 'e'`), `pg_index` for the key, `pg_class.reltuples` | `ILIKE` | `SET LOCAL statement_timeout` | `citext` compares without case already; `jsonb::text`; Supabase, Neon, RDS, Aurora, Cloud SQL and Azure alike |
| SQLite | `pragma_table_xinfo`, `pragma_index_list` | `LIKE` (ASCII) | the driver's interrupt | no row estimate: by name; `WITHOUT ROWID` tables by their key |
| DuckDB | `duckdb_columns()`, `duckdb_constraints()`, `estimated_size` | `ILIKE` | the driver's interrupt | attached files and lakes (RS7) are tables like any other |
| MySQL | `information_schema.COLUMNS` (with `COLLATION_NAME`), `KEY_COLUMN_USAGE`, `TABLES.TABLE_ROWS` | the collation's | `/*+ MAX_EXECUTION_TIME(ms) */` | `ENUM` and `SET` as text; `JSON` cast to text |
| MariaDB | as MySQL | the collation's | `SET STATEMENT max_statement_time = s FOR …` | `JSON` is `longtext` there |
| SQL Server | `sys.columns`, `sys.types`, `sys.indexes` (`is_primary_key`), `sys.partitions` | the collation's | the driver's cancel | literals written `N'…'` for `nchar`/`nvarchar`, plain for `varchar`, so no column is converted; `xml` cast to `nvarchar(max)` |
| ClickHouse | `system.columns`, `system.tables.total_rows` | `ILIKE` | `SETTINGS max_execution_time` | `LowCardinality(…)` and `Nullable(…)` unwrapped; no key, so a row opens by its matching columns' values |

These run against the containers `docker-compose.yml` already starts for per-pull-request CI
(PostgreSQL, MySQL, MariaDB, SQL Server, ClickHouse), and SQLite and DuckDB in the process. No
service is added.

### Then: the engines that bill each read, and Trino

Trino, Athena, BigQuery, Snowflake and Databricks speak SQL through the same core, so the
statements are `dbrover-sql`'s and the work is the columns' reading (`information_schema.columns`
on each), the question above, and the time limits. Trino's and BigQuery's containers run in the
nightly workflow; Athena, Snowflake and Databricks are tested on recorded exchanges, as C3, C5
and C6 are. **Redshift**, reached through the PostgreSQL driver, reads columns from
`SVV_REDSHIFT_COLUMNS` and bills Serverless compute; `ServerInfo::service` turns the search off
there in W16, as it turns off Server activity, and C7 (#693), which is about Redshift, is where
it belongs.

### Later, or not at all

- **Elasticsearch and OpenSearch** are search engines: one `_search` with a `multi_match` or
  `simple_query_string` over an index pattern answers the whole question natively, from the
  index, with hits grouped by index. A natural next step, with its own query and escaping of
  the query syntax's reserved characters, but not `dbrover-sql`'s statements.
- **MongoDB:** each collection's fields aren't declared, so a search would sample documents for
  their string fields (as its explorer would), then `find` with a `$regex` (escaped, `i` for
  case) on each, or `$text` where a collection has a text index; Atlas Search's `$search` only on
  Atlas. A layer of its own, after Elasticsearch.
- **Redis and Valkey:** values aren't indexed, so a search reads every key with `SCAN` and
  every value with a read per type; on a large keyspace that is the server's whole memory.
  Left out; RediSearch's `FT.SEARCH`, where the module is loaded, is the later answer. Keys are
  already found by name in the explorer.
- **Cassandra and ScyllaDB** can't filter on a column that isn't a key without `ALLOW
  FILTERING`, a scan of every node. **DynamoDB** bills each item a Scan reads. Both are left
  out.

## How it's built

- **`dbrover-core`:** the model: `DataSearch` (the value, `MatchKind::{Contains, WholeValue}`,
  `CaseMatch`, the scope, the limits), `SearchScope::{DataSource, Database, Schema, Tables}`,
  `SearchableTable` (its name, key, row estimate, and columns with their `SearchKind`), and the
  results (`TableSearched::{Matched, NoMatch, NoColumns, TimedOut, Failed, Skipped}`). The value's
  reading (`SearchValue`: text, and whether it is an integer, a decimal, a UUID, a date) is a
  pure function. One `Connection` method, defaulting to `Unsupported` as `describe_schema`
  does: `searchable_tables(&SearchScope)`, the scope's tables and their columns in one or two
  catalog queries, not one `describe_table` per table, since a schema can have thousands.
- **`dbrover-sql`:** `Dialect::search_kind`, the `LIKE` pattern with the dialect's escape, the
  case and collation rules, and `search_statement(dialect, &SearchableTable, &DataSearch)`, which
  writes a table's statement, or none when no column can hold the value. Pure and golden
  tested per dialect, as the DDL and DML builders are. `Capabilities` gains `data_search`:
  whether the explorer offers it, and whether the engine bills each read.
- **Each driver** implements `searchable_tables` with its own catalog queries, typed as
  `describe_schema` reads them.
- **`dbrover-engine`:** `Command::StartDataSearch`, `CancelDataSearch`, answered by
  `Event::DataSearchProgress` (a table started, a table done with its rows) and
  `Event::DataSearchEnded`, on a session task of the search's own; the read-only mode, the time
  limit, the order, the careful data source's and the billed engine's questions (the estimate
  is asked before the first statement runs and answered by `Command::ConfirmDataSearch`), and
  the history entry with `HistorySource::Search`.
- **`dbrover-app`:** `Tab::DataSearch(DataSourceId)`, not restored; the toolbar, the scope picker
  (the explorer's catalog with tick boxes), the results drawn as the history tab draws its
  table, the highlighting (the same match done on the cell's text, so a collation's accent
  folding that the app can't repeat still shows the row, unhighlighted), and **Open row** and
  **Open all** through the consoles W5b's links open. **Search data…** in the explorer's menus,
  and the keymap action `console.search-data`, unbound by default, which opens the tab on the
  console's data source.
- **`dbrover-store`:** `HistorySource::Search`, origin `search`; `[data_search]` in
  `settings.toml`.
- **`dbrover-mcp`:** `search_data`, run through the engine's search, as `server_activity` runs
  through W15's.

## Options weighed

### One statement per table, or one per column

A statement per column (`SELECT key, col FROM t WHERE col ILIKE …`) would say exactly which
column matched, and let each column use its own index. But a table with twenty text columns
would be read twenty times, and each read is the whole table when no index helps, which is
the usual case for **Contains**. One statement per table, with an `OR` of its columns, reads it
once; which column matched is found in the rows it returns.

### `LIKE`, or the grid's `contains_text`

The grid's in-database filter (`arrange`) casts every column to text and looks for the value
with a position function, which needs no escaping. It is right for filtering a result: every
column, whatever its type. For a search across tables it reads every column as text, finds
`1042` in `21042`, and is answered by no index. `LIKE` on text columns, `=` on the others, costs
the escaping and is answered by the indexes engines have for it.

### Its own connection, or the explorer's

As W15's tab: the explorer's requests run in order, and a search that takes a minute would hold
up expanding a node. One connection while the search runs, closed after.

### A tab, or a dialog

DBeaver's search is a dialog whose results open in a panel. A tab keeps the results beside the
consoles the matches open in, survives switching to them and back, and follows D8: what stays
open is a tab.

### Every engine's own full-text search, or `LIKE` everywhere

See [Native full-text search](#native-full-text-search-later): different matches on different
columns depending on indexes the user can't see. `LIKE` first, the same everywhere; **Words**
later, as a choice the user makes.

## Layers

1. **W16a — The plan:** this design, and the roadmap's entry.
2. **W16b — The search's shape:** the core model, the value's reading, `search_kind`, the `LIKE`
   patterns and `search_statement` for every SQL dialect, warehouses included, golden tested:
   `%`, `_`, `!`, `\`, `[` and `'` in the value; a number that fits one column and not
   another; a UUID with braces; a table with nothing to search; each case and match kind.
3. **W16c — PostgreSQL, SQLite and DuckDB:** `searchable_tables` on each, and the conformance
   check `check_data_search`: a schema seeded with a customer `Kate O'Brien`, a note `50%_off
   [today]`, an order with id `1042` and a phone `+44 20 1042 7731`, a UUID key, a `jsonb`
   payload, mixed case and accents, and a table with only numbers; each search finds exactly
   the rows it should, and nothing it shouldn't (`50%_off` doesn't find `50 offers`).
4. **W16d — The engine and the history:** the commands and events, the search's session, the
   read-only mode, the order, the time limit, the cap, Cancel, the careful question,
   `HistorySource::Search` and its badge and filter in History. Engine tests on the PostgreSQL
   test server: a table locked `ACCESS EXCLUSIVE` by another connection times out and the
   search goes on; Cancel while blocked on it ends the search and keeps what was found; a
   read-only data source is searched; a careful one asks; one history entry per search.
5. **W16e — The tab:** **Search data…** in the explorer's menus, the toolbar, the scope picker,
   the results, the highlighting, **Open row**, **Open all**, **Retry**, and **Settings › Data
   search**. Kittest tests on SQLite and the PostgreSQL test server: results grouped by table,
   a row opened in a console on its key, a keyless table's rows opened by their values, a
   timed-out table listed with Retry, the careful question naming the data source.
6. **W16f — MySQL, MariaDB, SQL Server and ClickHouse:** `searchable_tables` on each, through
   `check_data_search`, with a case-sensitive collation's column, an `N'…'` literal for an
   `nvarchar`, a `[` in the value on SQL Server, and ClickHouse's `LowCardinality(String)`.
7. **W16g — Trino and the engines that bill each read:** `searchable_tables` on Trino, Athena,
   BigQuery, Snowflake and Databricks; the question before the first statement with BigQuery's
   dry-run estimate; the schema-or-table scope; Redshift turned off. Trino and BigQuery on the
   nightly containers, the others on recorded exchanges.
8. **W16h — MCP:** `search_data` behind `read`, the project's schemas, row limit and time limit,
   the refusal on billed engines, and its tests in `dbrover-mcp`.
9. **W16i — Manual and demo note:** a chapter, *Searching data*, in `book/src`, with pictures from
   new scenes (`data-search`, `data-search-scope`) on the PostgreSQL test server; the explorer's
   menus in [Data sources](../book/src/data-sources.md); History's new origin; the MCP tool;
   the exit as an engine test, `w16_exit`.

Each layer compiles and passes on its own. The tests use the containers `docker-compose.yml`
already starts, in the driver, engine and app test binaries CI already runs, so no job and no
service is added to per-pull-request CI; Trino and BigQuery stay in the nightly workflow. The
time-limit and Cancel tests block on a lock another connection holds, and wait for the server
to report the wait (polled every 50 ms, as W15's are) rather than sleeping.

**Exit:** on the PostgreSQL test server, searching a schema for `o'brien` lists `customers` and
`notes` with the matching rows and columns, and nothing else; searching `50%_off` finds only the
literal text; `1042` finds the order by its id and the customer by their phone; a UUID finds its
row by its `uuid` key; double-clicking a match opens a console on that row, editable; a table
locked by another session times out and the search goes on; Cancel ends a search and keeps what
it found; on a careful data source the search asks first, naming it; History lists the search
once, as **Search**.

## Decisions

The nine open questions below were answered on 2026-11-26, all as proposed; the rest are
confirmed as their layers are built.

| Question | Proposed | Consequence |
| --- | --- | --- |
| The engines | **PostgreSQL, SQLite, DuckDB, MySQL, MariaDB, SQL Server, ClickHouse**, then Trino and the billed engines with a question; Elasticsearch and MongoDB later; Redis, Cassandra and DynamoDB not at all | The servers CI already starts first; nothing that bills searches without asking. |
| Where results show | **A tab per data source**, not restored | Results stay beside the consoles their matches open in. |
| What is searched | **Text in text columns; numbers, UUIDs and dates as whole values in their own columns; JSON as text**; binary, arrays, booleans never | No `1042` inside `21042`; indexes on keys and numbers are used. |
| Default match | **Contains, ignoring case** | What a person typing a fragment expects. |
| Matching | **`LIKE` with the dialect's escape, `!` where there is a choice** | Indexes that answer `LIKE` are used; no server setting changes what the escape means. |
| Limits | **15 s per table, 100 rows per table, one table at a time, smallest first** | A search ends, never runs two statements at once, and shows most matches early. |
| Read-only | **Every search runs in the engine's read-only mode** | A search can't write, even through a function. |
| Careful data sources | **Ask before more than one table** | A whole-database search of production is never one click. |
| Billed engines | **Always ask, with an estimate where there is one; schema or table scope only** | A click never scans a project. |
| History | **One entry per search, origin Search**, with the matching tables' statements | What was searched, and how to find it again, without one row per table. |
| MCP | **`search_data` behind `read`**, refused on billed engines | Nothing `query` couldn't already read; nothing billed without a person. |
| Full-text indexes | **Later**, as a **Words** match | Results don't depend on indexes the user can't see. |
| Statements | **Literals, not bound values** | What is shown, copied and kept is what ran. |

## Risks

- **Load on a production server.** A search reads whole tables when no index helps. One
  statement at a time, a time limit per table, the key and searched columns only (never a
  table's blobs), and the careful question cover it; the manual says a whole-database search is
  a scan of the database.
- **Cost on a warehouse.** A `LIMIT` doesn't lower what BigQuery and Athena bill. The question
  before the first statement, the estimate, the small default scope, and the MCP refusal cover
  it; Athena's missing estimate is said in the question.
- **Matches the user didn't expect, or missed.** A collation that folds accents finds `café` for
  `cafe` on MySQL; SQLite folds only ASCII; `PAD SPACE` ignores trailing spaces; a float stored
  as `0.1000000001` isn't `0.1`. Each is what the database itself would say for the same
  `WHERE`, the statement is shown, and the manual lists them.
- **Escaping mistakes.** A wildcard left unescaped finds too much; an escape the server reads
  differently finds nothing. The two steps are separate functions, golden tested per dialect,
  and `check_data_search` runs the hard values against each real server.
- **A value that is a secret.** Searching for a token or password keeps it in the history; the
  history's redaction (W4b) covers the shapes it knows, and the value never leaves the machine.
- **Large catalogs.** A schema with thousands of tables: the columns are read in one or two
  catalog queries, not per table, and the results list folds the tables with no match.
- **Flaky time-limit tests.** Tests block on a lock and wait for the server to show the wait,
  with a lock timeout on the waiter so a broken test fails in seconds, run in a loop before each
  layer lands.

## What stays out

- **Replacing values** where they are found ("find and replace" across tables): a search only
  reads.
- **Regular expressions** and wildcards typed by the user. Engines' regular expressions differ
  (POSIX, RE2, ICU, none in SQL Server before 2025), and none uses an index.
- **Searching several data sources at once.**
- **A partition filter** for BigQuery's and Athena's partitioned tables.
- **Counting every match** in a table; **Open all** pages through them.
- **Saved searches.** The history keeps each one, and **Run again** repeats it.

## Open questions

Answered on 2026-11-26: all nine as proposed. Kept as they were asked.

1. **Engines.** PostgreSQL, SQLite and DuckDB, then MySQL, MariaDB, SQL Server and ClickHouse,
   then Trino, BigQuery, Athena, Snowflake and Databricks with a question first, all in W16;
   Redshift with C7; Elasticsearch, OpenSearch and MongoDB in a following milestone; Redis,
   Cassandra and DynamoDB left out. Should the billed engines wait for a later milestone too,
   which drops W16g?
2. **A tab or a dialog** for the results: a tab per data source, not restored, replaced by the
   next search?
3. **JSON and XML columns** searched as text by default (slower, but where values often hide),
   or only when ticked?
4. **The limits:** 15 s per table, 100 rows per table, one table at a time, smallest tables
   first, all set in **Settings › Data search**?
5. **Careful data sources:** ask before a search of more than one table, or never ask, since a
   search only reads?
6. **Billed engines:** always ask, even with the data source's cost guard off, and offer the
   search only on a schema or a table there, not the whole data source?
7. **History:** one entry per search with origin **Search**, holding the matching tables'
   statements, or not recorded at all, as Server activity's reads aren't?
8. **MCP:** `search_data` in W16, behind `read` rather than a new permission, and refused on the
   billed engines?
9. **Full-text indexes:** leave the **Words** match (PostgreSQL's `tsvector`, MySQL's
   `FULLTEXT`, SQLite's FTS5, SQL Server's full-text, BigQuery's `SEARCH`) for a following
   milestone, or add it to W16 as a layer before the manual?
