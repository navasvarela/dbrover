# Charts as code

Status: **proposed on 2026-11-04; CH1 and CH2 built on 2026-11-06; CH3 started on
2026-11-06.** The [roadmap](roadmap.md#charts-as-code-track) tracks it as CH1 to CH4. CI fixes,
the reports track and W13 schema compare came first. Each milestone's decisions are confirmed
when it starts, as in [Reports](reporting.md): CH1's [below](#ch1-confirmed), CH2's and CH3's
after their layers.

W12 gave every result a **Chart** tab: bars, lines or a scatter of the rows the grid holds,
picked in the toolbar and exported as SVG or PNG ([Charts](../book/src/consoles.md#charts)).
A chart picked there lasts as long as the console's next run with the same columns. It can't be
written down, kept with the query, reviewed in a pull request, or drawn by a report that runs
every month from cron.

This plan gives a chart a short text form, a few TOML keys, that lives in two places:

- **Above a statement, in a `.sql` file or console,** as a comment block. Running the statement
  opens its result on that chart, and the toolbar writes its changes back to the block.
- **In a report,** as a `[[chart]]` table. Each run writes the chart as an SVG or PNG file
  beside its CSV, Excel or Parquet files.

The same keys go in both places, so a chart worked out in a console is pasted into a report
unchanged. The plan adds two things the text form makes worth having, a `series` column and
stacked bars, and a `chart` tool for MCP clients.

## The question

A chart picked by hand answers a question once. Three things need it written down:

- **Saved queries.** A query kept in a file (a console, a W6 file, a repository's `queries/`
  folder from [VC2](version-control.md#vc2--a-folder-of-queries)) should open as the chart its
  author meant, on any machine, after any edit to the `WHERE`.
- **Reports.** A month-end report that writes an Excel file is often read through one picture:
  net sales by category, valuations over time. Today someone opens the CSV and draws it again.
- **Reviewing.** A chart in text can be diffed and reviewed like the query under it.

W12 already has everything a chart needs except a name for it. `dbrover-chart` turns a `Choice`
(the kind, X and Y) and rows into marks, the app paints the marks, and the export writes the same
marks as SVG. This plan adds a text form for the choice, and a way to lay out and write marks
without a window.

## What a chart spec holds

Every key is optional. A missing key takes the value the **Chart** tab would start with
([W12's starting choice](well-rounded.md#picking-the-columns)), given the keys that are present:
an `x` of dates with no `kind` draws lines, as the tab would.

| Key | Takes | Default | Built in |
| --- | --- | --- | --- |
| `kind` | `"bars"`, `"lines"` or `"scatter"` | lines over times, a scatter over numbers, bars over text | CH1 |
| `x` | a column's name | the first column of dates or text | CH1 |
| `y` | a column's name, or a list of them | every other column of numbers, up to eight | CH1 |
| `series` | a column's name: one `y`, split into a series per value of this column | none | CH2 |
| `stack` | `true` to stack the series' bars | `false` | CH2 |
| `title` | text above the chart | none | CH1 |
| `x_title`, `y_title` | text under the chart and beside it | X's name; Y's name, or the legend | CH1 |
| `y_zero` | `true` to start the Y axis at zero for lines and scatters (bars always do) | `false` | CH1 |
| `sort` | a column's name, then `asc` or `desc`: `"net desc"` | the result's order | CH1 |
| `limit` | the number of rows to chart, after `sort` | every row, up to the kind's limit | CH1 |

A report's `[[chart]]` adds the keys that make a file:

| Key | Takes | Default |
| --- | --- | --- |
| `file` | a file name ending in `.svg` or `.png`, with parameters as outputs format them (`{since:%Y-%m}`) | required |
| `width`, `height` | the chart's size in points | 800 by 450 |
| `scale` | a PNG's scale, `1`, `2` or `3`, as **Export ›** offers | `2` |
| `query` | a DuckDB query over the run's sources, drawn instead of the result (see [Which rows](#which-rows-a-reports-chart-draws)) | the result |

The names follow what people already use: `x`, `y` and `series` are the names Evidence's chart
components and most plotting libraries give these roles, and `kind`, `title` and `sort` read as
they would in a sentence. Nothing is a DbRover idiom except `y_zero`, whose name says what it
does.

**What the spec doesn't hold:** colours, fonts, the theme, legends' placement, ticks. The
chart's look is W12's, the same on screen and in a file, and a spec that can't set it can't get
it wrong. Each could become a key later, if someone needs it.

### Sort and limit belong in SQL, mostly

In a console, `ORDER BY` and `LIMIT` in the query are the better way to sort and cut a chart,
and the manual says so. `sort` and `limit` exist for reports, where every output shares one
result: a CSV of every product in SKU order and a chart of the ten best-selling ones need
different orders from the same rows. They read the same in a console so that a spec copies
between the two unchanged.

`sort` takes one column, compared by its role as the grid compares it: numbers by value, dates
by instant, text as the grid sorts it. In a console it applies to the rows the grid holds,
through its filter; the line under the chart says "sorted by net, the first 10".

## Where a spec lives

### Above a statement

In a `.sql` file or console, the comment lines directly above a statement, starting with
`[chart]`, are its chart. The rest of each line after the comment marker is TOML:

```sql
-- Net sales by month, a bar per category stacked.
-- [chart]
-- kind = "bars"
-- x = "month"
-- y = "net"
-- series = "category"
-- stack = true
-- title = "Net sales by month"
SELECT strftime('%Y-%m', l.ordered_on) AS month, p.category, SUM(l.amount) AS net
FROM order_lines l JOIN products p ON p.id = l.product_id
GROUP BY month, p.category
ORDER BY month, p.category;
```

- **The block** runs from the `[chart]` line to the last comment line before the statement.
  Prose comments go above it, as the first line here does. A blank line, or a line that isn't a
  comment, between the block and the statement ends it, so the block belongs to no statement
  and the editor marks it.
- **The comment marker** is the console's language's own line comment: `--` for SQL and
  PartiQL, `//` for MongoDB's shell syntax. `dbrover-language` already knows each one to split
  statements. A language whose console has no line comment has no block; its charts are picked
  in the tab, as today.
- **One chart a statement.** A statement that returns several results charts the first one with
  rows.
- **Running it** opens that result on **Chart** rather than **Grid**, drawn as the block says.
  Running it again keeps a choice changed in the toolbar while the block's text is unchanged,
  as W12 keeps a choice from run to run; once the block is edited, the block wins.
- **Other tools** see an ordinary comment. A formatter that keeps comments with the statement
  below them keeps the block; the manual says to check one that reflows comments.

### In a report

A report file takes any number of `[[chart]]` tables, beside its `[[output]]`s:

```toml
[[chart]]
file = "net-by-category-{since:%Y-%m}.svg"
title = "Net sales by category"
query = """
SELECT category, COALESCE(SUM(amount), 0) AS net
FROM lines
GROUP BY category
ORDER BY net DESC
"""
x = "category"
y = "net"
```

This one, added to `examples/reports/sales-by-category.toml`, writes a bar chart of each
category's net sales next to the Excel and CSV files, each month the report runs. Without
`query`, a chart draws the report's result, as an output writes it.

A `[[chart]]` is a table of its own, not an `[[output]]` with `format = "svg"`. An output's keys
(`columns`, `preset`, `sheet_name`, `separator`) mean nothing to a chart and a chart's mean
nothing to an output; one table with both sets would accept any mix, and its problems would
read worse. Run by run, a chart behaves as an output does: written in the staging folder,
renamed into place with the others, listed with the files written.

### The same keys in both

A block's lines without their comment markers are a `[[chart]]` without its `file`. The tab's
**Copy for a report** puts the report's form on the clipboard, with a `file` named after the
chart (`net by category.svg`, as **Export ›** names it today), ready to paste.

## Options weighed

| Option | Simplicity | Round trip with the tab | Error messages | What users know | Verdict |
| --- | --- | --- | --- | --- | --- |
| **TOML keys, in a comment block and in `[[chart]]`** (above) | Ten keys, all optional; an empty `[chart]` is a valid chart | The tab rewrites three or four keys in place with `toml_edit`, keeping the others and their comments | TOML's own, with a line and column, plus DbRover's for keys and columns | TOML is every other DbRover file; the keys are the usual plotting names | **Chosen.** |
| **One line of DbRover's own** (`-- chart: bars month by net, returns`) | Shortest to type | A writer for a small grammar | A grammar to parse and explain, and names with spaces or commas to quote | Nobody knows it yet | **No.** A second syntax for reports, and a language to maintain for a few saved keystrokes. |
| **A subset of Vega-Lite** (`{"mark": "bar", "encoding": {"x": {"field": "month", "type": "ordinal"}, …}}`) | A dozen lines of JSON for a bar chart, each field's type stated by hand | Writable, but JSON in comment lines, and a key order to keep | A subset refuses much of what people copy from Vega-Lite's examples, each refusal a "DbRover doesn't support this" | Known to people who use Altair or Observable; unfamiliar to most of a database client's users | **No.** A larger model than W12 draws, with no Rust renderer short of embedding a JavaScript runtime (vl-convert's Deno). **Copy as Vega-Lite** can come later to take a chart elsewhere. |
| **A file beside the query** (`sales.sql` with `sales.chart.toml`) | Pure TOML | Two files to keep together; a console of several statements needs names for them | TOML's | TOML | **No.** A query and its chart drift apart when one file is renamed or copied. |
| **Kept by the app, not in the text** | Nothing to type | Trivial | None | None | **No.** Not in Git, not on another machine, not in a report: today's state, made permanent. |
| **A native Excel chart in an Excel output** | rust_xlsxwriter can write one, referring to the sheet's cells | Another mapping of the same keys | Excel's | Accountants know Excel's charts | **Later, if asked.** A picture file serves every output format, CSV and Parquet included; an Excel chart serves one. |

## What problems say

A spec is checked twice: its keys when it is read, its columns when the result's columns are
known. Each problem is a sentence that names the chart and the line, as a report's problems do.

- **TOML that doesn't read:** the parser's message, at its line in the console or the file.
- **A key that doesn't exist:** "chart, line 3: no key `ys`; did you mean `y`?" A misspelt key
  is refused rather than ignored, as reports refuse theirs.
- **A value of the wrong kind:** "chart, line 2: `kind` is bars, lines or scatter, not `area`."
  "`stack` stacks bars; this chart draws lines."
- **A column that isn't there:** "chart: `y = "revnue"` isn't a column of this result: month,
  category, net." A name that differs only in case suggests it: "did you mean `Revenue`?"
  Names are exact, since databases differ on case.
- **A column named twice in the result:** "chart: two columns are named `count`; name them
  apart with `AS`."
- **A column of the wrong role:** "chart: `y = "month"` holds dates, and Y needs numbers."
- **`sort`:** "chart: `sort = "net up"`: write a column, then `asc` or `desc`."

In a console, a key's or the TOML's problem is underlined in the editor, as a statement's
problems are, and the chart tab shows it in place of the chart with **Grid** one click away. A
column's problem shows over the chart the tab would start with, so the result is still seen.

In a report, key problems are the file's problems, listed in the tab and refusing the run, and
`dbrover report run` exits with 3 before a process starts. Column problems are known once the
result's columns arrive, before its first row is written: the run fails in the chart's step, and
the command exits with 1, as an output that can't be written does.

## The round trip with the tab

- **The spec opens the tab:** running a statement with a block shows **Chart**, drawn as the
  block says; a key it leaves out takes the starting choice's value.
- **The tab writes the spec:** a change in the toolbar (the kind, X, Y, and from CH2 the series
  and **Stacked**) shows "Not in the query" with **Save to query**, which writes the block above
  the statement: the keys the toolbar sets, written explicitly so that a later change of columns
  can't change the chart silently, and every other key, comment and blank as it was
  (`toml_edit`, already in the build for [VC1c](version-control.md#keeping-comments-and-order-vc1c)).
  A statement without a block gets one. The write is an edit of the console's text, undone with
  the editor's undo, and saved as the console saves.
- **Nothing is written without asking.** A click in the toolbar is often a look at the data
  from another side; only **Save to query** changes the text.
- **What the tab can't set,** such as `title` or `sort`, is typed in the block. The tab shows
  their effect.
- **A chart with no block** keeps working as W12 built it: no text is written until **Save to
  query**.

## Reports

### Which rows a report's chart draws

**By default, the result:** the combine's rows, or the one source's when there is no
`[combine]`, the rows every `[[output]]` writes. The chart reads them a page at a time beside the
outputs and keeps what it draws; with `sort` and `limit` it keeps the top rows as they pass,
never more than `limit`.

**With `query`,** a DuckDB query over the sources' tables, as the combine sees them, run after
the combine in the same locked-down workspace, under the combine's limits. The sales-by-category
example needs it: its result mixes each product's rows with a subtotal per category and a total,
which is right for the Excel file and wrong for a chart. `query` needs a DuckDB workspace, so a
report with one source and no `[combine]` refuses it, and says to add one. The combine's result
isn't a table the chart's query can read, since the outputs stream it; a chart of the combine's
rows reshaped repeats the logic or, across files, uses [RS9](reports-at-scale.md#composing-reports).

### How it fails, and its guardrails

A report's chart is unattended: nobody reads the line under it. So where the tab shows the first
rows and says so, a report fails rather than write a picture of part of the data.

- **More rows than the kind draws** (250 bars, 50,000 points, W12's limits) fails the run in the
  chart's step, with the limit and the fix: "chart 1 draws bars, at most 250 rows; the result has
  1,204. Group the rows, or set `limit`." `dbrover report run` exits with 5, as for Excel's row
  limit. With `limit`, the chart draws the first rows after `sort`, and its file says so in a
  line under it.
- **More series than colours** (from CH2, more than eight values of `series`) fails the same
  way, naming the values past the eighth.
- **A null or unreadable value** is a gap, as on screen, never a zero; the run's warnings count
  them per series, and the file's line under the chart does too.
- **Nothing to draw** (an empty result) writes the chart with "No rows" in place of marks, so the
  month whose report found nothing still has its file; the warning says so.
- **Size:** `width` and `height` at most 4,000 points, and a PNG's pixels within
  `dbrover_picture::MAX_PIXELS`, checked when the file is read.
- **Time and memory** are the outputs': a chart holds at most its kind's limit of rows, laid out
  once; a chart's `query` runs under the combine's `timeout` and `memory_limit`.
- **Files** go through the staging folder with the others: a run that fails or is stopped
  writes no chart.

The run's metrics record each chart as an output: its time, the rows drawn and the file's size.
RS1's profile shows it as a step of its own.

### The Scope line

[Reports' scope](reporting.md#scope) keeps out "dashboards, charts that refresh, and a BI
semantic layer", and says "W12's charts stay one result's picture in a console". A report's
chart moves that line by one step, and only one:

- **What it is:** one result's picture, written once per run beside the rows it pictures, as
  static as the CSV. It never refreshes; the next run writes a new file.
- **What stays out:** a page of several charts (a dashboard), refreshing on a timer or a server,
  interactive or HTML charts, a model of measures and dimensions shared by charts, and charts in
  the report's tab drawn from live queries.

The wording, applied to [Reports' scope](reporting.md#scope) when CH3 started: "Dashboards, charts that refresh, and a BI
semantic layer. A chart stays one result's picture: in a console, or as a file a report writes
beside its others."

### With composed reports (RS9)

[RS9](reports-at-scale.md#composing-reports) lets a report use another's result as a source.
Charts follow outputs there: an inner report's charts, like its outputs, aren't written, since
only its result is its contract. That is also how a chart of a heavy report's rows is reused: a
second, small report takes the first as a source, groups in its combine, and draws its chart,
with `reuse` (RS5) keeping the heavy run to once a day.

## Drawing without a window

Today the chart's text is measured by egui, the SVG is written in the app (`console/chart/
svg.rs`), the light palette lives in the app's theme, and the bundled fonts are the app's. A
report's worker and the MCP server have no window. CH3's first layer moves what they need out of
the app, without changing what the tab shows:

- **The SVG writer and the export palette** move into `dbrover-chart`, which then writes a
  drawing's SVG itself. The SVG helpers shared with ER1's diagrams (`hex`, `Escaped`, the font
  stacks, `BASELINE_DROP`) and the bundled font files move from the app into `dbrover-picture`,
  which `dbrover-chart` then depends on.
- **Measuring text without egui:** `dbrover-picture` measures a string's width with the bundled
  Inter's advance widths, read with `ttf-parser`, which the build already compiles for resvg
  (0.25 in `Cargo.lock`), so no new crate is built.
- **The tab** keeps measuring with egui and laying out at its own size; **Export ›** keeps
  writing what the tab shows. A report and the MCP tool lay out at their own size with the font
  files' measure. The two measures differ by a fraction of a point on some labels; a test bounds
  it.
- **Deterministic files:** with the fonts' measure, the same rows give the same SVG byte for
  byte, so a report's chart is tested against a checked-in file, as its CSV is.

## Which chart kinds to add

Judged by how often a database result wants the kind, against what it costs in `dbrover-chart`'s
layout and on screen.

| Kind | Value | Cost | Verdict |
| --- | --- | --- | --- |
| **`series`** (not a kind, but needed first) | High. `GROUP BY month, category` is the usual shape of a result, and today it needs a pivot in SQL before it charts | Small: reading the rows splits one Y by the series column's values, in first-seen order | **Yes, CH2.** |
| **Stacked bars** (`stack = true`) | High once `series` exists: parts of a total by month | Small: bars offset by the series below them, positives up and negatives down from zero; the Y range from the stacks; hover shows the total | **Yes, CH2.** |
| **Horizontal bars** | Moderate: long category names, a top-20 of customers | Moderate: the layout's axes swapped, labels measured on the left | **Later,** if asked. |
| **Area** (stacked) | Moderate: shares over time, close to what stacked bars show | Moderate: a filled shape per series, drawn in egui as a strip of four-sided pieces, since egui fills only convex shapes; a polygon in SVG | **Later,** after stacked bars, if asked. |
| **Histogram** | High for a distribution (latencies, order sizes) | Small to draw, but binning the rows the grid holds bins a first page, not the table | **No kind.** Binning belongs in the query (`width_bucket`, `floor(x / 10) * 10`), and the result charts as bars; the manual shows it. |
| **Heatmap** | Moderate: day of week by hour | High: two category axes, a colour scale and its legend, labels on both axes | **No,** for now. |
| **Pie** | Low: hard to compare slices | Arcs, labels around them | **No.** Bars say it better. |

## Command line and MCP

### `dbrover report run`

Charts are written as outputs are, with nothing new to type:

- **Standard output** lists each chart's path with the other files', so a script picks them up
  the same way.
- **`--json`** reports each chart as an `output_written` event, the type the app and CI already
  read, with its rows and size.
- **Exit codes** as [RP6's](reporting.md#exit-codes): 3 for a chart's key problems, 1 for a
  column it can't find or a file it can't write, 5 for its row or series limit.

A `dbrover chart` command that draws a `.sql` file's block is left out: a report of one source
and one chart does the same, with parameters, guardrails and history.

### A `chart` tool for MCP clients

An assistant that has queried a database often wants to show the answer. A `chart` tool
returns a picture of a query's result:

```json
{
  "data_source": "shop",
  "sql": "SELECT category, SUM(amount) AS net FROM order_lines l JOIN products p ON p.id = l.product_id GROUP BY category",
  "chart": { "kind": "bars", "x": "category", "y": "net", "title": "Net sales by category" },
  "format": "png"
}
```

- **The spec** is the same keys as JSON, read by the same types, with the same problems as the
  tool's errors.
- **The query** runs as the `query` tool's does: read-only, the cost guard, the data source's
  MCP login and its **row limit**. A chart reads no more rows than `query` would return, since a
  picture shows them too; the reply says when rows were cut.
- **The reply:** PNG at 2× as MCP image content by default, which clients show; `"format":
  "svg"` returns the SVG as text. Beside it, what was charted: rows, gaps, rows left out.
- **Annotations:** read-only and closed-world, as `query`.
- **Running a report** over MCP stays RS8's question; this tool charts one query.

## Milestones

One GitHub stack per milestone, each layer one pull request, useful and tested on its own, with
synthetic data only (the SQLite shop of `examples/reports/shop.sql`).

### CH1 — A chart in a query's text (M)

1. **CH1a — Plan:** CH1's decisions confirmed, and the roadmap's layers.
2. **CH1b — The spec:** in `dbrover-chart`, the spec's types, reading it from TOML with each
   problem as a sentence, resolving it against a result's columns and roles into a `Choice`
   (missing keys from the starting choice), `sort` and `limit`, `title`, `x_title`, `y_title`
   and `y_zero` in the layout, and writing a spec back. Pure, with table tests of every problem.
3. **CH1c — Finding the block:** in `dbrover-language`, the block above each statement by the
   language's line comment, with its position for problems, and a block belonging to no
   statement marked.
4. **CH1d — The tab reads and writes it:** a statement with a block opens on **Chart**; problems
   underlined in the editor and shown in the tab; **Save to query** with `toml_edit`, undoable;
   **Copy for a report**.
5. **CH1e — Manual and demo note:** a section, *A chart in the query*, in the consoles chapter.

**Exit:** on the SQLite shop, a console whose statement has a `-- [chart]` block of bars by month
opens as that chart; switching to lines and **Save to query** rewrites only `kind`, keeping a
comment inside the block; undo restores it; `y = "nett"` shows the problem with the result's
columns.

#### CH1 confirmed

Started on 2026-11-06, after the reports track, W13 and C3. The decisions above stand as
proposed; checking them against the code settled these details:

- **The comment marker** is the language's own line comment, and every language has one: `--`
  in SQL and PartiQL, `//` in MongoDB's shell syntax, and `#` for Redis and Elasticsearch, which
  is what **Save to query** writes. A block is read after any line comment the language's lexer
  knows, so MySQL's `#` and CQL's `//` work too. `dbrover-language` lexes them already, but
  names none: CH1c adds `QueryLanguage::line_comment` and the block's finder.
- **A comment line** is a line comment alone on its line, as the lexer finds it, so
  `-- [chart]` inside a string or a `/* … */` comment is no block. A second `[chart]` line ends
  the block before it, which then belongs to no statement.
- **Lines in problems** are the console's: "chart, line 4: …" points at the line the editor
  shows, not the block's fourth.
- **`series` and `stack`** are refused as keys that don't exist until CH2 draws them, rather
  than read and ignored.
- **More than eight Ys** is a column problem, one colour per series, as the toolbar starts with
  at most eight.
- **`sort`** puts a row whose value is null or doesn't read as its column's role last, whichever
  the direction, so the first ten rows of a chart are rows with something to draw.
- **`y_title`** stands where Y's name does, above the plot, and before the legend when there are
  several series: W12's layout writes no turned text, and a title beside the axis would need it.
- **Save to query** on a statement that shares its first line with another (`SELECT 1; SELECT
  2`) writes nothing and says to put it on a line of its own, since a block above the line would
  belong to the first. A statement that moved since it ran is found again by its text.
- **`toml_edit`** came into the build with RP8f's Quiet this hint, not VC1c; CH1b's spec is read
  with it too, so a problem points at the bytes of the key or value it is about.

### CH2 — Series and stacked bars (S–M)

1. **CH2a — `series`:** reading one Y split by a column's values, first-seen order, the eighth
   colour's limit, gaps per series.
2. **CH2b — Stacked bars:** `stack`, its layout and hover, and the SVG of it.
3. **CH2c — The toolbar and the manual:** **Series** and **Stacked** in the tab, written by
   **Save to query**; the manual's section and demo note.

**Exit:** `GROUP BY month, category` on the shop draws as stacked bars per category with no pivot
in the query, on screen and exported.

#### CH2 confirmed

Started on 2026-11-06, after CH1. The plan stands; building it settled these details:

- **Rows of a split chart** become a point per X value, in the order the X values first come
  (sorted by value for lines over numbers and dates, as without a series). A second row with the
  same X and series value is left out and counted under the chart, rather than added: a sum
  belongs in the query's `GROUP BY`.
- **Past eight values** of the series column, the tab charts the first eight and names the rest
  under the chart; a report's chart (CH3) fails instead, as planned. A null series value is a
  series named `NULL`, as the grid shows it.
- **Y with `series`** is one column; left out, it is the first column of numbers that isn't X or
  the series. A column can't be the series and X or Y at once, and `series` with several Ys is a
  problem.
- **`stack`** stacks the series of two Ys or more as well as a split one, since both are series.
  It applies to bars only: a spec that stacks lines or a scatter is a problem, and the toolbar's
  **Stacked** is off for them.
- **Stacked pieces** are drawn square, on screen and in the SVG, so they meet without notches;
  bars standing alone keep their rounded corners.
- **Save to query** writes `series` and `stack = true` while the chart has them, and takes them
  out when it doesn't, so a block of CH1 saved again gains no `stack = false`.

### CH3 — Charts from reports (M)

1. **CH3a — Plan:** CH3's decisions, and the Scope line's new wording in
   [Reports](reporting.md#scope).
2. **CH3b — Drawing without a window:** the SVG writer and palette in `dbrover-chart`; the
   helpers, fonts and the font files' measure in `dbrover-picture`; the app's export through
   them, unchanged on screen.
3. **CH3c — `[[chart]]` in the report file:** read and planned in `dbrover-report`, with `file`
   and its parameters, `.svg` or `.png`, `width`, `height` and `scale`, and every problem naming
   the chart.
4. **CH3d — Running it:** the chart beside the outputs on the result's pages, its limits and
   failures, staged and renamed with the files, its warnings, metrics and `output_written`
   event; `dbrover report run` prints it and exits as above.
5. **CH3e — A chart's own `query`:** run after the combine in the locked workspace, under its
   limits.
6. **CH3f — Manual, examples and demo note:** a section of the **Reports** chapter; the
   sales-by-category example with its chart, and its October 2026 SVG checked in.

**Exit:** `dbrover report run sales-by-category` writes the Excel, CSV and SVG files, the SVG
equal byte for byte to the checked-in one; a chart of more than 250 bars without `limit` exits
with 5 and leaves no file; the report's tab lists the chart with the others.

#### CH3 confirmed

Started on 2026-11-06, after CH2. The plan stands; checking it against the report crate and the
app settled these details:

- **What moves out of the app (CH3b).** `dbrover-picture` takes the SVG helpers ER1's diagrams
  and the chart share (`SANS`, `MONO`, `BASELINE_DROP`, `Escaped`), a colour of its own (`Rgb`,
  since it has no egui), the bundled Inter and JetBrains Mono files, and a text measure: a
  string's width from Inter's advance widths, its line from the font's ascender, descender and
  line gap, read with `ttf-parser` (a direct dependency at the lockfile's 0.25.1, so nothing new
  is built). `dbrover-chart` takes the SVG writer, the light palette a file is drawn in, the
  eight series colours of both themes, and the sentence the tab writes under a chart, which a
  file writes too.
- **Same bytes everywhere.** A report's chart is laid out with the font files' measure at 11
  points, the tab's small text, and the SVG writes each coordinate rounded to a hundredth of a
  point, so the last bits of a platform's floating point never reach the file. The tab's
  **Export ›** writes through the same writer, so its files round the same way; nothing changes
  on screen. A test bounds how far the two measures place a label apart.
- **The keys (CH3c).** `[[chart]]` takes the spec's keys, read by `dbrover-chart`'s own reader
  from the report's TOML, and `file`, `width`, `height` and `scale`. `file` ends in `.svg` or
  `.png` and takes parameters as an output's does. `width` and `height` are whole points from
  200 to 4,000; `scale` is 1, 2 or 3 and only for a `.png`, where it is a problem with an
  `.svg`; a PNG past `dbrover_picture::MAX_PIXELS` at its scale is a problem when the file is
  read. A misspelt key is suggested from both sets ("no key `fiel`; did you mean `file`?").
  Problems say "chart 2: …" and are placed on their key in the report's editor. `query` is
  refused as an unknown key until CH3e reads it, as CH1 refused `series`.
- **One name a file.** No two files of a run, outputs' or charts', may have the same name, with
  the file's templates or with a run's values.
- **A chart's step (CH3d).** A chart is a step of its own, as an output is: `chart 1
  (net-by-category-2026-10.svg)` in messages, `"step": "chart"` in events. A column it can't
  find or a file it can't write exits with 1, its row or series limit with 5. Its file is an
  `output_written` event, so the report's tab lists it with the outputs' files, the history
  records it as an output after them (rows drawn, size, time), and `dbrover report run` prints
  its path. An inner report's charts under [RS9](reports-at-scale.md#composing-reports) aren't
  written, as its outputs aren't; RS9 isn't built yet, so nothing changes for it here.
- **Reading the rows.** A column's role comes from its declared type, or from the first page of
  rows when it has none, and the spec is resolved against the columns then; a problem with them
  fails the chart's step. Without `limit`, the chart keeps the rows up to its kind's limit and
  counts the rest; past it the run fails: "chart 1 draws bars, at most 250 rows; the result has
  1,204. Group the rows, or set `limit`." With `limit` and `sort` it keeps the top rows as the
  pages pass; with `limit` alone, the first ones. A `limit` above the kind's limit doesn't lift
  it.
- **More than eight series** fails in the chart's step at its limit, naming the values past the
  eighth. The advice under a chart's limits says to group the rows or set `limit`, never to raise
  a default in Settings, which has none for charts.
- **The line under a file's chart** is the tab's: the rows charted, rows left out and why, gaps
  per series, and the sort and limit. Its gaps and rows left out are also the file's warnings,
  which the tab and `dbrover report run` show.
- **No rows** writes the chart's title and "No rows to chart" in place of marks, with a warning.
  The spec's column names are checked, but not their roles, which need values.
- **When files are written.** Charts of the result read its pages beside the outputs; after its
  last row each chart is drawn and written into the staging folder, and only then are all the
  files put in place, outputs first, then charts in the file's order. A chart that fails fails
  the run, which leaves no file.
- **A chart's own `query` (CH3e)** runs after the combine query's rows are written, a chart at
  a time in the file's order, on the locked-down workspace, with a `timeout` of the combine's
  length counted from its own start, under the workspace's `memory_limit`. It is one DuckDB
  statement that only reads, takes the report's parameters, and is a problem, exit 3, in a
  report without `[combine]`, which says to add one. The editor colours it as DuckDB's SQL.

### CH4 — A chart for MCP clients (S)

1. **CH4a — The `chart` tool:** its arguments, the query run as `query` runs it, PNG and SVG
   replies, the audit log's entry.
2. **CH4b — Manual and demo note:** the MCP chapter's tool table.

**Exit:** over stdio, a `chart` call on the shop returns a PNG an MCP client shows; a data source
whose MCP access is off refuses it; a result past the row limit is drawn cut, and the reply says
so.

### Order

CH1 and CH2 are the app's and can come any time after W13. CH3 needs CH1b's spec, and is best
after RS9 is designed into place, though it doesn't depend on it. CH4 needs CH3b and nothing
from reports.

## Decisions

Proposed defaults; each is confirmed when its milestone starts.

| Question | Proposed | Consequence |
| --- | --- | --- |
| The syntax | **TOML keys, DbRover's own and few,** not a subset of Vega-Lite | No new dependency and one parser for both places; a chart can't ask for more than W12 draws. Vega-Lite's users get **Copy as Vega-Lite** later, if asked. |
| Where it lives in a query | **A comment block above the statement,** in the language's line comment | The chart travels with the query in its file, through Git and copy-paste; other tools ignore it. |
| Where it lives in a report | **`[[chart]]`,** a table of its own | Its keys and its problems stay apart from outputs'; it runs as an output does. |
| Missing keys | **The tab's starting choice** | An empty `[chart]` is a chart; a block names only what it means. |
| Writing back | **Only on Save to query,** explicit keys, others kept with `toml_edit` | A look at the data from another side changes no text; a saved chart can't change with the columns. |
| Column names | **Exact,** a differing case suggested | Databases differ on case; a guess would chart the wrong column silently. |
| `sort` and `limit` | **In the spec, for reports;** SQL recommended in consoles | One result serves a CSV and a top-10 chart; the same keys read in both places. |
| What a report's chart draws | **The result, or its own DuckDB `query`** over the sources | The subtotal rows a report writes for Excel needn't be in its chart. |
| Rows past a kind's limit in a report | **Fail, unless `limit`** | An unattended file never shows part of the data without saying so; the tab keeps showing the first rows. |
| Chart kinds | **`series` and stacked bars;** horizontal bars and area later; no histogram, heatmap or pie | The usual `GROUP BY` shape charts without a pivot; binning stays in SQL, where it sees every row. |
| Measuring text without a window | **The bundled fonts' advances, through `ttf-parser`** | Deterministic files and golden tests; labels a fraction of a point from the tab's. |
| The Scope line | **Moved one step:** a report may write a chart's picture beside its files | Still no dashboards, refresh, HTML or semantic layer. |
| `dbrover report run` | **Charts as outputs:** paths, `output_written`, RP6's exit codes | Nothing new for cron or CI to learn. |
| MCP | **A `chart` tool over one query,** PNG by default, the row limit applied | An assistant shows an answer; a chart reveals no more rows than `query` would. |

## Risks

- **Drifting towards dashboards.** Each step looks small: a second chart, a page of them, a
  refresh. The Scope line's wording is the guard, and this plan's [What stays out](#what-stays-out)
  repeats it.
- **A chart that misleads.** A report's chart of the first 250 rows, or with gaps drawn as
  zeros. Reports fail past a limit, `limit` is written in the file and in the picture, and gaps
  stay gaps.
- **Blocks moved by other tools.** A formatter or an editor's "join comments" may separate a
  block from its statement. The block then belongs to nothing, and the editor marks it rather
  than charting the wrong statement.
- **Two measures of text.** The tab's egui and the files' font tables may place a label a point
  apart. Files never come from the tab's measure, so each path is consistent with itself, and a
  test bounds the gap.
- **A spec that outgrows ten keys.** Requests for colours, log scales and annotations will come.
  Each new key is weighed as the kinds are, and the [Options weighed](#options-weighed) table is
  where a larger format would be argued again.
- **Moving code W12 and ER1 share.** CH3b moves SVG helpers and fonts out of the app; ER1's
  diagram export uses them too. The move is behaviour-preserving, with ER1's and W12's export
  tests run unchanged.

## What stays out

- Dashboards: a page, file or tab of several charts.
- Charts that refresh, on a timer, a server or a live connection.
- Interactive or HTML charts in files, and JavaScript in DbRover.
- A semantic layer: measures, dimensions or metrics defined once for many charts.
- Vega-Lite, Plotly or another charting runtime in the build.
- Colours, fonts and themes per chart; dark files.
- Charts in PDF; [Reports](reporting.md#scope) keeps pixel-perfect PDF layouts out.
- A `dbrover chart` command, until a report of one chart proves too heavy for the need.
