# Your own export formats

`exports.toml` adds formats of your own to the export dialog and to **Copy as ›**. There are
three kinds:

- a **preset**: a built-in format with options of its own, such as CSV with semicolons;
- a **template**: a header, a line (or more) per row and a footer, written in Jinja;
- a **program**: anything that reads rows as JSON Lines and writes a file, such as one that makes
  a PDF.

The file sits next to `keymap.toml` and `actions.toml` (see
[Where DbRover keeps its files](files.md)), and doesn't exist until you write it.
**Settings › Exports** (**View → Exports…** opens it) lists every format the file adds.
**Create exports.toml** on that page writes a file of commented examples, and **Reload** reads
it again after you edit it. Reload reads `actions.toml`, `snippets.toml` and `theme.toml` again
too.

A mistake costs only the format it is in: one that can't be used is skipped, and the status bar
shows **exports.toml: N problems**. Click it to see each one, with the format's id and, for a
template, the part and line.

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

Every format has:

- an **`id`**: lowercase letters, digits, `-` and `_`;
- a **`name`**, shown on its tile and in the menu;
- optionally a **`description`**, shown when you hover over it;
- an **`extension`** for its files, without the dot. A preset uses its format's unless you give
  one.

What makes an entry a preset, a template or a program is the key only that kind has: `format`,
`row` or `command`.

## Where they appear

- **The export dialog** shows your formats as tiles after the built-in ones, for **Export…**,
  **Export data…** and **Run to file…** alike.
  - Choosing one shows what it writes, rather than options to change: those are changed in the
    file.
  - The dialog opens on the one you used last, as it does for the built-in formats.
  - A format that can't write these rows is greyed out, as a built-in one is.
- **Copy as ›**, in a cell's right-click menu and on macOS in the Edit menu, lists your formats
  that write text after a line. That is every template, a preset of a format that writes text,
  and a program with `text = true`.
- **Keys:** bind copying as one in `keymap.toml` as `"export.<id>" = "cmd+alt+c"`. The keymap
  page lists them under **Copy as your own formats**.

MCP's `query` tool keeps the built-in formats.

## Presets

`format` is the built-in format's id, and its options sit beside it:

| `format` | Options |
| --- | --- |
| `csv` | `separator`, `quoting` (`"where_needed"` or `"always"`), `header`, `null_text`, `line_ending` (`"crlf"` or `"lf"`), `bom` |
| `xlsx` | `header`, `sheet_name`, `freeze_header`, `autofilter` |
| `tsv` | `header`, `null_text` |
| `markdown` | `null_text` |
| `sql-insert` | `rows_per_statement` (1 to 1000) |
| `parquet` | `compression` (`"zstd"` or `"snappy"`) |
| `html` | `document` (`"page"` or `"fragment"`), `null_text` |
| `json`, `jsonl`, `sql-update` | none |

An option you leave out takes its default. One the format doesn't have is a problem that names
the ones it does, so a typo such as `seperator` never quietly writes commas.

## Templates

A template writes its `header` once, its `row` once for each row as the rows arrive, and its
`footer` once. Any number of rows streams through it, as through every other format. `row` is
required; `header` and `footer` are empty unless given. Long templates fit in TOML's multi-line
strings, between `'''`.

Templates are written in [MiniJinja](https://docs.rs/minijinja)'s Jinja: `{{ … }}` writes a
value, `{% for … %}` and `{% if … %}` do what they say, and Jinja's filters (`upper`, `replace`,
`join`, `length`, …) are there.

| Name | In | What it is |
| --- | --- | --- |
| `columns` | every part | Each column, with `name`, `type` (as the database names it, or none) and `kind`: `integer`, `decimal`, `float`, `boolean`, `date`, `timestamp`, `timestamp_with_zone`, `text` or `undeclared` |
| `data_source` | every part | The data source's name |
| `table` | every part | The table the rows came from, as `main.orders`, when they came from one |
| `parameters` | every part | The values of the query's `:name` parameters, by name: `{{ parameters.since }}` |
| `exported_at` | every part | When the export started, as `2026-11-01T12:00:00Z` |
| `row` | `row` | The row's values in column order: `{{ row[0] }}` |
| `record` | `row` | The row's values by column name: `{{ record.email }}`. A repeated name gets `_2`, `_3`, as in JSON |
| `index` | `row` | The row's number, counting from 1 |
| `count` | `footer` | How many rows were written |

