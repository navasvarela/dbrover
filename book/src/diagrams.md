# ER diagrams

A diagram draws the tables you pick, with their columns and the foreign keys between them, so a
schema reads at a glance. It only shows the schema: nothing in a diagram changes the database.

![The shop's tables as a diagram: customers and products first, orders pointing at them, order
items pointing at orders and products, and events, which no key joins, on its own](images/er-diagram.png)

Diagrams need foreign keys to draw, so they are offered on PostgreSQL, MySQL, MariaDB, SQLite,
SQL Server and DuckDB, and not on ClickHouse or the engines without SQL.

## Opening a diagram

- **A schema's tables:** right-click a schema in the explorer and choose **Diagram…**. Every
  table in the schema is ticked; untick the ones you don't want, or use **None** and tick the
  few you do. The filter narrows the list as you type, and **All** and **None** act on what it
  shows. Tick **Add the tables these reference** to add, once your tables are loaded, every
  table their foreign keys point at. Give the diagram a name (the schema's, to begin with) and
  choose **Open**.
- **A table and its neighbours:** right-click a table and choose **Diagram with related
  tables**. The diagram shows the table, the tables its foreign keys point at, and the tables
  whose foreign keys point at it.

More than 150 tables make a diagram that is hard to read and slow to load from a distant
server, so the dialog says so and its button becomes **Open anyway**.

The tables load a few at a time on the explorer's connection, and appear as they come: the
toolbar says "Loading 24 of 80 tables…" meanwhile. The explorer stays usable while they load.

## Reading a diagram

- Each table is a box: its name, then a row for each column with its type. A key marks the
  primary key's columns, and a link the columns of a foreign key.
- Each foreign key is a line from its column to the column it refers to, with an arrow at the
  referenced table. A line that would pass behind another table bends around it. A table's
  key to itself loops out of its right side.
- Tables from another schema than the diagram's show their schema: `audit.log`.
- Hover over a table to light up its lines.
- A table dropped from the database since the diagram was made is a dashed box marked
  **missing**. A table that couldn't be read shows the error; hover over it for all of it.

The layout puts the tables other tables point at on the left, and the tables pointing at them
in columns to their right; tables no key joins sit in columns of their own after the rest.

## Moving around

- **Pan:** drag the background, or scroll.
- **Zoom:** pinch, or scroll with ⌘ held (Ctrl on Linux and Windows).
- **Fit** shows the whole diagram.
- **Drag a table** to move it. The other tables stay where they are from then on.
- **Arrange** lays every table out again, forgetting where you moved them.
- Right-click a table for **Add related tables**, which adds every table its keys join, and
  **Remove from diagram**.

## Diagrams are saved

A diagram is saved a second after you last change it, and when you close its tab or quit. To
open it again, right-click its data source and choose it under **Diagrams**; a diagram whose
tab was open when you quit opens again by itself.

Each diagram is a small TOML file in the settings folder (see
[Where DbRover keeps its files](files.md)):

- `diagrams/<project>/<name>.toml` for a data source in a project, where `<project>` is the
  project's short name (its slug, as MCP clients see it), or `diagrams/_none/<name>.toml` for a
  data source in no project. A project's folder can be kept in Git with the rest of its work.
- It names the data source, the schema the diagram was opened on, and each table with where its
  box sits. Columns and keys are not in it: they are read from the database each time the
  diagram opens, so a diagram never shows a schema older than the database's.
- Moving a data source to another project leaves its diagrams in their folder; they are still
  listed under its **Diagrams**.

**Delete diagram…** in the toolbar removes the diagram's file, after asking. It never touches
the database.

## Exporting a picture

**Export** in the toolbar writes the whole diagram to a file:

- **SVG…** keeps every line sharp at any size, and opens in a browser or a document. It names
  the Inter and JetBrains Mono fonts, and falls back to the system's where they aren't
  installed.
- **PNG at 1×…**, **2×…** or **3×…** draws it at that many pixels a point; 2× suits most
  screens and documents. A PNG uses the fonts that come with DbRover, so it looks the same on
  every machine. A diagram too big for a PNG (past 64 megapixels) is refused: choose a smaller
  scale, or SVG.

Pictures use the light theme's colours on white, whatever theme DbRover is in, so they read in
documents and pull requests. The toolbar says where the picture was written.
