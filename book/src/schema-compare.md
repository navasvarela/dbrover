# Comparing schemas

Two schemas that should be alike drift apart: a staging schema gains a column the production
migration forgot, an index is made by hand on one server and never written down. Schema
compare shows two schemas side by side with what differs between them, and writes the script
that turns one into the other, for you to read and run. A schema can also be saved as a
**snapshot**, a plain text file to keep in Git and compare with later.

Schema compare works on PostgreSQL. It is not offered on Redshift, whose catalog it can't read
yet, nor on the other engines.

## Comparing two schemas

1. Right-click a schema in the explorer and choose **Compare schema…**. A **Schema compare**
   tab opens with that schema on its left.
2. Pick the right side: a data source of the same engine, which may be the same one, and the
   name of a schema in its database. A side can also be a [snapshot file](#schema-snapshots).
3. Choose **Compare**. Both schemas are read on their data sources' explorer connections, and
   the explorer stays usable meanwhile.

**Swap sides** turns the comparison round. Once the schemas are compared, **Compare again**
reads both again, after you change either.

A side that can't be read says why under the sides, and the other side is still read.

## Reading the differences

The list on the left groups what differs as **Tables**, **Indexes** and **Views**, with a
mark in front of each:

| Mark | Means |
| --- | --- |
| **+** | Only on the right: the script adds it. |
| **−** | Only on the left: the script drops it. |
| **~** | On both sides, and different. |

Under a changed table, a line says what changed ("1 column added, 1 column changed"), then a
line for each column and constraint that differs: `~ total: type: numeric(10,2) →
numeric(12,2)`, `+ column note`, `− check orders_total_check`. The boxes above the list hide a
kind. When nothing differs, the tab says **The schemas are the same.**

Select an object to see its definition from each side, side by side, with the lines that
differ marked. An object on one side only shows an empty side opposite it.

### What is compared

| Object | Compared | Matched by |
| --- | --- | --- |
| Table | Its columns, primary key, unique keys, foreign keys and checks | Name |
| Column | Type, whether it takes `NULL`, default, identity | Name, within its table |
| Primary key | Its columns, in order, and its name | One per table |
| Unique key, foreign key, check | Its columns; a foreign key's table, columns and actions; a check's condition | Name, within its table |
| Index | Its table, whether it is unique, its method, its key (columns or expressions, with their order and operator class), its included columns, and a partial index's condition | Name |
| View, materialized view | Its kind and its query | Name |

**Never compared,** because copies of a schema differ in them without the schema being
different: owners, grants, comments, tablespaces, storage settings and the order of a table's
columns. An index that holds up a primary or unique key is part of that key, not an index of
its own.

**Not compared yet:** sequences no column owns, functions and procedures, triggers, types
(enums, domains), extensions and what they create, partitions, exclusion constraints, row-level
security policies and publications. A column of an enum type is compared by its type's name.

### Names

Objects are matched by name, so a renamed column reads as one column dropped and another
added, and a renamed index as one index dropped and another created. Renames are never
guessed.

Each schema is read with the search path set to it alone, so the same object in two schemas
reads the same: a view over `staging.orders` and one over `public.orders` both read
`FROM orders`. An object in a third schema keeps its schema's name: `audit.events`.

A column whose default takes the next value of a sequence the column owns reads as
`serial` (or `smallserial`, `bigserial`), as it was most likely written.

## The script

The script turns the **left** schema into the **right** one: run on the left, it makes it match.
**Open script in console** puts it in a new console on the left's data source, and **Copy
script** copies it. DbRover never runs it: it runs when you choose **Run** in the console, with
every check a console has, such as a red data source's confirmation.

```sql
-- Changes schema staging on shop-dev (left) to match schema public on shop (right).
-- Written by DbRover's schema compare. Read it before running it: nothing has run.
-- Columns are matched by name, so a renamed column is dropped and added again: edit
-- the script to rename it instead, or its data is lost.

BEGIN;
-- The definitions below name the schema's own objects without the schema.
SET LOCAL search_path TO staging;

-- Drops column orders.legacy_code and the values in it.
ALTER TABLE staging.orders DROP COLUMN legacy_code;
ALTER TABLE staging.orders ADD COLUMN note text;

CREATE INDEX orders_customer_id_idx ON staging.orders USING btree (customer_id);

COMMIT;
```

- **One transaction:** if a statement fails, nothing has changed; roll back in the console.
- **The search path** is set to the left's schema for the script, because the definitions it
  copies name that schema's own objects without it.
- **In an order that works:** views that go, change or read a table that changes are dropped
  first and created again last; foreign keys are dropped before tables and added after them;
  indexes come last. Tables are changed with the same statements as **Modify table…**.
- **What it loses** is said in a comment above the statement: a dropped table's rows, a
  dropped column's values, a materialized view's stored rows.
- **What it can't write** is a comment saying so, starting `-- Not written:`, and the rest is
  still written: a column that becomes or stops being an identity column, or that changes to
  or from a `serial` type. The toolbar counts them.

Once the script has run, **Compare again** should find the schemas the same.

## Schema snapshots

A snapshot is a schema's tables, indexes and views written to a TOML file. Taken twice of an
unchanged schema it is the same file, byte for byte: nothing in it changes by itself (no date,
no server version, no host). Commit one to Git after each release or migration, and the file's
history is the schema's history; `git diff` shows an added column as one added line.

To save one, right-click a schema in the explorer and choose **Save schema snapshot…**, or use
the save button beside a live side in a compare tab ("Save a snapshot of the left side"). The save dialog suggests
`<schema>.schema.toml`. The status bar says where it was saved, or why it wasn't.

```toml
[snapshot]
format = 1
engine = "postgresql"
schema = "public"

[tables.orders]
columns = [
  { name = "id", type = "bigserial", nullable = false },
  { name = "customer_id", type = "integer", nullable = false },
  { name = "total", type = "numeric(12,2)", nullable = false },
]
primary_key = { name = "orders_pkey", columns = ["id"] }
foreign_keys = [
  { name = "orders_customer_id_fkey", columns = ["customer_id"], references = "customers", referenced_columns = ["id"], on_delete = "cascade" },
]

[indexes.orders_customer_id_idx]
table = "orders"
method = "btree"
key = ["customer_id"]
```

The file is regenerated whole each time: edits made by hand are lost the next time it is
saved, and a file DbRover can't read as a snapshot is refused with the reason.

**To compare with a snapshot,** pick **Snapshot file…** in a side's data source menu instead of
a data source. A snapshot of `public` may be compared with any schema of the same engine. When
the left side is a snapshot, **Open script in console** asks which data source to open the
script on.

## From the command line

Both work without opening a window, for a CI job. They connect to a data source as the app
does: with its saved password or password command, and through its SSH tunnel.

```text
dbrover schema snapshot <data source> <schema> [-o <file>] [--config-dir <folder>]
dbrover schema compare <left> <right> [--script] [--config-dir <folder>]
```

- **`snapshot`** writes the schema's snapshot to the file `-o` names (written beside it first,
  so a failure leaves an earlier file whole), or to standard output.
- **`compare`** compares two sides and prints what differs, as the tab's list says it, or with
  `--script` the script. Each side is `<data source>:<schema>`, split at the last colon (so a
  data source's name may hold one), or a snapshot file: a side ending in `.toml`, or naming a
  file that exists. Comparing two snapshot files needs no data source.
- **`--config-dir <folder>`** uses that settings folder's data sources, as for the app.

They exit as `diff` does, so a CI step can fail when a server has drifted from the snapshot in
the repository:

| Code | `snapshot` | `compare` |
| --- | --- | --- |
| 0 | written | the schemas are the same |
| 1 | | the schemas differ |
| 2 | not written: the reason is on standard error | something went wrong: the reason is on standard error |

```sh
# After a migration, keep the schema's snapshot in the repository:
dbrover schema snapshot production public -o schema/public.schema.toml

# In CI, fail when production has drifted from it:
dbrover schema compare schema/public.schema.toml production:public
```