- **Values are text, and `NULL` is `none`, written as nothing.** To write something else for
  it, in `{% for value in row %}`: `{{ value if value is not none else "NULL" }}`.
- **Each part keeps its last line break**, so a `row` ending in `\n` writes a line a row. Jinja
  itself drops it.
- **Values are written as they are.** With `escape = "html"`, every value is HTML-escaped (for
  HTML and XML) unless you mark it `{{ value|safe }}`.
- **Filters for exports:**
  - `sql_literal`: a string literal quoted for the data source's database, or `NULL`;
  - `sql_name`: a name quoted for it;
  - `csv`: a field quoted as CSV needs it, with `csv(";")` for another separator;
  - `json`: a JSON string, or `null`.

  The SQL filters stop the export on rows from a database without SQL.
- A name that doesn't exist, such as `{{ recrod.id }}`, is an error rather than an empty
  string. The dialog's preview shows it before anything is written.
- Jinja writes `true` and `false` as `True` and `False`, as Python does.

A template can't read files: `include`, `extends` and `import` don't exist here. A loop that never
ends, or a part that writes more than 16 MB for one row, stops the export with a problem naming
the row, rather than hanging DbRover or filling the disk. The limit is a million steps of the
template's code for each row, far more than any table needs.

An XML file, every value escaped:

```toml
[[export]]
id = "xml"
name = "XML"
extension = "xml"
escape = "html"
header = "<rows table=\"{{ table }}\">\n"
row = '''
  <row>{% for column in columns %}<{{ column.name|safe }}>{{ row[loop.index0] }}</{{ column.name|safe }}>{% endfor %}</row>
'''
footer = "</rows>\n"
```

## Programs

A program gets the rows as JSON Lines on stdin, exactly as the **JSON Lines** format writes
them: one object a line, keyed by column name, values as text or `null`. What it writes on stdout
becomes the file, as it writes it, so a program can export any number of rows.

- **`command`** is the program, then its arguments, as a list, with no shell, as for
  [command actions](customising.md#commands). Arguments can name `{table}`, `{schema}`,
  `{database}`, `{data_source}`, `{host}`, `{port}`, `{user}` and `{file}`. `{table}` and
  `{schema}` are filled only for rows from one table.
- It gets `DBROVER_DATA_SOURCE`, `DBROVER_DRIVER`, `DBROVER_DATABASE`, `DBROVER_SCHEMA` and
  `DBROVER_TABLE` where there is one, `DBROVER_FORMAT=jsonl`, and `DBROVER_EXPORT`, its id. As
  with command actions, it never gets a password.
- **`timeout`** is how long it may go without taking more rows, and how long it has to finish
  once the rows have ended: a minute unless you give one, as `"30s"` or `"5m"`. A program that
  keeps reading is never cut off, however many rows there are. **Cancel export** stops it at any
  time.
- **`text = true`** says it writes text, and puts it under **Copy as ›**. Copying runs it on the
  selected cells and puts what it wrote on the clipboard, as a command action's output: up to
  4 MB, within its timeout, with **Stop** in the status bar.

Before a program runs for the first time, DbRover shows its command in full and asks
**Run a program?**. **Run and export…** in the export dialog, or **Run** for **Copy as**, runs it,
and from then on it runs without asking. Change its command and it asks again. What you trusted
is kept in `trusted_commands.toml`, as for command actions (see
[The first run](customising.md#the-first-run)).

A program that ends with an error leaves no file, not even half of one: DbRover says how it
ended and what it wrote to stderr. On a Mac, write the program's full path, since a Mac app
doesn't see your shell's `PATH` (see
[On a Mac and on Windows](customising.md#on-a-mac-and-on-windows)).

A program that turns the rows into YAML, for **Copy as**:

```toml
[[export]]
id = "yaml"
name = "YAML"
extension = "yaml"
command = ["/opt/homebrew/bin/yq", "-P", "-p", "json"]
text = true
```

## Where it stops

Templates work on one row at a time: there is no grouping, no totals, no sorting and no charts.
Group and total in the query (`GROUP BY`, `ROLLUP`, window functions), and leave anything
larger to a program. Formats only ever come from your own `exports.toml`: a data source export
carries none.
