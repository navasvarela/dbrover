# Charts

A result often answers its question better as a picture: totals by month, a measure over time,
one measure against another. DbRover draws that picture from the rows a query has already
returned, without running anything again, and writes it where you need it.

- **In a console,** **Chart** beside **Grid** draws the result as you pick the kind, the columns
  and the series in the toolbar, and **Export ›** saves what you see as SVG or PNG.
- **Written above a statement,** a `[chart]` block makes the query open as that chart on any
  machine, after any edit to its `WHERE`, and puts the chart in a pull request with the query.
- **In a report,** a `[[chart]]` writes the chart as a file beside the report's CSV, Excel and
  Parquet files, every time it runs.
- **To an AI assistant,** the MCP server's `chart` tool answers a question with a picture.

The same five kinds and the same keys serve all four. This chapter is the one place that says
what a chart can be; [Reports](reports.md#charts) and
[Connecting an AI assistant](mcp.md) add only what is their own.

## The five kinds

Each example below is a statement with the chart that draws it written above it, as
[A chart in the query](#a-chart-in-the-query) explains. They run on a small shop database of
customers, orders and the lines of those orders.

### Bars

Bars give each row a slot of its own, in the result's order, and grow from zero. They read best
over text along the bottom: a month, a status, a category.

```sql
-- [chart]
-- kind = "bars"
-- x = "month"
-- y = "revenue"
-- title = "Revenue by month"
SELECT strftime('%Y-%m', o.placed_at) AS month, ROUND(SUM(t.total), 2) AS revenue
FROM orders o
JOIN order_totals t ON t.id = o.id
WHERE o.status <> 'cancelled'
GROUP BY month
ORDER BY month;
```

![Revenue by month as six bars, 2026-01 to 2026-06, each between 1,700 and 2,500, titled
Revenue by month, with "Charting 6 rows" under them](images/chart-bars.png)

### Horizontal bars

`kind = "hbars"` lays the bars down: the categories up the left side, the values along the
bottom. Names too long to fit under an upright bar fit beside a horizontal one, which is what
the kind is for — a top-twenty of customers, a list of error messages, anything named rather
than numbered.

```sql
-- [chart]
-- kind = "hbars"
-- x = "customer"
-- y = "revenue"
-- sort = "revenue desc"
-- limit = 5
-- title = "The five best customers"
SELECT c.name AS customer, ROUND(SUM(t.total), 2) AS revenue
FROM orders o
JOIN customers c ON c.id = o.customer_id
JOIN order_totals t ON t.id = o.id
WHERE o.status <> 'cancelled'
GROUP BY customer
ORDER BY revenue DESC;
```

![Five horizontal bars, the customers' names in full down the left side and revenue along the
bottom from 0 to 2,000, the longest bar at the top](images/chart-hbars.png)

The left gutter grows to fit the names, up to a third of the width; past that a name is cut with
an ellipsis to the room it has. Everything else is bars: the slots, the growth from zero, the
stacking, and the 250-row limit.

### Lines

Lines place numbers and dates along the bottom by value and join the points in that order, so
an unsorted result doesn't zigzag. They are what a measure over time wants.

```sql
-- [chart]
-- x = "week"
-- y = "revenue"
-- title = "Revenue by week"
SELECT date(o.placed_at, 'weekday 1', '-7 days') AS week, ROUND(SUM(t.total), 2) AS revenue
FROM orders o
JOIN order_totals t ON t.id = o.id
WHERE o.status <> 'cancelled'
GROUP BY week
ORDER BY week;
```

![Revenue by week as one line of twenty-seven points over six months, rising and falling
between nothing and a thousand, its bottom axis labelled Jan 2026 to Jun
2026](images/chart-lines.png)

The `week` column holds dates, so this chart needs no `kind`: a date along the bottom draws
lines by itself, and the ticks under it are dates rather than one label per row.

### Scatter

A scatter places both axes by value and draws a point per row: one measure against another, or
every row where grouping them into a line would hide how they spread.

```sql
-- [chart]
-- x = "day_of_year"
-- y = "total"
-- title = "What each order came to"
SELECT CAST(strftime('%j', o.placed_at) AS INTEGER) AS day_of_year,
       ROUND(t.total, 2) AS total
FROM orders o
JOIN order_totals t ON t.id = o.id
WHERE o.status <> 'cancelled'
ORDER BY day_of_year;
```

![A scatter of fifty-five orders, the day of the year along the bottom from 0 to 200 and each
order's total up the side from 0 to 600, the points spread across the whole
area](images/chart-scatter.png)

### Area

`kind = "area"` fills the band between each series and the one below it, for shares of a total
over time. **An area always stacks** — unstacked areas hide whichever series is in front — so
it wants a `series`, or several columns of numbers; see
[Several series on one chart](#several-series-on-one-chart) for where those come from. Writing
`stack = true` on one is allowed, since it says what is true; `stack = false` is a problem.

```sql
-- [chart]
-- kind = "area"
-- x = "month"
-- y = "revenue"
-- series = "country"
-- title = "Revenue by month and country"
SELECT strftime('%Y-%m', o.placed_at) AS month, c.country,
       ROUND(SUM(t.total), 2) AS revenue
FROM orders o
JOIN customers c ON c.id = o.customer_id
JOIN order_totals t ON t.id = o.id
WHERE o.status <> 'cancelled'
GROUP BY month, c.country
ORDER BY month, c.country;
```

![Four filled bands piled on each other over six months, each a country, with a legend naming
CH, GB, NL and US; the blue CH band breaks where March has no Swiss order](images/chart-area.png)

An area draws at most 5,000 rows, fewer than a line's 50,000: a band's cost grows with the
points *and* the series. A gap breaks a band as it breaks a line, and a run of one point draws
nothing, since a band needs two points to have a width.

## Several series on one chart

A series each comes from either shape of result: one measure split by a column's values, or
several columns of numbers. Bars of several series stand side by side in a slot, or stack.

### A series per value of a column

`series` names a column whose values split one measure into a series each, so the usual
`GROUP BY month, category` result charts without a pivot in the query.

```sql
-- [chart]
-- x = "month"
-- y = "revenue"
-- series = "country"
-- title = "Revenue by month, a series per country"
SELECT strftime('%Y-%m', o.placed_at) AS month, c.country,
       ROUND(SUM(t.total), 2) AS revenue
FROM orders o
JOIN customers c ON c.id = o.customer_id
JOIN order_totals t ON t.id = o.id
WHERE o.status <> 'cancelled'
GROUP BY month, c.country
ORDER BY month, c.country;
```

![Revenue by month as a group of four bars per month, one per country, in four colours with a
legend naming CH, GB, NL and US; March has no CH bar, and the line under the chart counts the
gap](images/chart-series.png)

The series come in the order their values first appear, at most eight, one per colour; the line
under the chart names the values left out past the eighth. A month with no row for a country is
a gap in that country, not a zero: March above has no Swiss order, so it has no CH bar, and the
line under the chart says `1 gap in CH: no row, null or not a number`. Where two rows have the
same month and country, the first is charted and the line counts the others.

### Stacked bars

`stack = true` draws one bar per X with its series' values stacked on each other, for the parts
of a total: positive values up from zero, negative ones down from it. Hovering a bar shows each
part and the total.

```sql
-- [chart]
-- kind = "bars"
-- stack = true
-- x = "month"
-- y = "revenue"
-- series = "country"
-- title = "Revenue by month, stacked by country"
SELECT strftime('%Y-%m', o.placed_at) AS month, c.country,
       ROUND(SUM(t.total), 2) AS revenue
FROM orders o
JOIN customers c ON c.id = o.customer_id
JOIN order_totals t ON t.id = o.id
WHERE o.status <> 'cancelled'
GROUP BY month, c.country
ORDER BY month, c.country;
```

![Revenue by month as one bar per month, each divided into coloured bands for CH, GB, NL and
US, the whole bar reaching the month's total of up to 2,500](images/chart-stacked.png)

### Several columns of numbers

Where the measures are already columns of their own, tick them all on Y: each is a series, with
a legend once there are two. This is the shape a pivot in SQL produces, and the one to use for
measures that aren't parts of each other.

```sql
-- [chart]
-- x = "month"
-- y = ["orders", "items"]
-- title = "Orders and items by month"
SELECT strftime('%Y-%m', o.placed_at) AS month,
       COUNT(DISTINCT o.id) AS orders,
       SUM(i.quantity) AS items
FROM orders o
JOIN order_items i ON i.order_id = o.id
WHERE o.status <> 'cancelled'
GROUP BY month
ORDER BY month;
```

![Orders and items by month as two bars per month in two colours, around nine orders and
twenty-three items a month, with a legend naming orders and
items](images/chart-columns.png)

## The Chart tab

**Chart**, beside **Grid** above the rows, draws the result in the console. It charts the rows
already loaded, in the grid's order and through its filter, and runs nothing again; sort or
filter the grid, or click **Load more rows**, and the chart follows. **Grid** switches back. For
documents, **Chart** comes after **Tree** and **JSON**.

![A console with Revenue by month run and its result charted as bars under the
editor](images/chart.png)

- **Bars**, **Lines** and **Scatter** pick the kind. Text along the bottom is placed in slots in
  the grid's order whatever the kind, as bars are.
- **X** picks the column along the bottom, from any column. **Y** ticks the columns of numbers
  measured up the side. **Series** picks the column whose values split Y, and **Stacked** stacks
  the bars of several series.
- **What it starts with:** X is the first column of dates or text, and Y every other column of
  numbers, up to eight. A month written as text (`to_char(day, 'YYYY-MM')`) starts as bars, a
  date or timestamp (`date_trunc('month', day)`) as a line, and two columns of numbers as a
  scatter.
- **Which columns are numbers and dates** comes from the types the database reports. Where it
  reports none, as PostgreSQL doesn't, it comes from the values: a column is numbers if every
  value reads as one, and dates if every value is an ISO date or timestamp. A value with a
  leading zero, such as `00501`, makes its column text, since it is a code rather than a
  quantity. A result with no column of numbers says "A chart needs a column of numbers".
- **Missing values:** a row with no X value is left out, and a null or a value that isn't a
  number leaves a gap: no bar, and a break in the line. It is never drawn as zero. The line
  under the chart counts both: `Charting 12 rows · 1 gap in refunds: null or not a number`.
- **Many rows:** bars chart the first 250 rows, lines and scatters the first 50,000, and the
  line under the chart says so. To chart more, group the rows in the query.
- **Hover** over a bar or a point to see its row's X and Y values, as the grid shows them.
- Running the console again keeps the chart open, and keeps its kind, X, Y and series while the
  result has the same columns.
- **Export ›** saves the chart as it is drawn, at its size in the panel: **SVG…**, which a
  browser, a document or a pull request shows, or **PNG at 1×…**, **2×…** or **3×…**. Both are
  in the light theme's colours on white, whatever the app's theme. The save dialog suggests a
  name from the chart's columns, such as `total by month.svg`, and the toolbar then says where
  it went.

## A chart in the query

A chart written above its statement travels with the query: it opens the same on any machine,
survives an edit to the `WHERE`, and is reviewed in a pull request with the query under it. It
is a block of comment lines starting with `[chart]`, the rest of each line after the comment
marker being TOML:

```sql
-- Net sales by month.
-- [chart]
-- # Bars read best for three months.
-- kind = "bars"
-- x = "month"
-- y = "net"
SELECT strftime('%Y-%m', ordered_on) AS month, SUM(amount) AS net
FROM order_lines
GROUP BY month
ORDER BY month;
```

Running the statement opens its result on **Chart** rather than **Grid**, drawn as the block
says.

- **The block** runs from the `[chart]` line to the last comment line before the statement, and
  belongs to the statement directly below it. Prose comments go above it, and TOML's own `#`
  comments go inside it. A blank line, or a line that isn't a comment, between the block and the
  statement leaves the block belonging to no statement: the editor underlines it, and the
  statement opens on **Grid** as any other.
- **The comment marker** is the console's own: `--` for SQL and PartiQL, `//` for MongoDB, `#`
  for Redis and Elasticsearch. MySQL's `#` and CQL's `//` work too.
- **Other tools** see an ordinary comment. A formatter that keeps comments with the statement
  below them keeps the block; check one that reflows or joins comments, since a block moved away
  from its statement belongs to nothing.

## The keys

Every key is optional, and these are the same keys wherever a chart is written down: above a
statement, in a report's `[[chart]]`, and in the `chart` tool's JSON. A key left out takes the
value the **Chart** tab would start with, given the keys that are there: with only
`x = "month"`, Y is every other column of numbers, and a month of dates draws lines. An empty
`[chart]` is the chart **Chart** would show.

| Key | Takes | Left out |
| --- | --- | --- |
| `kind` | `"bars"`, `"hbars"`, `"lines"`, `"scatter"` or `"area"` | lines over dates, a scatter over numbers, bars over text |
| `x` | a column's name | the first column of dates or text |
| `y` | a column's name, or a list of them: `["net", "refunds"]` | every other column of numbers, up to eight; with `series`, the first |
| `series` | a column's name, whose values split the one Y into a series each | none |
| `stack` | `true` to stack the series' bars | `false` |
| `title` | text above the chart | none |
| `x_title`, `y_title` | text under the chart, and above it where Y's name goes | X's name; Y's name, or none before the legend |
| `y_zero` | `true` to start the Y axis at zero for lines and scatters; bars always do | `false` |
| `sort` | a column's name, then `asc` or `desc`: `"net desc"` | the order the rows come in |
| `limit` | the number of rows to chart, after `sort` | every row, up to the kind's limit |
| `y_scale` | `"log"` to run the Y axis by powers of ten, for lines and scatters | `"linear"` |
| `colours` | hex colours given to the series from the first: `["#e34948"]` | the theme's eight |
| `overlay` | lines computed from the points drawn: `["trend", "mean", "average 7", "line 100"]` | none |

- **Column names are exact,** since databases differ on case: `y = "Net"` doesn't find `net`,
  and says so, suggesting it.
- **`y_scale = "log"`** suits a measure whose range is decades — prices from cents to thousands,
  latencies from a millisecond to a minute — where an even axis crushes the small values into
  the axis. The ticks are powers of ten, with 2 and 5 inside each while they fit. A log axis has
  no place for zero or a negative value, so those become gaps, counted and named under the
  chart: `1 gap in price: zero or negative on a log scale`. It draws lines and scatters only: a
  bar's length and a band's thickness are read as ratios from the zero line, and one grown from
  a log axis's baseline would misstate exactly that.
- **`colours`** names a hue per series, from the first, so a brand's red is that red — on both
  themes and in the exported file alike. `colors` reads the same key. Hex only, `#rrggbb` or
  `#rgb`; a list shorter than the series recolours only the ones it names, and the rest keep the
  palette's own. The colour is used as given, so where one is hard to see on the current theme
  the tab says so beside the chart rather than changing it.
- **`sort` and `limit`** sort and cut the rows the chart was given, before they are charted, and
  the line under the chart says so: `Charting 10 rows · sorted by net, largest first, the first
  10`. A row whose value is null comes last. In a console, `ORDER BY` and `LIMIT` in the query do
  the same and are the better way; the keys are there so that a chart copies to a report
  unchanged.

A report's `[[chart]]` adds the keys that make a file — `file`, `width`, `height`, `scale` and
`query` — and the rows it draws can be a query of its own. See
[Charts](reports.md#charts) in Reports.

**Problems** are said where they are found. A misspelt key, a value of the wrong kind or TOML
that doesn't read is underlined in the editor, and shown in the tab in place of the chart, with
**Grid** one click away: "chart, line 4: `kind` is bars, hbars (horizontal bars), lines, scatter or area, not "pie"". A column
the result doesn't have is known once the result's columns arrive, and shown over the chart
**Chart** would start with, so the result is still seen: "chart: `y = "nett"` isn't a column of
this result: month, net".

### Between the toolbar and the block

**The toolbar writes the block.** A click on **Lines** or a new X or Y is often a look at the
data from another side, so it changes no text: it shows "Not in the query" and **Save to
query**. **Save to query** writes `kind`, `x` and `y` into the block, explicitly, so a later
change of columns can't change the chart without anyone seeing, and `series` and `stack = true`
while the chart has them (taking them out when it no longer does); a key that already says the
same is left as it is, and every other key, comment and blank line stays as it was. A statement
without a block gets one above it. The write is an edit of the console's text: ⌘Z takes it back,
and it is saved as the console saves. Running the statement again keeps a choice changed in the
toolbar while the block is unchanged; once the block is edited, the block wins.

**Copy for a report** puts the chart on the clipboard as a report's `[[chart]]` table, with a
`file` named as **Export ›** names it:

```toml
[[chart]]
file = "net by month.svg"
# Bars read best for three months.
kind = "bars"
x = "month"
y = "net"
```

Paste it into a report file, and each run writes the chart as a picture beside the report's
other files.

## Statistics: in the query, or over the points drawn

Statistics split in two, and the split decides which answer you get.

**What has to see every row belongs in the query.** A distribution and a percentile are exactly
the statistics that go wrong when they are worked out over a page: a chart draws the rows it was
given — in a console, the rows the grid holds, which may be the first page of a much larger
table — so binning there would bin a page and call it a distribution. The database sees every
row and already has the functions, so that is where those belong, and
[Summarise](#summarise-writes-the-sql) writes the SQL for you.

**What is honest over the points already drawn belongs in the chart.** A trend through the
points on screen, a moving average of them, a line at their mean or median: each is a statement
about *those points*, and the points are in front of you. Those are
[the `overlay` key](#overlays-over-the-points-drawn), and the line under the chart always says
what each one was computed over.

### Summarise writes the SQL

**Summarise ›** sits beside the chart's toolbar and in the grid's column header, on any column
of numbers, and writes a statement summarising that column:

- **Distribution into 10, 20 or 50 buckets** — each bucket's lower bound and how many rows fell
  in it, which charts as bars with nothing more to do.
- **Percentiles** — the median, p90, p95 and p99, with the smallest and largest value, a row
  each so they chart too.

It writes the statement **below the one that produced the result**, with its own `[chart]` block
above it, and leaves the original alone: the query you ran is still the query above the result,
and one undo takes the whole insertion back. Tick **Copy the SQL instead** to have it on the
clipboard and your file untouched.

The statement reads the one above it as a common table expression and asks the database for the
range, so the buckets come from every row that statement returns rather than from the page on
screen. That is the whole reason it writes SQL rather than binning what the chart has. Bucketing
is arithmetic every engine can do; percentiles are not, so where an engine has none the item is
greyed with the reason in its tooltip, rather than writing a statement that would fail when it
ran.

### Overlays over the points drawn

`overlay` takes a list of short phrases. Each is drawn once per series, in that series' colour,
dashed and thinner than the series' own line so it reads as derived rather than measured.

| Phrase | Draws |
| --- | --- |
| `"trend"` | a least-squares straight line through the series' points |
| `"average N"` | a moving average over N points, ending at each one |
| `"mean"`, `"median"` | a line across the plot at that statistic of the series' values |
| `"line V"` | a line across the plot at the value V: a target, a threshold, last year's figure |

```sql
-- [chart]
-- x = "day"
-- y = "revenue"
-- overlay = ["trend", "mean"]
-- title = "Revenue by day, with its trend and mean"
SELECT date(o.placed_at) AS day, ROUND(SUM(t.total), 2) AS revenue
FROM orders o
JOIN order_totals t ON t.id = o.id
WHERE o.status <> 'cancelled'
GROUP BY day
ORDER BY day;
```

![Revenue by day as a line swinging between nothing and 600, with two dashed lines over it in
the same blue: one flat near 230 and one rising gently from about 210 to
270](images/chart-overlay.png)

**The line under the chart names every overlay and what it was computed over,** always:
`Charting 55 rows · Trend and mean over those 55 rows, by day`, as under the picture above. Where the rows were cut, the
clause that says so reads straight into it. Over text along the bottom the fit is by position —
the slot's index — and the note says `by position`, since that is what was fitted. Gaps stay
gaps: a trend skips them and counts them, and a moving average whose window holds one gives a
gap rather than quietly averaging a shorter window.

Where an overlay would mislead, it is refused rather than drawn:

- **On an area and on stacked bars,** `trend`, `average`, `mean` and `median` are a problem: the
  series are piled on each other, so a line at one series' own values would sit where nothing
  was drawn. `"line V"` still works there, because an absolute value is read against the axis
  and so is a stack's total.
- **In a report, `overlay` together with `limit`** is a problem in the file, before the run
  starts: an overlay is computed over the rows charted, and `limit` charts only some of them, so
  an unattended file would carry a statistic over a deliberate subset with nobody there to read
  the caption. In a console the same chart is drawn, because the note is right under it and you
  set the limit a moment ago.

### A moving average in the query

`overlay = ["average 7"]` averages the points the chart drew. A window function averages every
row the query returned, which is the one to reach for when the grid holds a page of a larger
result — and both the measure and its average then chart as lines:

```sql
-- [chart]
-- x = "day"
-- y = ["revenue", "moving_average"]
-- title = "Revenue by day, and its moving average"
WITH daily AS (
    SELECT date(o.placed_at) AS day, ROUND(SUM(t.total), 2) AS revenue
    FROM orders o
    JOIN order_totals t ON t.id = o.id
    WHERE o.status <> 'cancelled'
    GROUP BY day
)
SELECT day, revenue,
       ROUND(AVG(revenue) OVER (ORDER BY day ROWS BETWEEN 6 PRECEDING AND CURRENT ROW), 2)
           AS moving_average
FROM daily
ORDER BY day;
```

![Revenue by day as a line swinging between nothing and 600, with a second, much flatter line
over it holding near 200 to 300, and a legend naming revenue and
moving_average](images/chart-moving-average.png)

`ROWS BETWEEN 6 PRECEDING AND CURRENT ROW` is the window: each point is the average of its own
row and the six before it. Widen or narrow it there, and the chart follows. The same shape gives
a running total (`SUM(…) OVER (ORDER BY day)`), a share of the whole, or a change on the row
before (`revenue - LAG(revenue) OVER (ORDER BY day)`).

### Counts in bands, by hand

[Summarise](#summarise-writes-the-sql) writes this statement for you, and it is worth reading
once to see what it writes. A histogram is a count per band, and a count per band is a
`GROUP BY`. Band the measure in the query, count the rows in each band, and chart it as bars:

```sql
-- [chart]
-- kind = "bars"
-- x = "band"
-- y = "orders"
-- title = "Orders by value, in bands of 100"
SELECT CAST(total / 100 AS INTEGER) * 100 AS band, COUNT(*) AS orders
FROM order_totals
GROUP BY band
ORDER BY band;
```

![Orders by value as five bars labelled 0, 100, 200, 300 and 500, the first twice the height of
the rest](images/chart-buckets.png)

`band` holds numbers, so this chart says `kind = "bars"`: left to itself, a number along the
bottom would draw a scatter.

Bars give a slot to each row the result has, and to no other, so a band that no order fell in
has no bar at all rather than an empty one — above, there is no bar at 400, and 500 sits next
to 300. Where the empty bands matter, produce them in the query, from a series of bands joined
to the counts.

Every database writes the banding its own way — `width_bucket(total, 0, 1000, 10)` in
PostgreSQL and DuckDB, `FLOOR(total / 100) * 100` elsewhere — and `date_trunc` or `strftime`
band a time the same way. Percentiles come from the query too, through `percentile_cont` or
`NTILE`, and chart as bars or a line like any other result. A chart's own `query` in a report
takes exactly this kind of statement, which is how a distribution gets into a report: write
there what **Summarise** would have written.

## What charts don't do

A chart in DbRover is five kinds, fourteen keys and a picture of one result. The limits are
chosen, not missing by accident, and each is weighed again when someone asks for it.

- **The kinds are bars, horizontal bars, lines, a scatter and a stacked area.** There is no pie:
  slices are hard to compare, and bars say the same thing better. There is no heatmap, which
  would need two category axes, a colour scale and a legend of its own. There is no histogram as
  a kind, because binning belongs where every row is — see
  [Summarise](#summarise-writes-the-sql).
- **No distributions or percentiles in the chart layer,** as above: those see a page, so they
  are written as SQL instead. What the chart layer computes is what is honest over the points it
  drew, and it says so under every picture.
- **No annotations:** no arrow or label placed at a point by hand. `overlay` draws lines
  computed from the data or set at a value, not prose.
- **A log scale on Y only,** through `y_scale`, and on lines and scatters only. X is already
  placed three ways — by category, by number and by time — and a fourth would double that work
  for a scatter of two numbers.
- **Colours per series, not a palette by name.** `colours` names the hues you want; there is no
  `palette = "viridis"`, which would be a word DbRover owns forever and still wouldn't hold
  anyone's three particular hues. Exported files are always the light theme's colours on white,
  with the ones you named used as given.
- **No pan or zoom.** Narrowing is the grid's filter, or the query.
- **No dashboards:** no page, file or tab of several charts, and no chart that refreshes on a
  timer or a live connection. A report writes as many charts as you like, each its own file, and
  a chart's SVG is the same byte for byte from the same rows, so one can be kept in git and
  compared with last month's.
- **No interactive or HTML pictures in files,** and no charting runtime in the build. SVG and
  PNG are what a chart writes.
