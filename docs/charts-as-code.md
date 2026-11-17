# Charts as code

Status: **CH1 to CH4 built; CH5 planned, not built.** Proposed on 2026-11-04; CH1, CH2 and CH3
built on 2026-11-06, and CH4 on 2026-11-07: CH1 in #515 to #519, CH2 in #522 to #524, CH3 in
#526 to #531, CH4 in #533 and #534. CH5, [a fuller chart](#a-fuller-chart), was planned on
2026-11-16 after the manual's thin charts coverage turned out to reflect a narrow feature set
as much as a gap in the writing; it takes up what the first four milestones
[left for later](#left-for-later) and adds statistics, and it comes after RS7f and RS7g and
before RS8. The [roadmap](roadmap.md#charts-as-code-track) tracks it as CH1 to CH5. CI fixes,
the reports track and W13 schema compare came first. Each milestone's decisions are confirmed
when it starts, as in [Reports](reporting.md): CH1's [below](#ch1-confirmed), CH2's, CH3's and
CH4's after their layers.

W12 gave every result a **Chart** tab: bars, lines or a scatter of the rows the grid holds,
picked in the toolbar and exported as SVG or PNG
([The Chart tab](../book/src/charts.md#the-chart-tab)).
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
| `kind` | `"bars"`, `"hbars"`, `"lines"`, `"scatter"` or `"area"` | lines over times, a scatter over numbers, bars over text | CH1, CH5 |
| `x` | a column's name | the first column of dates or text | CH1 |
| `y` | a column's name, or a list of them | every other column of numbers, up to eight | CH1 |
| `series` | a column's name: one `y`, split into a series per value of this column | none | CH2 |
| `stack` | `true` to stack the series' bars | `false` | CH2 |
| `title` | text above the chart | none | CH1 |
| `x_title`, `y_title` | text under the chart and beside it | X's name; Y's name, or the legend | CH1 |
| `y_zero` | `true` to start the Y axis at zero for lines and scatters (bars always do) | `false` | CH1 |
| `sort` | a column's name, then `asc` or `desc`: `"net desc"` | the result's order | CH1 |
| `limit` | the number of rows to chart, after `sort` | every row, up to the kind's limit | CH1 |
| `y_scale` | `"linear"` or `"log"`; a log scale draws lines and scatters only | `"linear"` | CH5 |
| `colours` | a list of hex colours, given to the series from the first (`colors` reads the same key) | the theme's eight | CH5 |
| `overlay` | a list of `"trend"`, `"average N"`, `"mean"`, `"median"` and `"line V"`, each drawn per series and named under the chart | none | CH5 |

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

**What the spec doesn't hold:** fonts, the theme, legends' placement, ticks. The chart's look is
W12's, the same on screen and in a file, and a spec that can't set it can't get it wrong. Each
could become a key later, if someone needs it. Colours were on that list until CH5, which lets a
spec name a series' colour and keeps the rule that matters: the colour a spec names is the same
on screen and in the file ([Colour control](#colour-control-ch5e)).

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

This one, in `examples/reports/sales-by-category.toml` since CH3f, writes a bar chart of each
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
- **A value of the wrong kind:** "chart, line 2: `kind` is bars, hbars (horizontal bars), lines,
  scatter or area, not `pie`." "`stack` stacks bars; this chart draws lines."
- **A key the kind can't honour** (CH5): "`y_scale = "log"` draws lines or a scatter; this chart
  draws bars." "`stack = false`: an area always stacks; leave `stack` out." "`mean` reads a
  series against the axis, and a stacked chart piles them; `line` still works."
- **Two keys that disagree** (CH5): "`y_zero` starts the axis at zero, which a log scale can't
  reach; set one or the other." In a report, "an overlay is computed over the rows charted, and
  `limit` charts only some of them; drop `limit`, or group the rows in the query."
- **A colour that isn't one** (CH5): "`colours`: write a hex colour such as `#2a78d6`, not
  `red`."
- **An overlay that isn't one** (CH5): "`overlay`: `"trend"`, `"average 7"`, `"mean"`,
  `"median"` or `"line 100"`, not `"regression"`." "`average 30` needs 30 points; the chart has
  12."
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
| **Horizontal bars** | Moderate: long category names, a top-20 of customers | Moderate: the layout's axes swapped, labels measured on the left | **Yes, CH5b.** Asked for; a kind of its own, `kind = "hbars"` ([why](#horizontal-bars-ch5b)). |
| **Area** (stacked) | Moderate: shares over time, close to what stacked bars show | Moderate: a filled shape per series, drawn in egui as a strip of four-sided pieces, since egui fills only convex shapes; a polygon in SVG | **Yes, CH5c,** always stacked, with the splitting in the painter rather than the layout ([why](#stacked-area-ch5c)). |
| **Histogram** | High for a distribution (latencies, order sizes) | Small to draw, but binning the rows the grid holds bins a first page, not the table | **No kind,** confirmed in CH5. Binning belongs in the query (`width_bucket`, `floor(x / 10) * 10`), and the result charts as bars; CH5f's **Summarise** writes that query, so the bins see every row ([why](#statistics-sql-first-ch5f-and-ch5g)). |
| **Heatmap** | Moderate: day of week by hour | High: two category axes, a colour scale and its legend, labels on both axes | **No,** for now. |
| **Pie** | Low: hard to compare slices | Arcs, labels around them | **No.** Bars say it better. |

## A fuller chart

CH1 to CH4 gave a chart a text form and three kinds. What the track left for later was asked
for: a top-twenty of customers whose names don't fit under upright bars, shares over time,
prices whose range is three decades, one series that has to be red because it always is, and a
trend through a line. CH5 takes those up, with one rule holding it together: **whatever a key
can say, the layout must be able to draw.** A key that reads cleanly and then can't be placed
is worse than no key, because its problem arrives after the query ran. So `y_scale = "log"` is
refused on bars rather than drawn from an arbitrary baseline, and an area's `stack = false` is
refused rather than quietly ignored.

CH5 stays inside W12's model: one result, one picture. Small multiples are
[weighed below](#small-multiples-the-next-candidate) and left out; dashboards stay out, and
[Reports' scope](reporting.md#scope) does not move: every picture CH5 draws is still one
result's, in a console or as a file a report writes beside its others, so the wording CH3 put
there is still right word for word.

### Horizontal bars (CH5b)

`kind = "hbars"` draws the bars lying down: the categories up the left side, the values along
the bottom. It is **a kind of its own, not a key on bars.** `ChartKind` already decides the row
limit, the kind a result starts with from X's role, how hovering finds a point, and whether
stacking applies; a `horizontal = true` beside `kind = "bars"` would turn each of those into a
test of two fields, which is the same discriminator read in four places. It would also have to
live beside `stack`, since stacked horizontal bars are the natural pairing, leaving two
interacting booleans on one kind. A fourth kind costs one arm in each `match` the compiler
already checks.

~~And one more button in a toolbar that is a row of them.~~ **That part was wrong, and building
CH5 proved it.** The row held exactly one item more than it had: at five kinds with
**Summarise** beside them it wrapped at the window the app opens with, and the second row cost
the results panel 62 points — which took a small chart's plot to 23 against `MIN_PLOT_SIDE`'s
30, below which the layout draws nothing at all, so a result with no block charted as an empty
rectangle. A one-character label for Summarise still wrapped, so the row was already full at
CH5e. The kinds are one picker now, in the same idiom as the X, Y and Series pickers beside
them; the reasoning for a kind of its own is untouched, since it was never about the button.

`"hbars"` is short where the other kinds are short, and the sentence that refuses a bad value
spells it out: "`kind` is bars, hbars (horizontal bars), lines, scatter or area, not `pie`".
`"horizontal_bars"` would need no gloss, but no other kind is two words.

What it means for the layout:

- **The extents are named before they are used.** Today `layout` reads `plot.min.x` from the
  widest Y tick label and places categories across the width. CH5b first renames those two
  directions as the value extent and the category extent, with no change to any mark, and only
  then adds the kind that swaps them. The first commit's golden SVGs are unchanged byte for
  byte, which is what makes the second commit reviewable.
- **The tick logic needs no change at all.** `Ticks::nice` takes a range and a target count and
  knows nothing of direction. What changes is which extent it is given, and which label width
  it drives: upright, the value labels set the left gutter; lying down, the category names do.
- **The value ticks get the wider spacing.** Along the bottom, number labels sit side by side,
  so an hbars chart takes `X_TICK_SPACING`'s target (one tick per 90 points) where the upright
  chart took `Y_TICK_SPACING`'s (one per 48). A horizontal chart therefore gets fewer value
  ticks than the same chart standing up, which is right: the labels need the room.
- **The gutter is capped, and names are shortened by measure.** Long names are the reason for
  the kind, so the left gutter grows to fit them, to at most a third of the width; past that,
  names are shortened with an ellipsis to the width that is left, measured rather than counted.
  Upright bars keep `MAX_LABEL_CHARS`, whose 20 characters are about what a slot holds; lying
  down, the gutter is the constraint and a measure is the honest test.
- **Zero stays where it belongs.** Bars grow rightwards from the zero line and leftwards for
  negatives, and the zero line is drawn vertical when values fall either side of it.
- **Hovering** finds the slot along Y rather than along X: one more arm in `Drawing::hit`, which
  already branches on the kind for a scatter.
- **Stacking is free.** `stack` applies, and the pieces lie left to right from zero. Stacking is
  worked out in the values, not in the placement, so only where a piece goes changes.

Its row limit is `MAX_BARS`, the same 250: it is bars.

### Stacked area (CH5c)

`kind = "area"` fills the band between each series and the one below it. **An area always
stacks.** Unstacked areas hide whichever series is behind the front one, and fixing that needs
translucent fills, which means an alpha channel neither `Paint` nor `Rgb` carries and dark
files the track keeps out. So `stack = true` on an area is accepted, since it says what is
true, and `stack = false` is a problem: "an area always stacks; leave `stack` out". **Save to
query** never writes `stack` for an area, as CH2 never writes `stack = false`.

**One mark per series, tessellated by whoever paints it.** The layout emits one new
`Mark::Band`, a closed ring of points: the series' top edge, then the edge below it reversed.
The SVG writes it as one `<polygon>`. egui fills only convex shapes, so the app's painter
splits the ring into four-sided pieces at each X, in the painter, where the limitation lives
rather than in the layout.

The layout must not do the splitting. `dbrover-chart`'s whole shape is that the layout is the
one description of a picture and everyone else only paints marks; a strip of 250 quads in the
mark list would make the SVG write 250 polygons where one would do, bake an egui constraint
into a crate that knows nothing about egui, and leave the golden files unreadable.

**The two paths draw the same region, and this is provable rather than hopeful.** Between two
consecutive X positions a band's upper and lower edges are each a straight segment, so the
quad spanning them covers exactly the region the polygon covers there: no curve is being
approximated, and the union of the quads is the polygon. The two fills can therefore differ
only in how a rasteriser lands on the shared edges. A test bounds that: the chart's SVG is
rasterised with resvg and compared pixel by pixel against the tab's painted frame, allowing
differences only on pixels adjacent to an edge, as CH4b's report-chart PNG is already checked
by its pixels rather than its size.

**Its own row limit, `MAX_AREA_POINTS`, is 5,000.** A band's cost grows with the points *and*
the series, unlike a line's: 50,000 points across eight series is a 400,000-coordinate picture,
several megabytes of SVG, and a report would write it unattended. At 5,000 the widest band is
about 10,000 coordinates, roughly 120 KB a series, a file that still opens in a browser. A
stacked area over more than a few thousand X positions is a solid block anyway. Past the limit
a report fails and names it, as it does for bars' 250.

### A log scale (CH5d)

`y_scale` takes `"linear"` (the default) or `"log"`. A string rather than a `y_log = true`
because the axis's scale is one choice among several: a later `"symlog"`, or an `x_scale`, is
then a value and not another boolean. That is the shape that absorbs the growth the Risks
section fears, and it reads as `sort = "net desc"` already reads, one key carrying a small
phrase.

- **Zero and negative values are gaps.** A log axis cannot place them, and the crate's rule is
  that what can't be drawn is a gap, never a clamped epsilon and never a dropped row that
  nobody hears about. They are counted and named in the line under the chart with their own
  reason: "3 gaps in net: zero or negative on a log scale". When no value is left to place, the
  chart draws as a chart with no points and the note says why.
- **The ticks are decades,** 1, 10, 100, with 2 and 5 inside each decade while the axis spans
  few enough of them to label without collision; past about six decades only the decades are
  labelled. This is `nice_step`'s own 1-2-5 family read once per decade rather than a new rule.
  The axis rounds out to the decade below the lowest value and above the highest, and
  `MAX_TICKS` still caps the count.
- **`y_zero` and a log scale together are a problem,** known from the keys alone: "`y_zero`
  starts the axis at zero, which a log scale can't reach; set one or the other."
- **Bars, hbars and area refuse it.** A bar's length is read as a ratio, and a bar from a log
  axis's arbitrary baseline misstates exactly that; a stacked band's sum is not the sum of its
  positions. So `y_scale = "log"` draws lines and scatters, and on the other kinds it is a
  problem saying so. This is the rule at the top of this section doing its work.
- **Determinism.** Positions use `f64::log10`, whose last bit differs between platforms' maths
  libraries. The SVG rounds every coordinate to a hundredth of a point, which swallows a
  difference of that size unless a coordinate falls exactly on a rounding boundary. The guard
  is the cross-platform golden test the example's SVG already runs on all three platforms; if
  it ever does bite, the fix is to round the mantissa before scaling, as `decimal_exponent`
  already avoids `log10` for the tick steps.

A log X is left for later: X is already placed three ways (categories, numbers, times), and a
fourth doubles that layer's work for a scatter of two numbers.

### Colour control (CH5e)

`colours` takes a list of hex colours, given to the series from the first: `colours =
["#e34948"]` makes the first series red and leaves the rest as they were. `colors` reads the
same key: it is another spelling, not a misspelling, and refusing it would be a papercut for
no gain; **Save to query** always writes `colours`, and the "did you mean" suggester leaves
both alone.

**Per series, not a named palette.** A palette name is a word the app then owns forever, each
one a compatibility promise, and a user whose brand needs three particular hues cannot get
them from any name we ship. A list gives exactly those hues with no vocabulary to maintain,
and a list shorter than the series recolours only what it names, which is the common case —
one series picked out, in one short key.

Hex only, `#rrggbb` or `#rgb`, case ignored. Named CSS colours would be 148 names to carry and
to keep matching between egui and the SVG. A bad value says: "`colours`: write a hex colour
such as `#2a78d6`, not `red`."

**How a file still matches the screen.** Today the layout emits `Paint::Series(index)` and each
painter resolves it from its own array (`SERIES_LIGHT` or `SERIES_DARK` on screen,
`FILE_COLOURS.series` in a file), which is why a file is the light theme on white whatever the
app's theme. A spec's colours override **both** arrays at the same index, through one resolver
that `FileColours::of` and the app's palette lookup each consult before their own array. So a
series the spec names is that exact hue on screen and in the file, and a series it doesn't name
keeps each surface's own stepped hue. The layout still emits only `Paint::Series(index)`, so
nothing about "whoever shows a chart only paints the marks" changes.

**The dark theme gets a hint, not a correction.** A user's hex is used as given, on both
surfaces. Adjusting it for contrast would make the screen and the file disagree again, and
would surprise someone who pasted a brand colour. But how a hue lands on the other theme is
the one thing the TOML doesn't show, so when a named colour's contrast against the current
theme's background falls below 3:1 — WCAG's ratio for graphical objects, which the palette's
own "stepped for its theme's background" is already reaching for, the chart tab shows a hint
beside the chart: "`#1d1f26` is hard to see on the dark theme". A hint, not a problem: the
chart draws and a report runs. It is console-only, since a report has nobody to read a hint and
its file is always light on white, where the question doesn't arise.

### Statistics, SQL first (CH5f and CH5g)

Statistics split in two, and the split is the point.

**What sees every row belongs in SQL.** Distributions and percentiles are exactly the
statistics that are wrong when computed over a page: a histogram of the first 250 rows is a
histogram of 250 rows, whatever the table holds. So DbRover writes the SQL instead of binning
the rows it has, which puts the work where every row is, and leaves the user with a statement
they own, can read, can change, and can paste into a report's `query` or a repository. This is
the same judgement the track already made when it refused a histogram kind; CH5 keeps the
refusal and adds the affordance that makes it easy.

**What is honest over the points already drawn belongs in the chart.** A trend through the
points on screen, a moving average of them, a line at their mean or median: each is a statement
about those points, and the points are visible. Those three the chart layer gains, plus a line
at a value the user names.

#### Summarise writes SQL (CH5f)

A menu on a column of numbers, in the grid's header and beside the chart's toolbar, offering
for that column:

- **Distribution** into 10, 20 or 50 buckets: a statement giving each bucket's lower bound and
  its count, which charts as bars with no further work.
- **Percentiles**: the median, p90, p95 and p99 with the smallest and largest value, as a row
  each so they chart, or as one row.
- **Copy the SQL**, for anyone who would rather their file wasn't touched.

**It never reads the page to decide anything.** The bucketing statement computes its own range
in SQL, over a subquery of the minimum and maximum, so the buckets come from every row; the
number of buckets is picked from the menu, not derived from what happened to be fetched. That
property is the reason the affordance exists, so it is the first thing its tests check.

**Where the statement goes.** It is written as a **new statement below the original, with its
own `[chart]` block above it, and the original is left untouched.** The query that produced the
result on screen is the user's work and the thing they will run again; replacing it destroys it,
and "it can be undone" is a thin promise about a file that may be committed. Written below, the
console reads as the derivation it is, the query and then the summary of it, which is how
someone would have written it by hand, and the statement that ran stays the statement that ran,
so the result on screen still matches the text above it and CH1's search for a statement by its
text keeps working. Opening a new console tab was the alternative; it separates the summary
from the query it came from, which is the one thing worth keeping together, and it leaves undo
with nothing to undo.

**Where below.** Immediately after the line the statement ends on, as lines of its own. When
another statement shares that line (`SELECT 1; SELECT 2`), the insertion still goes after the
whole line, so the neighbour is never split and no text the user didn't act on moves; the
summary then lands after both, and the toast says where it went. One rule, and no refusal,
unlike **Save to query**, which has to refuse a shared line because it writes *above* the
statement.

**Undo** is the editor's, in one step, as **Save to query**'s is: the statement and its block
are one replacement in the console's text, so ⌘Z takes back both or neither.

**It writes the console's own dialect.** Bucketing uses arithmetic every engine has
(`floor((x - low) / width) * width`), so **Distribution** is offered everywhere. Percentiles
differ engine by engine, so they come from a per-dialect template through `dbrover-sql`'s
`Dialect`, which the `chart` tool already uses; where an engine has no percentile function the
menu's item is disabled with the reason in its tooltip, rather than writing a statement that
fails when it runs.

**Summarise is a console affordance only,** not in reports and not over MCP. It is an editing
gesture: it writes SQL into text somebody is looking at, and a report file has neither a result
on screen nor an editor gesture to hang it on. Someone who wants a distribution in a report
writes that bucketing SQL into the chart's own `query`, which is the statement Summarise would
have written; the manual shows it so it can be copied there. An assistant over MCP writes the
SQL itself, which is the whole point of statistics in SQL, and `chart`'s description already
tells it to group in the statement.

#### The overlays (CH5g)

One key, `overlay`, taking a list of short phrases:

```toml
overlay = ["trend", "mean", "average 7", "line 100"]
```

- `"trend"` — a least-squares straight line through the series' points.
- `"average N"` — a moving average over N points, N from 2 to the points charted.
- `"mean"`, `"median"` — a line across the plot at that statistic of the series' values.
- `"line V"` — a line across the plot at the value V, the plain reference line.

**One key, not four booleans.** Every overlay anyone asks for next (a band, a target, a second
fit) is then a phrase in a list the reader already understands, not another key in a spec
whose growth is this plan's own named risk. It follows `sort = "net desc"`: one key, one
small phrase, parsed and explained in one place.

**One overlay per series,** in that series' colour, drawn thinner and dashed so it reads as
derived rather than measured: the mean of two different measures is not a number worth
drawing. Dashes mean the line marks gain a stroke, a width and a dash length:
`stroke-dasharray` in the SVG, egui's dashed line on screen, one mark driving both.

**Where an overlay would mislead, it is refused.**

- **On an area, and on stacked bars,** `trend`, `average`, `mean` and `median` are a problem:
  the series are drawn piled on each other, so a line at one series' own values sits where
  nothing was drawn. `"line V"` is still allowed on stacked bars, because an absolute reference
  is read against the axis and a stack's total is too.
- **`average N`** with N below 2, or above the points charted, is a problem: "`average 30` needs
  30 points; the chart has 12."
- **Over categories, a fit is by position.** `trend` and `average` are computed against the X as
  drawn: the slot's index for categories, the value for numbers and times. Uniform slots make
  the index the distance, so the fit matches what the eye sees, and the usual case, a trend over
  months that `strftime` made text, works rather than being refused on a technicality. What the
  reader must know is which it was, so the note says it: "by position" for categories, "by
  month" for numbers and times.

**Gaps stay gaps.** An overlay skips a gap rather than interpolating it, and says it did: a
trend over 248 of 250 points reads "Trend over 248 of the 250 rows charted: 2 gaps". A moving
average whose window holds a gap gives a gap, not a shorter window, since a window that quietly
changes width changes what the line means halfway along.

#### How the overlays stay honest

A chart already draws the first 250 bars or 50,000 points and says so, and a report fails past
a limit rather than draw part of the data. A trend over a first page is a trend over a page, so:

- **The line under the chart names every overlay and what it was computed over,** always. Where
  the rows charted are all the rows: "Charting 250 rows · Trend and mean over those 250 rows, by
  month". Where they are not, the clause that already says so reads straight into it: "Charting
  the first 250 of 1,204 rows; group them in the query to chart them all · Trend and mean over
  those 250 rows, by month". There is no wording in which an overlay appears and its basis
  doesn't.
- **In a report, `overlay` together with `limit` is refused,** a key problem, exit 3, when the
  file is read, before anything runs: "chart 1: an overlay is computed over the rows charted,
  and `limit` charts only some of them; drop `limit`, or group the rows in the query." Without
  `limit`, a report already fails past a kind's row limit, so this closes the only remaining way
  an unattended file could carry a statistic over a deliberate subset with nobody there to read
  the caption. That is the misleading chart the Risks section is about, and it is cheap to make
  impossible.
- **In a console the same spec is allowed,** because the note is under the chart and the person
  set the limit a moment ago. The asymmetry is the one the row limit already has (the tab draws
  the first rows and says so, a report fails), and it is why `limit` with `overlay` is read in
  all three places but refused in one.
- **Over MCP it draws and says so,** as CH4 settled for every limit: an assistant reads the
  reply, so the picture's last line and the reply's summary both say what the overlay was
  computed over, including when the project's row limit cut the rows.

### Small multiples: the next candidate

Facets (one panel per value of a column, the same chart in each, sharing axes) are the kind
most likely to be asked for next, and they are not in CH5. The cost, so the decision is here
when someone asks: the layout places one plot today, with a gutter measured once and a legend
placed once, so facets mean a grid of plots, a shared Y range computed across them, a tick
spacing decided per panel from a much smaller height, and labels that have to survive at a
quarter of the size; then the same again for hovering, which must find the panel before the
point. That is the layout's largest single change since W12, larger than everything in CH5 put
together, for a picture a `series` chart already tells most of. It would not move
[Reports' scope](reporting.md#scope): one result, one picture, panelled. It waits for someone to
ask.

## Command line and MCP

### `dbrover report run`

Charts are written as outputs are, with nothing new to type:

- **Standard output** lists each chart's path with the other files', so a script picks them up
  the same way.
- **`--json`** reports each chart as an `output_written` event, the type the app and CI already
  read, with its rows and size.
- **Exit codes** as [RP6's](reporting.md#exit-codes): 3 for a chart's key problems, 1 for a
  column it can't find or a file it can't write, 5 for its row or series limit. CH5's keys add
  no code: everything knowable from the keys alone is 3, which is where a log scale on bars,
  `y_zero` beside a log scale, a bad hex, an unknown overlay and `overlay` with `limit` all
  land, before a process starts; an overlay refused by the chart's shape, such as a mean on a
  stacked chart, is 3 too, since the kind is in the keys; a column or role the result turns out
  not to have is 1, in the chart's step; and an area past `MAX_AREA_POINTS` is 5, with the bars'
  250 and the eight series.

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
- **CH5's keys** are all read here, by the same reader, with the same problems as a block's: the
  new kinds, `y_scale`, `colours`, `overlay` and `overlay`. `overlay` with `limit` is drawn and explained
  rather than refused, unlike in a report, for CH4's reason: an assistant reads the reply, so
  the picture's last line and the reply's summary both say what each overlay was computed over,
  including when the project's row limit cut the rows. **Summarise** has no tool: an assistant
  writes the SQL itself, which is the point of keeping distributions and percentiles in SQL.

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
  on screen. The font file's advances leave out kerning, which egui applies: a test bounds the
  difference at a point, or 3% of a label of kerned capitals such as "WAVY".
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

#### CH4 confirmed

Started and built on 2026-11-07, after CH3. The plan stands; building it settled these details:

- **Arguments:** `data_source`, `sql`, `chart` (a JSON object of a block's keys, all optional)
  and `format` (`png`, the default, or `svg`). The picture is 800 by 450 points, a report's
  chart's default; there is no size argument, since a client shows the picture at its own size.
- **The spec** is read by the same reader as a block's and a report's, from JSON
  (`ChartSpec::from_json`), each problem naming its key: "chart.kind: `kind` is bars, lines or
  scatter, not "pie"". It is checked before the statement runs.
- **The statement** runs through the same steps as `query`'s, and is logged as a `chart` call
  and kept in the history. A data source the project doesn't serve to MCP clients, such as a
  SQL Server one without an MCP login, is refused as it is by every tool: the project has no
  data source of that name.
- **Drawing** is a report's file's (`dbrover_chart::file_svg`, which the report's run uses
  too): the font file's measure, and the line under the chart. Past the kind's rows or colours,
  the tool draws the first ones and says so, as the tab does, rather than fail as a report does:
  an assistant reads the reply.
- **The row limit:** when it cut the rows, the reply has `truncated` and `truncated_reason`, and
  the picture's last line ends with "only the first N rows were read, the project's row limit".
- **The reply** is the picture, then the summary as JSON text, which is also the structured
  content: kind, x, y, series, rows read, points, rows left out and the line under the chart.

### CH5 — A fuller chart (L)

The kinds and keys CH1 to CH4 left for later, and statistics: SQL for what must see every row,
overlays for what is honest over the points drawn. Settled in
[A fuller chart](#a-fuller-chart). One GitHub stack, each layer one pull request, useful and
tested on its own, with synthetic data only (the SQLite shop of `examples/reports/shop.sql`).

1. **CH5a — Plan (XS):** CH5's decisions confirmed against the code, the keys in the tables
   above, and the roadmap's entry. Docs only.
2. **CH5b — Horizontal bars (S):** `kind = "hbars"`. First the value and category extents named
   with no mark changed, then the kind: the gutter capped at a third of the width with names
   shortened by measure, the value ticks at the wider spacing, a vertical zero line, stacking,
   and hovering by the slot along Y. *Exit:* a top-twenty of customers by net charts as
   horizontal bars whose names are readable, on screen and in the exported SVG. *Tests:* the
   extents' commit leaves every golden SVG unchanged byte for byte; the layout table-driven (the
   gutter's cap, a name shortened to the gutter, the value ticks' target, negatives either side
   of zero, stacked pieces left to right); `Drawing::hit` by slot along Y; a golden SVG; a
   kittest test of the fourth kind in the toolbar and of **Save to query** writing it.
3. **CH5c — Stacked area (S–M):** `kind = "area"`, always stacked; `Mark::Band` as one ring per
   series, written as one `<polygon>`, split into quads by the app's painter; `MAX_AREA_POINTS`
   and its failure in a report. *Exit:* category shares by month draw as a stacked area on
   screen and in the SVG, the two pictures the same. *Tests:* the ring's points per series,
   table-driven, with negatives and with gaps; `stack = false` refused and `stack = true`
   accepted; the point limit, and a report past it exiting 5; a golden SVG; the SVG rasterised
   with resvg against the tab's painted frame, differences allowed only beside an edge.
4. **CH5d — A log scale (S):** `y_scale`; decade ticks with 1-2-5 inside while few decades;
   zero and negatives as counted gaps; `y_zero` and the bar kinds refused. *Exit:* a scatter of
   prices spanning three decades reads at every size, and a zero in the column is a gap the note
   names. *Tests:* the log ticks table-driven (one decade, few, many, past `MAX_TICKS`); the gap
   count and its reason in the note; every contradiction as a key problem, table-driven; the
   golden SVG built on all three platforms.
5. **CH5e — Colour control (S):** `colours` and its `colors` spelling; the override consulted by
   both palettes; the tab's contrast hint. *Exit:* `colours = ["#e34948"]` makes the first
   series that red on screen, in the dark theme and in the SVG, and the other series keep their
   own. *Tests:* the resolver at an index for both arrays and for a list shorter than the
   series; the hex reader's table of bad values; the alias read and `colours` written back; the
   hint's threshold either side of 3:1; a golden SVG.
6. **CH5f — Summarise writes SQL (M):** the column menu, its statements per dialect, the
   insertion below the statement with a `[chart]` block, one undo step, **Copy the SQL**, and
   the disabled percentiles item where an engine has none. *Exit:* **Summarise › Distribution**
   on a price column writes a bucketing statement below the original, which runs and opens as
   bars, with the original untouched and ⌘Z taking the whole insertion back. *Tests:* the SQL
   per dialect as snapshots, with the range read in SQL and never from the rows fetched; the
   insertion point table-driven (own line, sharing a line, last in the file, no trailing
   newline); undo in one step; the disabled item's reason; a kittest test of the menu.
7. **CH5g — The overlays (M):** `overlay` and its phrases; one per series, dashed, through a
   stroke on the line marks; the note's sentences; every refusal, and `limit` with `overlay`
   refused in a report but not in a console. *Exit:* `overlay = ["trend", "mean"]` on monthly
   totals draws both dashed in the series' colour, and the note says what each was computed
   over; the same spec with `limit` in a report exits 3 before the run starts. *Tests:* the
   reader's table of problems; each overlay's values over known points, with gaps skipped and a
   gap in a window giving a gap; the note's wording with and without a limit, and "by position"
   over categories; the report's exit 3 and the MCP reply's wording; a golden SVG per overlay.
8. **CH5h — Manual and demo note (S):** the new kinds, keys and **Summarise** written into the
   manual's charts page, and this milestone's demo note. It **extends that page rather than
   adding one:** the manual gains a charts page of its own alongside this plan, so CH5h adds the
   new kinds to the kinds it already lists, the new keys to its key table, and a section on
   **Summarise** beside the note about binning in SQL — and nothing that page already says is
   written again here. *Exit:* the page documents every key in
   [What a chart spec holds](#what-a-chart-spec-holds), with no key documented twice.

#### CH5 confirmed

Built on 2026-11-17, after RS7g, in seven pull requests off `main`. The plan stands; building it
settled these details, and departed from it in five places, each noted below.

- **The extents, and the frame.** The layout named its two directions after the axes they
  happened to be. They are now the *value* extent and the *category* extent, and every mark goes
  through a `Frame` holding both and whether they are swapped. The renaming commit changed no
  mark: the example report's SVG was byte-identical and the crate's tests passed unchanged, which
  is what made the kind reviewable on its own. What the frame deliberately does not make
  symmetric is where a label sits, since an upright chart's value labels and a horizontal one's
  are at different ends of their axis, so `value_label` and `category_label` each branch once.
- **Hovering needed no new arm** — one fewer than the plan expected: the pointer is read back
  through the frame, so one slot lookup serves both bar kinds. `Drawing::x_of` became
  `Drawing::guide`, which gives the whole segment, since a vertical line is wrong lying down.
- **A departure, CH5d: decade ticks take how many labels *fit*,** a line's height apiece, rather
  than `Ticks::nice`'s comfortable target of one per 90 points. Read as a decade count alone, the
  plan's rule crowded a short plot badly — the first golden SVG had ten labels in 147 points. An
  even axis can pick any step, so a target suits it; a log axis's granularity is the decade, and
  the only question is collision.
- **Two of the three logarithms are gone.** The axis's ends are powers of ten whose exponents
  come from `decimal_exponent`, the trick `ticks.rs` already used for the steps. Only each
  value's own logarithm needs `log10`.
- **A departure, CH5d: the cross-platform guard is a `const` in the crate's tests,** a log
  scatter compared byte for byte, rather than a second chart in the example report: it gives the
  same three-platform comparison without a new output file or the manual debt that would follow.
- **A departure, CH5e: a single colour reads as a list of one,** as `y` takes one column's name
  or a list, and `colours = []` is a problem. The other spelling, `colors`, is read as the key
  but never suggested or listed, so `colour` and `color` are both pointed at `colours`. **Save to
  query** writes no colours at all — the toolbar sets none — so a spec's `colors` survives a save
  with its spelling and its colours untouched.
- **A departure, CH5f: the generated statement wraps the original as a CTE.** An original that is
  itself a `WITH` then nests one, which PostgreSQL, SQLite and DuckDB allow and SQL Server and
  MySQL do not; the statement is the user's, below theirs and untouched, so it is theirs to
  adjust. **Copy the SQL** is a toggle beside the chart rather than a fifth menu item, so each
  summary can be had either way without a menu three levels deep.
- **DbRover's own SQLite has no `FLOOR`.** `libsqlite3-sys` doesn't define
  `SQLITE_ENABLE_MATH_FUNCTIONS`, so the first bucketing statement ran against a system `sqlite3`
  and failed inside the app. SQLite gets `CAST(… AS INTEGER)`, a floor for a value that can't be
  negative; not a cast everywhere, since BigQuery's rounds.
- **A departure, CH5g: a window longer than the chart is a sentence, not a problem.** How many
  points there are isn't known until the rows arrive, long after the keys are read, so
  `average 1` and below is a key problem and a longer window isn't drawn while the note says why
  — which keeps the key from looking inert, and puts the sentence in a report's caption.
- **Overlays are computed from the values and then placed,** not fitted to where the points
  landed: on a log axis the mean of the values is not the mean of their positions. The note's
  basis comes before the gaps it skipped, so the clause reads as one while the basis is still
  always named.
- **A GitHub Linux runner has no GPU,** and `egui_kittest`'s only renderer is a wgpu one, so
  CH5c's comparison of a band's two fills uses `egui::epaint::Tessellator` on the CPU — the same
  `Shape::convex_polygon` calls the painter makes, with feathering off — against resvg's
  rasterisation of the polygon. The claim at risk is geometric, so it tests it just as well.

**Exit:** on the SQLite shop, a top-twenty of customers draws as horizontal bars with readable
names; category shares by month draw as a stacked area that matches its SVG; a scatter of
prices on a log scale reads at every size; `colours = ["#e34948"]` holds on both themes and in
the file; **Summarise › Distribution** writes bucketing SQL below the statement, which charts
as bars; `overlay = ["trend", "mean"]` draws both with the note naming their basis, and the
same spec with `limit` in a report exits 3.

### Order

CH1 and CH2 are the app's and can come any time after W13. CH3 needs CH1b's spec, and is best
after RS9 is designed into place, though it doesn't depend on it. CH4 needs CH3b and nothing
from reports.

CH5 comes after RS7f and RS7g, the warehouse handoff planned in
[Reports at scale](reports-at-scale.md#rs7-against-todays-code), and before RS8: it touches
none of the report crate's scale work, so it
waits only because the handoff was already in flight. Within it, the kinds come first (CH5b,
CH5c) because each is demonstrable on its own and neither needs anything else. The log scale
(CH5d) and colour control (CH5e) come before the overlays, which need both: an overlay has to
know what a log axis does to a trend line, and it needs a stroke and a colour that aren't a
series' own. Summarise (CH5f) comes before the overlays (CH5g) so that the honest path for a
distribution exists before the convenient one tempts anyone to fake one, and because it is the
layer with the most to get wrong, since it writes into the user's own text, so it is best built
while attention is on it. The manual (CH5h) is last, as every track's is.

## Left for later

What the track leaves out, each to be weighed when someone asks for it. CH5 takes up the kinds
and keys that were waiting here (horizontal bars, stacked area, colours and a log scale), so
they are no longer on this list; what is left of that bullet is below.

- **Re-resolving a toolbar choice:** when more rows change a column's role, the tab makes the
  block's or the starting choice again (fixed in CH3b), but a choice changed in the toolbar is
  kept as it was, even if its Y no longer holds numbers.
- **Copy for a report** gives no confirmation on screen that the chart was copied.
- **A size for the `chart` tool's picture:** 800 by 450 points, always.
- **Small multiples (facets):** the next candidate, with its cost weighed in
  [Small multiples](#small-multiples-the-next-candidate), and the largest change the layout has
  seen since W12.
- **Text annotations:** an arrow and a label at a point, which CH5's `overlay` does not cover —
  it draws lines computed from the data or at a value, not prose placed by hand.
- **A log X,** and a scale per axis beyond `y_scale`: X is already placed three ways, and a
  fourth doubles that work for a scatter of two numbers ([A log scale](#a-log-scale-ch5d)).
- **A named palette,** as against CH5's per-series `colours`: a name the app would own forever,
  which still wouldn't hold anyone's three particular hues
  ([Colour control](#colour-control-ch5e)).
- **Heatmaps and pies,** as [Which chart kinds to add](#which-chart-kinds-to-add) says; CH5
  doesn't revisit either.
- **Copy as Vega-Lite,** and an Excel chart in an Excel output, as
  [Options weighed](#options-weighed) says.
- **A `dbrover chart` command,** until a report of one chart proves too heavy.
- **A chart in the report's tab,** drawn from its last run's file: the tab lists the file, and
  opens it in the system's viewer.

CH3's two loose ends were tied in CH4b: a report chart's PNG is checked by its pixels, not only
its size, and the manual's Reports chapter shows the example's chart.

## Decisions

Proposed defaults; each was confirmed when its milestone started.

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
| Chart kinds | **`series` and stacked bars** (CH2), **horizontal bars and stacked area** (CH5); no histogram, heatmap or pie | The usual `GROUP BY` shape charts without a pivot; binning stays in SQL, where it sees every row. |
| Horizontal bars | **A kind of its own,** `kind = "hbars"`, not a key on bars | One arm per `match` the compiler checks, instead of a second boolean beside `stack` read in four places. |
| Stacked area | **Always stacked,** one `Mark::Band` a series, split into quads by the painter | Unstacked areas would need translucency and dark files; the layout stays the one description of a picture, and the SVG writes one polygon where egui needs many. |
| A log scale | **`y_scale = "log"`,** lines and scatters only; zero and negatives are gaps | A bar from a log baseline misstates the ratio it looks like; what can't be placed is a gap, as a null is, never a clamped epsilon. |
| Colours | **Per series by position,** hex only, overriding both palettes at that index | A brand's hues without a palette name to maintain; the colour a spec names is the same on screen and in the file. The dark theme gets a hint, not a correction. |
| Statistics | **SQL for distributions and percentiles** (Summarise), **overlays for what is honest over the points drawn** | A histogram of a first page is a lie; a trend through visible points is not. The SQL is the user's to keep, read and paste into a report. |
| Where Summarise writes | **A new statement below the original,** the original untouched, one undo step | The query that made the result on screen survives; the console reads as the derivation it is. Console only: a report has no result on screen and no editor gesture. |
| Overlays and `limit` | **Refused in a report, allowed in a console** | An unattended file never carries a statistic over a deliberate subset with nobody to read the caption; the tab's note is right there. |
| Facets | **Not in CH5,** with the cost written down | One result, one picture, still; the layout's largest change since W12 waits for someone to ask. |
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
- **A spec that keeps growing.** CH1 weighed ten keys and worried about colours, log scales and
  annotations; CH5 adds three of those and takes the count to fourteen, which is the risk
  arriving rather than being avoided. Two things hold it: every new key is weighed in the table
  it joins, with its values, its default and what a bad value says; and a key that carries a
  small phrase (`sort = "net desc"`, `overlay = ["average 7"]`) absorbs the next request without
  a key, which is why `overlay` is one key and not four booleans. The
  [Options weighed](#options-weighed) table is still where a larger format would be argued
  again, and the honest trigger for that argument is a spec whose keys no longer fit one screen
  of a comment block.
- **A statistic that misleads.** An overlay is computed over the points drawn, not the table, so
  a trend over a first page is a trend over a page. Three guards, in order of strength: a report
  refuses `overlay` with `limit` outright and already fails past a kind's row limit, so an
  unattended file can't carry one; the line under every chart names each overlay and what it was
  computed over, in every wording, including "by position" when the fit was against category
  slots; and an overlay is refused where the picture would hide its basis: on an area and on
  stacked bars, where the series are piled, and with a window longer than the points. Gaps stay
  gaps and are counted, rather than being interpolated into a smoother line.
- **Summarise writing into the user's own text.** It edits a file that may be committed. It only
  ever inserts, never replaces: a new statement below the original, which is left untouched, in
  one replacement so one undo takes it back, and after the whole line when a neighbour shares
  it, so no text the user didn't act on moves. **Copy the SQL** is there for anyone who wants
  none of it.
- **Two fills for one area.** egui's tessellated quads and the SVG's polygon are two paths to
  one picture. They cover the same region exactly, since a band's edges are straight between
  consecutive X positions, so only a rasteriser's treatment of the shared edges can differ; the
  test rasterises the SVG and compares it with the painted frame, allowing differences only
  beside an edge.
- **A log position from `log10`.** The last bit of `f64::log10` differs between platforms' maths
  libraries, and a report's SVG is compared byte for byte. Rounding each coordinate to a
  hundredth of a point swallows a difference of that size unless one falls exactly on a rounding
  boundary; the guard is the golden test built on all three platforms, and the fix, if it ever
  bites, is to round the mantissa before scaling, as `decimal_exponent` already avoids `log10`
  for the tick steps.
- **Moving code W12 and ER1 share.** CH3b moves SVG helpers and fonts out of the app; ER1's
  diagram export uses them too. The move is behaviour-preserving, with ER1's and W12's export
  tests run unchanged.

## What stays out

- Dashboards: a page, file or tab of several charts.
- Charts that refresh, on a timer, a server or a live connection.
- Interactive or HTML charts in files, and JavaScript in DbRover.
- A semantic layer: measures, dimensions or metrics defined once for many charts.
- Vega-Lite, Plotly or another charting runtime in the build.
- Fonts and themes per chart; dark files. A series' colour comes in with CH5; the theme a chart
  is drawn in does not, so a file stays light on white.
- A page of small multiples drawn from one result, until someone asks
  ([Small multiples](#small-multiples-the-next-candidate)).
- Statistics computed over rows the chart never read: a distribution or a percentile belongs in
  SQL, which CH5f writes.
- Charts in PDF; [Reports](reporting.md#scope) keeps pixel-perfect PDF layouts out.
- A `dbrover chart` command, until a report of one chart proves too heavy for the need.
