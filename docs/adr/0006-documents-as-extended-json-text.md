# 0006. Documents as Extended JSON text

- Status: Accepted
- Date: 2026-10-30

Extends [ADR 0004](0004-results-as-text.md), which stays accepted.

## Context

ADR 0004 made results text: a cell is `Option<String>`, and a statement's columns are known
before its first row. Every SQL engine fits that. The non-SQL engines planned in
[NoSQL databases](../nosql.md) don't:

- MongoDB returns documents. Each has its own fields, and a field can hold an object or an
  array, nested to any depth. Its values have types JSON lacks: object ids, dates, 32- and 64-bit
  integers, decimals, binary data, timestamps, regular expressions.
- Elasticsearch and OpenSearch return hits whose `_source` is a JSON document, and answer most
  other requests with JSON.
- Redis keeps JSON in strings and in its JSON module, and its replies nest.
- DynamoDB's items are documents too.

The SQL engines already return JSON in cells: PostgreSQL `json` and `jsonb`, MySQL `JSON`,
SQLite's JSON functions, DuckDB's and ClickHouse's `JSON` types. The cell viewer pretty-prints
them, but there is no way to browse one, nor to turn a document's fields into columns.

The questions are how a document travels from a driver to the grid, and what a document is
inside DbRover.

## Decision

- **A result has a shape: rows or documents.** Rows are ADR 0004's, unchanged. A cursor says
  which it gives (`ResultCursor::shape`, rows unless it says otherwise), and
  `Event::StatementStarted` carries it to the console.
- **A document travels as text,** one string of **canonical Extended JSON** per document:
  MongoDB's JSON form for every BSON type, which the `bson` crate writes and reads. A page of
  documents is a page of rows of one cell each, so paging (500 a page through `FetchMore`),
  cancelling, the history and the engine's events stay as they are. As with ADR 0004, every type
  shows correctly with no decoding code for each in DbRover.
- **The canonical form, not the relaxed one,** because it records each value's type: a 64-bit
  integer is `{"$numberLong": "7"}`, not `7`. A document edited in the grid or the tree (N2) can
  then go back with every type unchanged. Engines whose documents are plain JSON
  (Elasticsearch, a Redis string) send it as it is: plain JSON is Extended JSON with no special
  values in it.
- **Shown the way the shell shows it:** `ObjectId("…")`, `ISODate("…")`, `NumberLong(7)`,
  `NumberDecimal("1.10")`, `UUID("…")`, `Timestamp(1, 2)`, `/pattern/flags`. Plain JSON values
  from a SQL column are shown as JSON, with no Extended JSON reading: a `jsonb` holding
  `{"$oid": "…"}` is an object with one field.
- **DbRover reads the text itself,** in a crate with no engine in it, `dbrover-document`, which
  keeps each object's fields in the order written and each number as written (`1.10` stays
  `1.10`, and a 30-digit integer keeps every digit). The same reader serves documents, SQL
  engines' JSON columns and Redis's JSON values, so one tree view serves them all.
- **The console flattens documents into columns,** not the driver: the columns are the
  top-level fields of the documents fetched so far, `_id` first and the rest in the order they
  first appear, and a field first seen on a later page adds a column at the right. A field a
  document lacks is told apart from one that is `null`. An object or array shows compactly
  (`{3 fields}`, `[12 items]`), and can be expanded into columns of its own
  (`address.city`). Past 200 columns, the rest stay in the tree.
- **DynamoDB's sets are DbRover's own types** (added with N5, on 2026-11-01): BSON has no set,
  so a string, number or binary set is `{"$stringSet": […]}`, `{"$numberSet": […]}` or
  `{"$binarySet": […]}` (binary values in base64), shown as PartiQL writes them,
  `<<'a', 'b'>>`. An edited set goes back a set rather than a list. DynamoDB's other types
  are JSON's, and its binary values `$binary`.

## Consequences

- No driver's results change, and neither does anything about rows: sorting, filtering,
  selecting and copying the flattened columns work as they do for rows.
- A tree view of the whole document, for documents and for any JSON value in a cell.
- Documents are read twice: once as each page arrives, to find its columns, and again when
  a tree draws one. Reading JSON text is cheap next to fetching it, and a page is at most 500
  documents.
- Sorting and filtering compare the text shown, as for rows (ADR 0004): `NumberLong(10)` sorts
  by its text, not its value. Typed sorting can come later, as for rows.
- A flattened cell is still `Option<String>`, which has no room for "missing". The console
  keeps which fields each document lacks beside the rows, to draw them greyed; the formats that
  write flattened columns (CSV, Excel, …) write a missing field as they write `null`. JSON and
  JSON Lines write the documents as they came, so nothing is lost there.
- A cursor of documents must give rows of exactly one cell. The type system doesn't hold that;
  the console reads a row of any other width as a document it can't read, and says so in the
  cell, rather than failing.
- The SQL export formats and grid edits need a table and a dialect, which documents don't have:
  they aren't offered for documents. Editing documents comes with MongoDB (N2), through the
  canonical form above.

## Alternatives considered

- **A typed document in `dbrover-core`** (the `bson` crate's `Document`, or `serde_json`'s
  `Value`): every crate between the driver and the grid would learn one engine's types, and the
  engine's events, the history and the exports would each need a second path beside rows.
- **`serde_json::Value` as the reader:** the workspace turns on its `preserve_order`, so it
  keeps fields in order, but it reads numbers into `f64`, `i64` or `u64`, which changes `1.10`
  to `1.1` and rounds integers past 2⁶⁴, and it keeps only the last of two fields with the same
  name. Its `arbitrary_precision` feature keeps numbers as written, but would change how every
  other crate in the workspace reads and writes JSON numbers.
- **Relaxed Extended JSON:** shorter and closer to plain JSON, but `7` could be a 32- or 64-bit
  integer or a double, so an edited document would come back with its types changed.
- **The driver flattens documents into rows:** every driver would repeat it, and the grid would
  lose the whole document, which the tree and the JSON exports need.
