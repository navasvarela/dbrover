# ER diagrams

Status: **ER1 designed** on 2026-11-01; the [roadmap](roadmap.md#diagrams-track) tracks it.

A diagram of the tables you pick, or of a whole schema, with their columns and the foreign keys
between them. It shows the schema and changes nothing in it: editing from the diagram is out of
scope, as the roadmap says. Every other tool in [the comparison](comparison.md#er-diagrams) has
one; this is the read-only half, with export.

## What exists today

- **Every SQL driver describes a table** (`Connection::describe_table`): its columns with their
  types, nullability and defaults, its primary key, its own foreign keys (`foreign_keys`) and the
  keys of other tables that point at it (`referenced_by`), W5's work for editing rows. The engine
  answers one table at a time (`Command::DescribeTable`), on the explorer's connection.
- **The explorer** selects one node at a time and opens dialogs and tabs from its context menu.
  Schemas are nodes of their own on every SQL engine (SQLite's is `main`); ClickHouse is the only
  one whose dialect has no foreign keys (`Capabilities::foreign_keys`).
- **Tabs** are egui_dock tabs, saved with the layout in `app.ron`; a console's tab is restored at
  start-up from its file, which the engine owns and writes atomically.
- **Projects** have slugs (`analytics`, `shop-eu`): lower-case letters, digits and single
  hyphens. Most data sources are in no project.
- **resvg 0.45** is in the build already, through `egui_extras`' `svg` feature, which draws the
  database logos. It is built **without its `text` feature**, so it would draw an SVG's boxes
  and lines but none of its words.
- **egui 0.36's `Scene`** pans and zooms whatever is drawn inside it.

## Opening a diagram

- **On a schema: Diagram…** opens a dialog listing the schema's tables (the explorer's tables
  folder, loaded first if it wasn't), all ticked, with **All** and **None**, a filter, and
  **Add the tables these reference**, which grows the choice by one step of foreign keys once
  the chosen tables are described. The diagram's name starts as the schema's. **Open** opens
  the diagram's tab.
- **On a table: Diagram with related tables** opens a diagram at once, of the table and every
  table its keys join, both ways (its `foreign_keys` and its `referenced_by`), named after the
  table.
- **On a diagram's table, in the tab: Add related tables** grows the diagram the same way, and
  **Remove from diagram** drops the table from it.
- The explorer selects one node at a time, so choosing tables happens in this dialog, not by
  multi-selecting in the tree.
- **Not offered** where the dialect has no foreign keys (ClickHouse) or no SQL (Redis, MongoDB,
  Elasticsearch): a diagram there would be boxes without a line between them.

## What loads

- **One command describes a list of tables:** `Command::DescribeTables`, answered by one
  `Event::TablesDescribed` with each table's description, `None` for a table that isn't there,
  or the error describing it. It runs on the explorer's connection, so a hundred tables never
  open a hundred connections.
- **A few at a time:** the tab asks for eight tables, and for the next eight when they arrive.
  The explorer's connection answers in order, so expanding a node while a big diagram loads
  waits for one batch, not for the whole diagram. The tab shows how many tables are loaded and
  draws each batch as it comes.
- **Only tables:** a view, or a table that is gone, describes as `None`. Related tables are
  found in the descriptions, by schema and name, so a key to a table in another schema adds that
  table too.

## Layout

Automatic and layered, by Sugiyama's method, in a new crate, `dbrover-diagram`, as a pure
function from boxes (each table's size) and keys to where each box sits. It needs no window and
is tested on its own.

1. **Edges:** one per pair of tables a key joins, from the referencing table to the referenced
   one. Several keys between the same two tables are one edge for the layout.
2. **Cycles broken** at a self-reference, which is left out, or else at the last key found: a
   depth-first walk over the tables in name order reverses the edge that closes a cycle.
3. **Layers by longest path:** a table that references nothing is in the first layer, and every
   other table one layer after the furthest table it references. Layers are columns, left to
   right, so `customers` sits left of `orders`, and `orders` left of `order_items`. An edge that
   spans several layers gets a dummy node in each layer between, so the ordering sees it.
4. **Crossings reduced** by sweeping the layers in barycentre order: each layer is sorted by the
   mean position of its neighbours in the layer before, left to right, then right to left, a
   fixed number of times, keeping the order with the fewest crossings.
5. **Coordinates:** each column is as wide as its widest box, boxes stack down a column with a
   gap, and columns are centred on the tallest. **Tables no key joins** go in columns of their
   own after the rest, filled to the tallest column's height, rather than lengthening the first.

The tab measures each box from its text, then calls the layout. **Arrange** calls it again,
throwing away where the user moved things.

## The view

- **A dock tab** beside the consoles, titled with the diagram's name, using `Scene` for pan and
  zoom: drag the background or scroll to pan, pinch or ⌘-scroll to zoom. **Fit** shows the whole
  diagram.
- **Tables are boxes**: a header with the table's name (and schema, when it isn't the one the
  diagram was opened on), then a row per column with its type, the primary key's columns marked
  with a key and the foreign-key columns with a link, coloured like the explorer's icons for
  tables, keys and columns.
- **Each key is a line** from its first column's row, on the side facing the referenced table,
  to the referenced table's matching row, as a curve with a mark at the referenced end. Lines
  are drawn under the boxes. A self-reference loops back to the same table's side.
- **Drag a table to move it**; lines follow. Hovering a table highlights its lines.
- **Toolbar:** **Arrange**, **Fit**, **Export ▾**, and how loading is going ("Loading 24 of 80
  tables…") or what failed.
- **A table that is gone** (described as `None` when the diagram reopens) is drawn as a dashed
  box with its name and "missing", where it was left, and stays in the file until it is removed
  from the diagram. A table that couldn't be described shows the error in its box.

## Saving

- **Per project, in the settings folder:** `diagrams/<project-slug>/<name>.toml`, or
  `diagrams/_none/<name>.toml` for a data source in no project (see
  [decisions](#decisions)). The engine owns the folder, as it owns `consoles/`: it reads every
  diagram at start-up and writes one atomically when asked.
- **What the file holds:** the data source's id, and each table with where its box sits. Columns
  and keys are not saved: they are described again when the diagram opens, so a diagram never
  shows a schema older than the database's.

  ```toml
  # A DbRover ER diagram. Each table is described again when the diagram opens.
  data_source = "6f1c0d9e-3b8a-4c3e-9f57-2d1e8a7b5c40"

  [[table]]
  schema = "public"
  name = "customers"
  x = 0.0
  y = 0.0

  [[table]]
  schema = "public"
  name = "orders"
  x = 260.0
  y = 0.0
  ```

- **When:** a second after the last change (a table moved, added or removed, Arrange), as a
  console is saved a second after typing stops, and for every unsaved diagram on quit.
- **Names** are file names: a new diagram takes its schema's or table's name, numbered
  (`public_2`) if the folder already has one by that name.
- **Reopening:** the data source's context menu lists its saved diagrams under **Diagrams**, and
  a diagram's tab open at quit opens again at start-up, as a console's does. Closing the tab keeps
  the file; **Delete diagram…** in the tab removes it.
- **Moving a data source** to another project leaves its diagrams where they are: they are
  found by the data source they name, wherever they are under `diagrams/`. New diagrams go in
  its new project's folder.
- **Deleting a data source** leaves its diagram files, as it leaves its consoles.

## Export

- **SVG…** writes the diagram as SVG from the same boxes and lines the tab draws: rectangles,
  text and cubic curves, in the light theme's colours on white, so it reads in a document or a
  pull request whatever the app's theme. Fonts are named (`Inter`, then the platform's sans-serif;
  `JetBrains Mono` for types), not embedded.
- **PNG…** at **1×, 2× or 3×** is drawn from that SVG with resvg and encoded with tiny-skia, both
  already in the build. To draw the words, resvg's **`text` feature is turned on**, with fonts
  loaded from the app's own bundled Inter and JetBrains Mono, never the system's, so a PNG looks
  the same on every machine. That adds eight small pure-Rust crates (`fontdb`, `ttf-parser`,
  `rustybuzz` and its Unicode tables), no C and no new CI job.
- **Where:** the export runs in the engine, off the UI thread, since a big diagram at 3× takes a
  moment, and writes beside the final name before renaming, as other exports do. A PNG larger
  than 64 megapixels (256 MB in memory) is refused with a suggestion to pick a smaller scale or
  SVG.

## Decisions

| Question | Decision | Why |
| --- | --- | --- |
| Where a data source outside any project keeps its diagrams (open question) | **`diagrams/_none/`** | Most data sources are in no project, which exists for MCP; holding diagrams back until one joins would hide the feature behind an unrelated setting. `_none` can't be a project's slug, which has no underscores, so it never clashes. |
| How big a diagram may get before the dialog asks to narrow it (open question) | **150 tables**, then the dialog asks before opening, and offers to go on | Past that a diagram is unreadable at any zoom that shows it whole. Loading is what grows with it, not the layout: laying out 150 tables takes about 1 ms and 500 about 4 ms (release build, Apple M-series), but each table is about four catalog queries on PostgreSQL. 150 took 0.28 s on the local test server; at 20 ms a round trip that is about 12 s. Drawing isn't either: with 150 tables of six or seven columns on screen at once, a whole frame of the app takes about 1.2 ms (release; egui's work on the CPU, measured in the UI test harness). |
| Layer direction | **Columns, left to right**, referenced tables first | A table is tall (a row per column), so side by side wastes less space than stacked layers; reads as `customers ← orders ← order_items`. |
| Where layout lives | **`dbrover-diagram`**, a leaf crate on `dbrover-core` | Pure and tested without a window; the app crate has no library target, so its tests are slower to run. |
| What a diagram file holds | **The data source and each table's place**, never columns or keys | Described again on opening, so it never shows an old schema; the file stays small and diffs well in Git. |
| Whether moving a data source moves its diagrams | **No**; diagrams are found by the data source they name | Nothing is lost or left behind, and no rename can fail half-way. |
| The export's colours | **The light theme's, on white**, whatever the app's theme | Diagrams go into documents and pull requests, which are mostly light. |
| Text in the PNG | **resvg's `text` feature**, with the bundled fonts | Without it a PNG has boxes and no words. Turning text into paths ourselves would mean shaping text by hand; resvg already shapes it. |

## Layers

Each layer is a pull request of one stack, and each works on its own:

1. **ER1a — The plan:** this design, and the roadmap's entry.
2. **ER1b — Describing many tables, and the layout:** `Command::DescribeTables` with engine
   tests on SQLite and the PostgreSQL test server; the `dbrover-diagram` crate with the layered
   layout and its unit tests (cycles, self-references, long edges, unconnected tables, crossings
   reduced on known graphs).
3. **ER1c — The diagram tab:** **Diagram…** on a schema and its dialog, **Diagram with related
   tables** on a table, loading a few at a time, the `Scene` view with boxes, lines, dragging,
   **Arrange** and **Fit**; kittest UI tests on the SQLite fixture.
4. **ER1d — Saving and reopening:** the `diagrams/` folder in `dbrover-store`, the engine's
   commands to read and write it, the **Diagrams** menu, tabs restored at start-up, missing
   tables, **Add related tables**, **Remove from diagram** and **Delete diagram…**.
5. **ER1e — Export:** SVG from the boxes and lines, PNG at 1×, 2× and 3× through resvg with
   text, written by the engine.
6. **ER1f — The manual:** a chapter on diagrams with a generated screenshot, this roadmap's
   demo note, and the entry marked built. The site's clip is left for a later change.

## Risks

- **A wide schema loads slowly** on a distant server: one describe per table, each a few
  catalog queries. Batches keep the explorer usable meanwhile, the tab draws tables as they
  arrive, and the size limit asks before a very large one.
- **A dense schema lays out poorly**: barycentre sweeps reduce crossings but don't remove them,
  and a table referenced by everything pulls lines across the diagram. Dragging and saving
  positions is the answer; better routing is a later change.
- **Fonts differ in a browser**: the SVG names Inter, which a browser may not have, so text may
  be a little wider or narrower than its box. Boxes leave room at the right for it.
- **Huge PNGs**: refused past 64 megapixels rather than running out of memory.
