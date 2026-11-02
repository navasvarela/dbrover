# Exports and your own formats

Status: **W7 built** (2026-10-30); **W8 built** (2026-11-01). This is the design of W7 and W8 in
[A well-rounded app](well-rounded.md#w7--exports); the [roadmap](roadmap.md#well-rounded-track)
tracks them.

Getting rows out of the database is half of what a database tool is for: into a spreadsheet for
someone who asked, into a pull request as a Markdown table, into a test fixture as `INSERT`s, into
a data pipeline as Parquet. DbRover writes CSV and JSON, with no options, from the grid only. W7
adds the formats people ask for, their options, and every place an export should start. W8 lets
the user define formats of their own in plain files, as H1 and H2 did for actions.

## What exists today

- **The grid's Export menu** has **Loaded rows as CSV…** and **as JSON…** (the rows as sorted and
  filtered in the grid), and **All rows as CSV…** and **as JSON…**, which run the statement again
  on the console's connection and stream every page to the file (W5d). An export can be
  cancelled, is recorded in the history, and is written beside its final name and renamed when
  complete, so a failed one never leaves half a file.
- **CSV has no options.** Commas, a header line, CRLF, quotes only where needed, `NULL` as an
  empty field, UTF-8 without a BOM: RFC 4180, which Excel opens with mangled accents.
- **JSON** is an array of objects with every value a string (ADR 0004), keys in column order, a
  repeated column name suffixed `_2`.
- **Copying:** ⌘C and **Copy selection** copy the selected cells as TSV, quoted the way
  spreadsheets read them back, with `NULL` written out. **Copy row** joins a row with tabs but
  quotes nothing, so a value with a tab or a line break pastes into the wrong cells. **There is
  no way to copy the column headers**, an M4 follow-up still open.
- **Elsewhere:** CSV import (comma, semicolon or tab), the history's CSV and JSON export (W4f),
  the data sources' TOML export (W1), and H2's command actions, which get the selected cells on
  stdin as JSON, CSV or TSV.
- **Missing:** exporting a table from the explorer, running a query straight to a file, copying
  in any other format, and any of it through MCP. DbRover has no command palette; menus and the
  keymap are where commands live.
- **Types:** results are text (ADR 0004). SQLite, MySQL and MariaDB, SQL Server, DuckDB and
  ClickHouse report each column's declared type name; PostgreSQL's simple query protocol reports
  none.

## What others do

From each tool's current documentation (October 2026):

