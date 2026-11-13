# 0004. Query results as text

- Status: Accepted; extended by [0006](0006-documents-as-extended-json-text.md) for results
  that are documents, and amended by [0007](0007-arrow-on-bulk-paths.md) for paths that move
  many rows
- Date: 2026-10-22

## Context

A database tool shows values from every type the database has, including extension and
user-defined types it has never heard of. The architecture doc first sketched a typed
`Value` enum (integers, decimals, timestamps, …, with an `Other` fallback), decoded from
PostgreSQL's binary protocol by `tokio-postgres`'s `query_raw`.

Decoding binary values needs a Rust type for each PostgreSQL type. Anything without one (ranges,
`inet`, `interval`, enums, composite types, extension types) would come through as raw bytes,
and DbRover would have to reimplement the server's formatting for each to show it.

## Decision

Results are text:

- PostgreSQL statements run over the **simple query protocol**
  (`Client::simple_query_raw`). The server formats every value as text, exactly as `psql`
  shows it, and streams rows as they are read.
- SQLite values are formatted the way the `sqlite3` shell does.
- A cell is `Option<String>` (`None` is SQL `NULL`). Columns have a name and, when the driver
  knows it cheaply, a declared type name.

## Consequences

- Every type displays correctly with no decoding code, including ones that don't exist yet.
- The simple protocol takes one statement at a time here (the console splits scripts), which is
  also how errors and result sets map back to the editor.
- No column types from PostgreSQL's simple protocol (`RowDescription` there carries only names
  in tokio-postgres's API). The grid right-aligns cells that look numeric instead.
- No query parameters (`$1`) for console statements. Not needed for user-typed SQL.
- Sorting, filtering or editing by type will need typed values. When editing data arrives
  (M5), those features can use the extended protocol for the rows they edit, without changing
  how results are displayed.
  - *M5 update*: editing didn't need it. Edited values go back as string literals in
    generated `UPDATE`s, which PostgreSQL converts to the column's type and SQLite stores by
    affinity, so text in, text out holds. Client-side sorting compares numbers by value when
    both cells look numeric.

## Alternatives considered

- **Typed values via the extended protocol** (the original sketch): more exact sorting and
  alignment, but every unknown type falls back to bytes, and DbRover would own a formatter per
  type.
- **Extended protocol with text result format**: possible with lower-level tokio-postgres APIs,
  but it gives nothing the simple protocol doesn't for a console, and needs more code.
