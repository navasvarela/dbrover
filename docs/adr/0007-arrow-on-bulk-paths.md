# 0007. Arrow batches on bulk paths

- Status: Accepted; amends [0004](0004-results-as-text.md) for paths that move many rows
- Date: 2026-11-13

## Context

ADR 0004 made every result text: each value as its engine formats it, so every type shows
correctly with no decoding code. That suits a console and a grid, which show values, and it
is how a report's sources reach the combine step's DuckDB today: a page of text values, which
DuckDB casts into the table's types as it appends them (RS3c).

For a report, the text is a detour. Measured in RS3c on two million rows of four columns, text
into typed columns took 1.00 s and the same rows as typed Arrow arrays 0.72 s; RS1g's
benchmark found loading the run's largest cost. Some engines answer in Apache Arrow already,
or in a columnar form that reads into it: DuckDB natively, ClickHouse as `ArrowStream`, and
the services C5 and C6 add (Snowflake's result chunks, Databricks's `ARROW_STREAM`).
Formatting their values as text only for DuckDB to parse them back costs time and, for
decimals and times, invites a round trip that has to be exact.

`arrow` 58 is already in the build through the `duckdb` crate, and DuckDB's appender takes an
Arrow batch (the `appender-arrow` feature, on since RS3c).

## Decision

**Paths that move many rows into DuckDB may carry Arrow batches, where the engine gives them
natively.** Values shown on screen stay text, as ADR 0004 says.

- `dbrover-core` adds a capability beside `ResultCursor::next_page`: a cursor whose engine
  answers in Arrow hands out a `BatchCursor` (`ResultCursor::batches`), whose `next_batch`
  returns a `RecordBatch`. Cursors of engines that answer in text hand out none, which is the
  default, so no driver changes unless it has batches to give.
- An engine that must be told before a statement starts that its rows are wanted as Arrow
  (ClickHouse's `FORMAT ArrowStream`, Databricks's `ARROW_STREAM`) starts it so from
  `Connection::execute_for_batches`. The default runs the statement as `execute_bound` does.
- A batch's columns are of a small set of **bulk types**: booleans, integers, floats, decimals
  of up to 38 digits, dates, timestamps with or without a zone, and text. A column of any
  other type (lists, structs, intervals, UUIDs, huge integers) arrives as text, as the engine's
  driver formats it for `next_page`, so the rule of ADR 0004 still holds for every type
  DbRover has no exact equivalent for.
- A text column in a batch is typed by its values, as a page of text is; every other column
  keeps the type the engine gave it.
- The first user is a report's source loaded into the combine step's DuckDB. Exports may
  follow; the console, the grid, documents and every other path stay text.

## Consequences

- Decimals stay exact as Arrow's `Decimal128`, integers as integers, and no value of a bulk
  type is formatted and parsed again on its way to DuckDB.
- Two ways to read a cursor: a caller reads one through `batches()` or through `next_page`,
  never both. A cursor started for batches still answers `next_page`, from its batches.
- Each driver with batches owns the line between its bulk columns and its text ones, and turns
  the rest into the same text its `next_page` gives.
- `arrow-array` and `arrow-schema` become dependencies of `dbrover-core`, at the version the
  `duckdb` crate builds, so a batch passes from a driver to DuckDB's appender without
  conversion; the two are bumped together. `arrow-ipc` joins for ClickHouse's stream.
- A text column's formatting can differ between the two paths for types outside the bulk set
  where the engine's columnar answer is formatted by another hand: ClickHouse's `toString` of
  an array quotes strings with `'` where its JSON uses `"`.

## Alternatives considered

- **Typed values for every result** (ADR 0004's rejected sketch): the console and grid gain
  nothing, and every type without an Arrow equivalent would need a formatter of DbRover's.
- **Parse text into typed Arrow arrays in Rust** (RS3c's fastest measurement): the same win for
  engines without a columnar answer, but it moves the engine's formatting rules into DbRover;
  left out, as RS3c left it.
- **A `next_batch` every cursor answers, building text batches from pages by default:** keeps
  one method, but a caller couldn't tell an engine's typed batch from text it must type by
  values without inspecting every column, and the default would only move RS3c's text batch
  from the workspace into every driver.
- **Arrow Flight SQL or ADBC:** see [Reports at scale](../reports-at-scale.md#other-technologies);
  the engines DbRover reaches don't speak Flight, and ADBC drivers are native libraries.