- **DataGrip** has *data extractors*: CSV, TSV and pipe-separated with configurable separators,
  quoting, `NULL` text and header; SQL `INSERT` and `UPDATE` statements; and bundled scripts for
  Excel, HTML, JSON, Markdown, XML and more. Users write their own extractors in Groovy or
  JavaScript, getting the rows, the columns and a value formatter. An extractor is chosen once
  and then used by both **Export to File** and **Copy to Clipboard**. **Execute to File** runs a
  query straight to a file without loading the grid, and the Database Explorer exports whole
  tables. ([Data extractors](https://www.jetbrains.com/help/datagrip/data-extractors.html),
  [Export data](https://www.jetbrains.com/help/datagrip/export-data.html))
- **DBeaver's Data Transfer** wizard writes CSV, HTML, JSON, Markdown, SQL, TXT and XML, plus XLSX
  and Parquet in its paid editions. CSV has a delimiter, row delimiter, header position, quoting
  rules and `NULL` text; the output step has the encoding, **Insert BOM** (off by default), a
  file name pattern (`${table}_${timestamp}`), splitting by size and compression. **Export from
  query** reads from the database rather than the grid, and **Advanced copy** copies as any of
  those formats. ([Data export](https://dbeaver.com/docs/dbeaver/Data-export/))
- **TablePlus** exports CSV, JSON, SQL and Excel, and **Copy Rows As** offers CSV, CSV with
  header, JSON, HTML, Markdown and `INSERT`.
  ([Import and export](https://tableplus.com/docs/gui-tools/import-and-export))
- **Excel** opens a UTF-8 CSV correctly only if it starts with a byte order mark
  ([Microsoft](https://support.microsoft.com/en-us/office/opening-csv-utf-8-files-correctly-in-excel-8a935af5-3416-4edd-ba7e-3dfd2bc4a032)),
  and splits fields on the system's list separator, which is a semicolon in many European
  locales. The `sep=;` first line some tools write is undocumented by Microsoft and is read as
  data by most other programs. Google Sheets detects the separator; Numbers asks.

## Goals

- **The formats people ask for, with the options that matter:** CSV that opens correctly in
  Excel, a real `.xlsx`, Markdown for pull requests and chat, SQL to move rows between databases,
  JSON Lines and Parquet for pipelines, HTML for a page or an email.
- **One choice of format everywhere:** the same formats and options for a file, the clipboard,
  a whole table, a query run straight to a file and MCP's answers.
- **Any size:** every file export streams; no format holds all the rows.
- **Formats of your own, in plain files** (W8): named presets, templates and programs in
  `exports.toml`, beside `actions.toml`, read and reloaded the same way.

Not goals: reports with grouping, totals, charts, page layout or schedules. DbRover isn't a BI
tool. Grouping and totals are SQL's job (`GROUP BY`, `ROLLUP`, window functions), parameters are
the `:name` parameters consoles already ask for, and a program in W8 can turn rows into anything
else, a PDF included.

## What stays the same

- **Results are text** (ADR 0004). Formats that have types (`.xlsx`, Parquet, SQL literals) take
  them from the column's declared type where the driver reports one, and otherwise from the text
  itself, by rules written down below and tested. Values are never reformatted: a date is written
  as the database printed it, or as that same date in a typed format. Formatting dates and
  numbers differently is done in SQL (`to_char`, `FORMAT`, `strftime`).
- **Files are written beside their final name and renamed when complete**, cancelled from the
  toolbar, and recorded in the history as **Export**, as W5d's are.
- **Safety levels:** an export reads, so it runs everywhere, in the database's read-only mode on a
  read-only data source, as W5d's does. Exports write nothing to the database.

## Formats

In priority order. Each has the options listed and nothing more; every option has a default, so
**Export** with nothing changed writes a sensible file.

| Format | Extension | Options | File | Clipboard | MCP |
| --- | --- | --- | --- | --- | --- |
| CSV | `.csv` | separator, quoting, header, `NULL` text, line ending, BOM | ✓ | ✓ | ✓ |
| TSV | `.tsv` | header, `NULL` text | ✓ | ✓ (⌘C) | — |
| Excel | `.xlsx` | header, sheet name, freeze header, autofilter | ✓ | — | — |
| Markdown | `.md` | `NULL` text | ✓ | ✓ | ✓ |
| SQL `INSERT` | `.sql` | table, rows per statement | ✓ | ✓ | — |
| JSON | `.json` | (none) | ✓ | ✓ | its own, as today |
| JSON Lines | `.jsonl` | (none) | ✓ | ✓ | ✓ |
| HTML | `.html` | fragment or whole page, `NULL` text | ✓ | ✓ (as text) | — |
| SQL `UPDATE` | `.sql` | (table and key from the result) | ✓ | ✓ | — |
| Parquet | `.parquet` | compression | ✓ | — | — |

- **CSV:**
  - **Separator:** comma (default), semicolon, tab, pipe, or any single character.
  - **Quoting:** where needed (default), or every field.
  - **Header line:** on (default) or off.
  - **`NULL` as:** an empty field (default), `NULL`, `\N`, or any text.
  - **Line ending:** CRLF (default, as RFC 4180 asks) or LF.
  - **Byte order mark:** off (default; most programs other than Excel read it as part of the
    first column's name), or on.
  - **Encoding:** always UTF-8. A BOM is what Excel needs; other encodings are left out until
    someone asks.
  - **CSV for Excel** is a preset in the format list: comma, CRLF, a BOM. Its description says
    that Excel in a European locale may want a semicolon instead. No `sep=` line.
- **TSV** is what ⌘C copies, as a file: tabs, the header on, `NULL` written as `NULL`, fields
  quoted the way spreadsheets read them back.
- **Excel (`.xlsx`)** is written with
  [`rust_xlsxwriter`](https://crates.io/crates/rust_xlsxwriter) in its constant-memory mode, a
  row at a time. A bold header row, frozen and with an autofilter (both on by default), and column
  widths guessed from the first page of rows. Each cell is typed:
  - A column whose declared type is a number, a boolean, a date or a timestamp gets numbers,
    booleans or dates; one declared as text, or of a type DbRover doesn't map, stays text.
  - A column with no declared type (PostgreSQL) is typed cell by cell: a plain decimal number
    becomes a number, unless it has a leading zero (`007`, a code rather than a quantity) or more
    than 15 significant digits (beyond what Excel's numbers hold exactly, as with long ids); an
    ISO date or timestamp becomes a date shown in ISO format; everything else is text.
  - A timestamp with a time zone or offset stays text: Excel has no time zones, and converting it
    would change what it says.
  - Excel holds 1,048,576 rows a sheet. Past that, rows go on in a new sheet (`Rows 2`), and the
    status says so.
- **Markdown** is a GitHub-flavoured table: `|` escaped, line breaks as `<br>`, `NULL` shown as
  `NULL` (as in the grid) unless changed.
- **SQL `INSERT`** statements for a table: the one the result came from when it came from one
  (the grid already knows), otherwise a name typed in the dialog, quoted for the data source's
  dialect. One row per statement by default, or up to 1,000 rows (SQL Server's limit for one
  `VALUES`). Built by `dbrover_sql::dml`'s `RowSql`, as the grid's edits and the CSV import are.
  Values are string literals, as the grid's edits send them, except in columns whose declared
  type is numeric, where a number is written bare.
- **JSON** is today's: an array of objects, values as strings. **JSON Lines** is one object a
  line, the same objects, and the format a pipeline or `jq -c` reads best. For a result of
  documents (N0b, [ADR 0006](adr/0006-documents-as-extended-json-text.md)), both write the
  documents whole, as they came, rather than their flattened columns (`ExportSpec::JsonDocuments`
  and `JsonLinesDocuments`, chosen by `RowSource::shape`); the other formats write the
  flattened columns, and the SQL formats aren't offered, nor for rows of a language without a
  SQL dialect (`RowSource::dialect` is `None`).
- **HTML** is a `<table>` with a `<thead>`, every value escaped, as a fragment to paste into an
  email or a page, or a whole UTF-8 page with a little CSS that prints well. Copying it puts the
  markup on the clipboard as text: egui's clipboard holds only text (see Later).
- **SQL `UPDATE`** statements, one a row, setting every column but the key and naming the row by
  its primary key, as the grid's edits do. Offered only for the rows the grid could edit: a
  `SELECT` from one table whose result holds its whole primary key. Elsewhere it is greyed out,
  saying why.
- **Parquet** is written by DuckDB, which DbRover already contains: rows go into a table of
  text columns in an in-memory DuckDB, which can spill to a temporary folder rather than hold
  everything in memory. When the last row is in, each column is given the narrowest type every
  one of its values converts to (`BIGINT`, `DECIMAL`, `DOUBLE`, `BOOLEAN`, `DATE`, `TIMESTAMP`,
  `TIMESTAMPTZ`, else `VARCHAR`), with a declared type taking precedence, and `COPY … TO` writes
  the file with zstd (default) or Snappy compression. No new crate.

## Where exports start

- **The grid's Export… button** opens the export dialog: which rows (**Loaded rows** as sorted
  and filtered, or **All rows**), the format, its options, and a preview of the file's first
  lines for text formats (for Excel and Parquet, each column's type instead). **Export…** then
  asks where to save it. The dialog starts from the format and options used last.
- **Copy as ›** in the cell's right-click menu and the Edit menu copies the selected cells (the
  selected columns of the selected rows, as ⌘C does) in any format the clipboard takes. **Copy
  with headers** (⇧⌘C) copies TSV with the header line. **Copy row** quotes values as **Copy
  selection** does. Each **Copy as** format has a keymap id, `grid.copy-as.<format>`, unbound
  unless the user binds it. Copying takes the loaded rows only; it never runs the query.
- **Export data…** on a table or view in the explorer opens the same dialog with **All rows**,
  and runs `SELECT * FROM` the table on the explorer's connection, streaming, as an import runs
  there.
- **Run to file…** (⌥⌘↵, and in the Code menu) runs the statement at the cursor straight into a
  file, never loading the grid. It asks for the format and file first, then for `:name`
  parameters as a run does, then goes through the usual confirmations. Progress and **Cancel**
  show in the results toolbar, and the result panel says *Exported 1,204,331 rows to …* where
  rows would be.
- **MCP:** the `query` tool takes an optional `format`: `json` (today's answer, the default),
  `csv`, `markdown` or `jsonl`, and answers with the rows as that text, within the project's row
  limit and with `truncated` as now. MCP clients never write files: an assistant that wants a file
  writes the text it got itself, with its own permissions.
- **The keymap and menus:** **Export…**, **Run to file…** and each **Copy as** format are app
  commands, in the menus and bindable like every other, through `run_command`.
- **Left as they are:** the history's export keeps CSV and JSON, and the data sources' export
  stays TOML.

## Design

### A crate for formats

A new crate, `dbrover-export`, holds the formats, their options and the streaming writer. It
depends on `dbrover-core` and `dbrover-sql` (for the SQL formats' quoting), and brings in
`rust_xlsxwriter` (W7) and `minijinja` (W8), so neither weighs on crates that don't write files.
`dbrover-store`'s history export uses it in place of today's `ExportWriter`.

### One trait per kind of output

- **`TextFormat`** writes a header, then rows, then a footer, to any `io::Write`:

  ```rust
  pub trait TextFormat: Send {
      fn header(&mut self, out: &mut dyn Write, columns: &[ExportColumn]) -> io::Result<()>;
      fn row(&mut self, out: &mut dyn Write, row: &Row) -> io::Result<()>;
      fn footer(&mut self, out: &mut dyn Write) -> io::Result<()>;
  }
  ```

  CSV, TSV, Markdown, JSON, JSON Lines, HTML and the SQL formats implement it, and so do W8's
  templates. The same value writes a file (through a `BufWriter<File>`), the clipboard (into a
  `Vec<u8>`) and MCP's answer, which is what keeps them identical. A format keeps whatever state
  it needs (JSON's comma, a multi-row `INSERT`'s batch) and flushes it in `footer`.
- **Binary formats** (`.xlsx`, Parquet) write only files, and own them: they get rows a page at a
  time and finish into the path, through `dbrover-export`'s `FileFormat` trait. Parquet's writer
  is in the DuckDB driver, which owns DuckDB, and the engine adapts it to `FileFormat`, so
  `dbrover-export` doesn't depend on DuckDB; the others are in `dbrover-export`.
- **`ExportWriter`** stays the one way to write a file: it takes an `ExportSpec` (a format and
  its options), keeps W5d's `.partial` file and rename, and counts rows. Both loaded-rows and
  all-rows exports go through it, as now.
- **`ExportColumn`** is a column's name, its declared type name, and a `ValueKind` (integer,
  decimal, float, boolean, date, timestamp, timestamp with time zone, text, or undeclared when
  the driver reports no type). The kind is a `dbrover-core` type; `dbrover_sql::Dialect::value_kind`
  maps each dialect's type names to it, with a table-driven test per driver, since what differs
  between dialects lives in `dbrover-sql`'s `dialect` module. `.xlsx`,
  Parquet and `INSERT` use the kind; the text rules for columns without one are pure functions
  with their own tests.
- **Formats are an enum,** `ExportFormat`, matched exhaustively in one place, `ExportSpec::text_format`,
  which builds the writer for each; everything else (the dialog's option panel, the extension, the
  file dialog's filter, which outputs a format allows) is a method on it, so adding a format is one
  compiler-checked change.

### Options and where they live

- **Options are types,** one struct per format (`CsvOptions`, `XlsxOptions`, …), each with
  `Default` and serde, so illegal combinations can't be written (no quoting option on JSON).
- **The dialog remembers** the last format and each format's last options in the app's saved
  state (`app.ron`), as it remembers the window layout and recent files. They are conveniences,
  not configuration.
- **Named options are W8's presets** in `exports.toml`, where the user writes them down to keep,
  share and version. W7 adds nothing to `settings.toml`.

### Engine commands

| Command | Event | Session |
| --- | --- | --- |
| `ExportRows { console, export }` (now an `ExportSpec`) | `ExportFinished` | none (blocking pool) |
| `ExportAllRows { console, export }` (now an `ExportSpec`; also used by Run to file) | `ExportProgress`, then `ExportFinished` | the console's |
| `ExportTable { data_source, table, export }` | `ExportProgress`, then `ExportFinished` | the explorer's |

Copying formats the rows in the app: they are already there, and few. MCP formats its rows in the
MCP crate with the same `TextFormat`s.

## W8 — Your own formats

The [hackability plan](hackability.md) puts export formats among the extension points, opened
by command actions in H2 and by plugins in H3. W8 gives them a file of their own, `exports.toml`,
next to `actions.toml` in the settings folder, read like it (H1a's reader: at start-up and on
**Reload**, each entry checked on its own, problems listed in the status bar). **View ›
Exports…** opens an **Exports** page in the Settings tab, as **View › Custom actions…** opens
its own: every entry with its kind and what it writes, the file's problems, **Reload** and
**Create exports.toml**, which writes a commented example. Confirmed on
2026-11-01, once W7 had shipped; what changed since the first draft is listed
[below](#what-changed-from-the-first-draft).

Every entry has an `id` (lowercase letters, digits, `-` and `_`, as an action's), a `name` and
an optional `description`, and is one of three kinds, told apart by the key that only that kind
has: `format` for a preset, `row` for a template, `command` for a program. An entry with two of
them, or none, is a problem.

```toml
# A preset: a built-in format with its options.
[[export]]
id = "excel-semicolon"
name = "CSV for Excel (semicolon)"
format = "csv"
separator = ";"
bom = true

# A template: a header, a template per row, and a footer.
[[export]]
id = "confluence"
name = "Confluence table"
extension = "txt"
header = "||{% for column in columns %} {{ column.name }} ||{% endfor %}\n"
row = "|{% for value in row %} {{ value }} |{% endfor %}\n"

# A program: the rows as JSON Lines on stdin, the file from its stdout.
[[export]]
id = "pdf-report"
name = "PDF report"
extension = "pdf"
command = ["/Users/me/bin/rows-to-pdf", "--title", "{table}"]
```

Each appears in the export dialog's format list, after the built-in formats, and in **Copy as ›**
when it writes text: a preset of a text format, every template, and a program marked
`text = true`. A **Copy as** entry's keymap id is `export.<id>`, unbound unless the user binds
it; the prefix keeps a user's `csv` apart from the built-in `grid.copy-as.csv`. The same formats
are offered wherever the dialog opens: the grid's **Export…**, **Export data…** on a table and
**Run to file…**.

### Presets

- `format` is a built-in format's id (`csv`, `xlsx`, `tsv`, `markdown`, `sql-insert`, `json`,
  `jsonl`, `parquet`, `html`, `sql-update`), and the format's options sit beside it under the
  names the dialog saves them by in `app.ron`: `separator`, `quoting`, `header`, `null_text`,
  `line_ending` and `bom` for CSV; `sheet_name`, `freeze_header` and `autofilter` for Excel;
  `rows_per_statement` for `INSERT`; and so on. An option the format doesn't have is a problem
  naming the ones it does, so a typo never silently writes the default.
- `extension` is the format's own unless given.
- Choosing a preset in the dialog shows its options as set, without editing them: the file is
  where they change, so what a team shares stays the one definition. **CSV for Excel** stays
  built in.
- `sql-insert` and `sql-update` presets still take their table from the rows, as the built-in
  formats do, and are greyed out where those are.

### Templates

- **Parts:** `row` (required), `header` and `footer` (both empty unless given), in
  [MiniJinja](https://docs.rs/minijinja)'s Jinja syntax. Each part is rendered on its own,
  `header` once, `row` once a row as it arrives, `footer` once, so a template streams like every
  other format and a footer sees how many rows there were, never the rows. `extension` is
  required. Long templates fit in TOML's multi-line strings (`'''…'''`).
- **What every part sees:**
  - `columns`: each column's `name`, its `type` as the database names it (or none) and its
    `kind` (`integer`, `decimal`, `float`, `boolean`, `date`, `timestamp`,
    `timestamp_with_zone`, `text`, `undeclared`), the same list every format gets;
  - `data_source` (its name), `table` (the table the rows came from, when there is one),
    `parameters` (the run's `:name` values, by name) and `exported_at` (an ISO timestamp).
- **What each part adds:** `row` gets `row` (the values in column order), `record` (the values by
  column name, a repeated name suffixed `_2` as JSON's keys are) and `index` (counting from 1);
  `footer` gets `count`.
- **Values are text or `none`** (ADR 0004). `none` is written as nothing, where MiniJinja would
  write `none`; `{{ value if value is not none else "NULL" }}` writes something else.
- **Filters** added for exports: `sql_literal` and `sql_name` (quoted for the data source's
  dialect; a problem on rows of a language without one), `csv` (a field quoted as RFC 4180 asks)
  and `json` (a JSON string, or `null`). MiniJinja's own filters and tests are there too.
- **Escaping:** `escape = "html"` HTML-escapes every value unless marked `safe`; the default,
  `"none"`, writes values as they are.
- **Mistakes show early:** each part is compiled when the file is read, so a syntax error is a
  problem in the status bar naming the entry, the part and the line. A variable that doesn't
  exist (`{{ recrod.id }}`) is an error rather than an empty string, shown in the dialog's
  preview before anything is written.
- **Limits:** templates have no file access (no loader, so `include`, `extends` and `import`
  find nothing outside the entry), and each render of a part has a fuel limit
  (`FUEL_PER_RENDER`, MiniJinja counting instructions) and an output limit
  (`MAX_BYTES_PER_RENDER`), so an endless loop or a runaway string stops the export with a
  problem naming the row, rather than hanging DbRover or filling the disk. Both are named
  constants, the same for every template, and recorded with their reasons in the code.
- **Behind a small interface:** `dbrover-export` defines what a template is given (the context
  above, as DbRover's own types) and a `TemplateEngine` trait that compiles the three parts and
  renders one part into any writer. MiniJinja is its one implementation, in one module; nothing
  else names MiniJinja's types.

### Programs

- **Run as H2's command actions are:** no shell, `{table}`, `{schema}`, `{database}`, `{host}`,
  `{port}`, `{user}` and `{file}` placeholders in the arguments, the context in `DBROVER_*`
  variables (with `DBROVER_EXPORT` the entry's id and `DBROVER_FORMAT=jsonl`), no secret in any
  argument or variable, a stop, and trust by the exact definition, asked for in full before the
  first run and recorded in `trusted_commands.toml` as `export = "<id>"` beside the actions and
  password commands. The engine checks the trust again before it starts anything.
- **Input:** the rows as JSON Lines, exactly as the JSON Lines format writes them (an object a
  line, keys in column order, values strings or `null`), streamed a page at a time.
- **Output:** stdout goes straight into the export's `.partial` file, with no cap, so a program
  can export any number of rows; stderr's first 64 KB are kept for the error. A non-zero exit
  removes the partial file and shows the exit status and stderr.
- **Timeout:** `timeout` (60 s by default, as an action's) is how long the program may go without
  taking more input, and how long it has to finish once its input has ended. A long export of a
  program that keeps reading never times out; **Cancel** stops it at any time.
- **Text:** `text = true` says the program writes text, which adds it to **Copy as ›**. Copying
  runs it on the selected cells with H2's limits (4 MB of output kept, the timeout as a whole)
  and puts what it wrote on the clipboard.
- **Tests start no processes in the app:** the engine runs programs through H2's
  `ProgramRunner`, which gains a method for exports, and the app's tests give it the scripted
  runner. The engine's tests run H2's `dbrover-test-program`, which gains a mode that copies its
  stdin to stdout.

### Where it stops

- No grouping, totals, sorting or computed columns beyond what Jinja's expressions do on one row,
  no reports made of several queries, no charts, no scheduled exports. Each is SQL's job, or a
  program's.
- MCP keeps the built-in formats: a template would be safe to offer there, but a program can't
  ask to be trusted, and nothing yet asks for either. See Later.
- A data source export carries no exports, as it carries no actions.

### What changed from the first draft

- **Template files in `exports/` are left for later.** Multi-line TOML strings hold long
  templates, and one file keeps the reader, its problems and **Reload** as they are for the other
  customisation files.
- **`none` is written as nothing**, and the first draft's `default('')` isn't needed: MiniJinja's
  `default` replaces only undefined values, never `none`.
- **Undefined variables are errors**, so a typo shows in the preview instead of writing blanks.
- **Every part sees the shared context**, the footer included, so a closing line can name the
  table; `index` counts from 1.
- **The program timeout is about progress**, not the whole run, so a ten-million-row export isn't
  cut off, and **`text = true`** decides **Copy as**, since an extension alone can't say whether
  a program writes text.
- **The list is a Settings page, not a window,** as custom actions' became in W14.
- **Keymap ids are `export.<id>`, and only for Copy as**: the dialog needs no binding of its
  own, since it opens on the format used last.

### With reporting in mind

A later exploration may look at reports: several data sources, a language over query results, an
export template that sets the output's columns and their names (mostly for CSV and Excel),
metrics such as duration and size, and guardrails on memory and CPU. W8 builds none of that, but
keeps what it would build on:

- **Columns are declared, not guessed:** every format, built in or not, gets the same list of
  `ExportColumn`s (name, declared type, kind) and rows of text. A template that sets the output's
  columns would be a step that maps a result's columns to that list before any format sees it,
  so presets, templates, programs, `.xlsx` and Parquet would all follow it unchanged. For that
  reason W8's presets are only a format and its options; naming or choosing columns belongs to
  that later step, not to each preset.
- **The template engine is replaceable:** templates are given DbRover's own context through
  `TemplateEngine`, so a different language over the same context would be a second
  implementation, not a change to every caller.
- **Everything streams:** a template part sees one row at a time and the footer only the count.
  Totals in a report would be values DbRover computes as rows pass and hands to the footer, which
  keeps that contract; W8 adds none.
- **Limits are explicit:** fuel and output size per render are named constants passed to the
  engine as one `TemplateLimits` value, which a report could later set for itself. Memory beyond
  one render is bounded by streaming; CPU by fuel and by **Cancel**.
- **Metrics:** `ExportWriter` already counts rows; bytes written and the duration would be two
  more fields in `ExportFinished`, left out of W8 as nothing shows them yet.

What I would design differently with that direction in mind, rather than as the first draft did:
the context as DbRover's types behind `TemplateEngine` instead of MiniJinja values built at each
call; limits as one value rather than constants read inside the template module; and no column
options in presets, so a later column mapping has one place to live.

## Layers

Each layer is one pull request in a stack, and is useful and tested on its own.

**W7 — Exports**

- **W7a — Plan:** this document and the roadmap entries.
- **W7b — Text formats** (built): the `dbrover-export` crate; `TextFormat`; CSV with its options, TSV,
  Markdown, JSON, JSON Lines and HTML; `ExportSpec` and `ExportWriter` moved from the store, which
  uses them for the history; `ValueKind` from declared types. Golden tests: each format's output
  for awkward values (quotes, separators, line breaks, `|`, `<`, `NULL`, empty strings, emoji) and
  the paged-equals-whole test W5d has, for every format. The grid offers the new formats from its
  existing menu.
- **W7c — SQL statements** (built): `INSERT` and `UPDATE` formats over `RowSql`, golden tests per dialect,
  and a round trip on SQLite: export as `INSERT`, run into an empty copy, compare.
- **W7d — Excel** (built): `.xlsx` through `rust_xlsxwriter`: cell typing, header, freeze, autofilter,
  widths, sheets past a million rows. Tests read the file back with
  [`calamine`](https://crates.io/crates/calamine) (a dev-dependency) and check each cell's type and
  value; one writes a little over a sheet's worth of rows, in the slow tests.
- **W7e — Parquet** (built): the DuckDB writer, its type inference, compression; tests read the file
  back through DuckDB and check its schema and values.
- **W7f — The export dialog** (built): format, options, preview, loaded or all rows, the options
  remembered; kittest tests on SQLite.
- **W7g — Copy as** (built): the submenu in the cell menu and the Edit menu, **Copy with headers**, **Copy
  row** quoting as **Copy selection** does, the keymap ids; kittest tests reading the clipboard.
- **W7h — The explorer and Run to file** (built): **Export data…** on tables and views, `ExportTable`,
  **Run to file…**; engine tests on every engine with a test server (more rows than a page, a
  cancel, a read-only data source), kittest tests on SQLite.
- **W7i — MCP** (built): the `query` tool's `format`, tested in the MCP crate's tests.
- **W7j — Manual** (built): a chapter on exporting and copying, the troubleshooting entry for Excel and
  CSV, [Testing](testing.md), and the demo note.

**W8 — Your own formats**

- **W8a — Plan:** this section confirmed, the roadmap entry in layers.
- **W8b — Reading `exports.toml`, and presets** (built): `CustomFileKind::Exports` and its
  checks in the store, preset options checked in `dbrover-export`; the **Exports** settings page
  with **Reload** and **Create exports.toml**, **View › Exports…**, the status bar's problems;
  presets in the export dialog (remembered by id in `app.ron`) and in **Copy as ›**, with
  `export.<id>` in the keymap. Store tests of every problem message; kittest tests of the page,
  the dialog and the menu on SQLite.
- **W8c — Templates** (built): `minijinja`, `TemplateEngine`, the context, the filters,
  escaping, the fuel and output limits. Golden tests of the plan's Confluence table and an XML
  template, tests of each problem message, of hostile values (quotes, `<`, `{{`, line breaks,
  emoji, `NULL`) and of an endless loop stopping; the paged-equals-whole test W7 has; kittest
  tests of a template in the dialog's preview and in **Copy as ›**.
- **W8d — Programs** (built): trust under `export = "<id>"`, JSON Lines to the program, its
  stdout to the partial file, the progress timeout, **Cancel**, `text = true` for **Copy as**.
  Engine tests with H2's `dbrover-test-program` (every row, a table's arguments, a failure, a
  program that stops reading, trust); kittest tests of the question and of copying with the
  scripted runner.
- **W8e — Manual** (built): the chapter on `exports.toml` with a preset, a template and a
  program, [Testing](testing.md), and the demo note.

## Decisions

Agreed on 2026-10-29.

| Question | Decision | Consequence |
| --- | --- | --- |
| Formats in W7 | **CSV with options, TSV, `.xlsx`, Markdown, SQL `INSERT` and `UPDATE`, JSON, JSON Lines, HTML and Parquet.** XML is left to W8's templates. | One milestone covers what DataGrip, DBeaver and TablePlus offer between them, Parquet included. |
| Typing `.xlsx` cells | **Declared types first, then the text:** numbers and ISO dates become numbers and dates, leading zeros, long digit strings and offsets stay text. | Sums and sorts work in Excel; ids and codes are never mangled. |
| BOM | **Off for CSV, on in the CSV for Excel preset.** | Files stay as RFC 4180 and other tools expect; Excel users pick one item. |
| Custom formats | **`exports.toml` with presets, header/row/footer templates (MiniJinja) and programs (JSON Lines on stdin).** | Simple formats need no script; anything else is a program, with H2's safeguards. |
| Templates stream | **A template per row, not one over every row.** | Any size works; a footer can't see the rows, only their count. |
| MCP | **Formatted text in the `query` tool's answer; no files.** | Nothing new to permit, and the safety model stays as it is. |
| Where options live | **The last used, in `app.ron`; named ones in `exports.toml` (W8).** | Nothing new in `settings.toml`; what is worth keeping is a file the user can share. |
| Reformatting dates and numbers | **No.** Values are written as the database printed them. | Exact; formatting belongs in SQL. |
| Parquet | **Through DuckDB, already in DbRover,** not the `parquet` and `arrow` crates. | No new dependency; DuckDB's writer is well tested. |
| Rich HTML on the clipboard | **Not in W7.** | Pasting a table into Google Sheets or Word takes TSV, which they read; rich paste comes later. |

Confirmed for W8 on 2026-11-01, from the first draft above, with the changes listed in
[What changed from the first draft](#what-changed-from-the-first-draft):

| Question | Decision | Consequence |
| --- | --- | --- |
| Telling kinds apart | **By the key only that kind has:** `format`, `row` or `command`. | No `kind` key to keep in step; two of them is a problem. |
| Editing a preset in the dialog | **No:** its options show as set. | `exports.toml` stays the one definition a team shares. |
| `NULL` in templates | **`none`, written as nothing.** | `{{ value }}` reads naturally; other text is one expression away. |
| Undefined variables | **An error**, shown in the preview. | Typos never write blanks into a file. |
| Template files in `exports/` | **Later.** | One file, one reader; multi-line strings hold long templates. |
| Program timeout | **Time without progress,** 60 s by default. | Exports of any length; a stuck program still stops. |
| Programs in Copy as | **Only with `text = true`.** | Nothing binary reaches the clipboard. |
| Keymap | **`export.<id>`, copying as the format.** | User ids never collide with built-in ones. |
| MCP | **Built-in formats only.** | A program can't ask to be trusted there. |

## Risks

- **Excel mangling values anyway.** Typing a column wrongly would turn ids into rounded numbers or
  codes into dates. The typing rules are pure functions with table-driven tests of the values
  known to go wrong (leading zeros, 16-digit ids, `1e5`, `NaN`, `-0`, dates before 1900, offsets),
  and the round trip through `calamine` checks every cell's type.
- **Big exports holding memory.** `.xlsx` uses constant-memory mode, which writes each row to a
  temporary file; Parquet's DuckDB is given a memory limit and a temporary folder in the cache
  directory. An engine test exports more rows than a page in every format.
- **New dependencies.** `rust_xlsxwriter` (MIT or Apache-2.0, actively maintained, no `unsafe`,
  needing only `zip`, already in DbRover's dependency tree) and, in W8, `minijinja` (Apache-2.0, no
  dependencies, `unsafe` inside its value model, by an author with a long record). Both are checked
  against the minimum Rust version and the licence list when added. Download size is measured
  before and after each.
- **SQL that doesn't run elsewhere.** `INSERT`s use the source's dialect and quoting; moving rows
  between different databases may need edits, and the manual says so.
- **Templates and programs from someone else.** `exports.toml` is the user's own file, like
  `actions.toml`; templates can't touch files or run programs, programs are trusted one by one,
  and a data source export carries no exports, as it carries no actions.
- **CI time.** The new tests are store and crate tests that need no server, except W7h's engine
  tests, which reuse W5's test sources and add seconds per server. The million-row sheet test runs
  only with the slow tests.

## Exit

**W7:** I export a PostgreSQL query's every row to `.xlsx` and open it in Excel: numbers sum, dates
sort, a zip code keeps its leading zero, and the header is frozen. **CSV for Excel** opens in Excel
with accents intact. I copy a selection as a Markdown table into a pull request, and as `INSERT`s
into a SQLite console that runs them. **Export data…** on a ten-million-row table writes Parquet
without DbRover's memory growing, and **Run to file…** writes a query's rows without the grid
loading them. An assistant asks MCP for rows as CSV.

**W8:** I add a semicolon CSV preset, a Confluence table template and a program that makes a
PDF to `exports.toml`, and each appears in the export dialog and where it applies in **Copy as ›**;
a template with an endless loop stops with a problem naming the row; a typo in a template shows
in the preview; the program asks once before it first runs, and writes every row of a table
larger than a page.

## Later

- Rich HTML on the clipboard (through `arboard`, which egui already uses) so a paste into Google
  Sheets, Docs or Word keeps the table.
- JSON with typed values (numbers and booleans) from declared types.
- Exporting several tables at once from the explorer, and a file name pattern for them.
- Encodings other than UTF-8, compressed output, splitting files by size.
- Exporting the history in every format.
- Importing `.xlsx`, JSON Lines and Parquet, the counterpart of W5e.
- Templates in files of their own, in an `exports/` folder, with the three parts as
  `{% block %}`s.
- Templates as MCP formats, once someone asks; programs stay out of MCP.
- The rows, bytes and duration of every export in `ExportFinished`, for a status line or a report.
