# Comparing schemas

Status: **designed on 2026-11-05, being built.** The [roadmap](roadmap.md) tracks it as
W13 in the [well-rounded track](roadmap.md#well-rounded-track) and the Version control track's
[VC3](roadmap.md#version-control-track), built together in one stack. It starts with
PostgreSQL. Each layer's decisions are confirmed as it is built, and departures go in the demo
notes.

Two schemas that should be alike drift apart: a staging schema gains a column the migration for
production forgot, an index is made by hand on one server and never written down, a view is
edited in place. Today finding out means reading two explorers side by side, or running
`pg_dump --schema-only` twice and diffing the output, which differs in a hundred lines that
don't matter (owners, comments, `SET` lines, the order objects were created in).

This plan adds two things, built on one model of a schema:

- **Schema compare (W13):** two schemas side by side, on one data source or two of the same
  engine, listing what differs (tables, columns, keys, indexes, views), and the script that turns
  one into the other, opened in a console to read and run by hand. Nothing is ever run for you.
- **Schema snapshots (VC3):** a schema written to a deterministic TOML file, from the app and as
  `dbrover schema snapshot`, which W13 compares with a live schema as it compares two live ones.
  Committed to Git at each release, the file's history is the schema's history
  ([Version control](version-control.md#vc3--schema-snapshots)).

## The question

Three questions come up, in this order of frequency:

1. **"Is staging like production?"** Two live schemas, often on two data sources of the same
   engine, sometimes two schemas on one (`app` and `app_next` during a migration's rehearsal).
2. **"What changed since the release?"** One side in the past. A snapshot committed at the
   release answers it, without keeping an old database around.
3. **"What do I run to make them alike?"** The script, in the order that works, to read, edit
   and run in a console.

Other tools answer them in a similar shape. pgAdmin's Schema Diff lists objects as identical,
different, or present on one side only, shows each object's DDL from both sides, and generates a
script. DataGrip compares two schemas' objects in its diff viewer. migra wrote the SQL that
turns one PostgreSQL database into another, from the command line, and Atlas's `schema diff`
does the same for several engines. DbRover's version follows the shape
they share and keeps to its own rules: the script is shown, never run, and the files are plain
text you can keep in Git.

## What is compared

A schema, as compared, is its **tables, indexes and views**. Each kind is matched by name
between the two sides, so each object is **only on the left**, **only on the right**, the
**same**, or **different**, with what differs.

| Object | What is compared | Matched by |
| --- | --- | --- |
| **Table** | Its columns, primary key, unique keys, foreign keys and checks | Name |
| **Column** | Type, nullability, default, identity | Name, within its table |
| **Primary key** | Its columns, in order, and its name | One per table |
| **Unique key, foreign key, check** | Columns; a foreign key's target table, columns and actions; a check's condition | Constraint name, within its table |
| **Index** | Its table, whether it is unique, the method, the key (columns or expressions, in order), included columns, and the `WHERE` of a partial index | Name |
| **View, materialized view** | Its kind and its query | Name |

**Left out on purpose,** because they differ between copies of the same schema without the
schema being different, or because they are not what W13 is for: owners, grants, comments,
tablespaces, storage parameters, statistics targets, column order, object ids, and the
server's version. Column order is shown in each side's definition but never counted as a
difference: PostgreSQL can't reorder columns, and two schemas built by different migrations
often list them differently.

**Not compared in the first version,** and named as such in the manual: sequences that no
column owns, functions and procedures, triggers, types (enums, domains, composites),
extensions, partitions and a table's `PARTITION BY`, exclusion constraints, policies (row-level
security) and publications. A table, index or view that uses one of them is compared as written
(an enum-typed column's type is its type's name), so a renamed enum still shows as a changed
column. Each could be added later as a kind of its own, if asked.

### Names inside definitions

The same object in two schemas says its own schema's name in many places: a view's query reads
`staging.orders` on one side and `public.orders` on the other, a column's default is
`nextval('staging.orders_id_seq'::regclass)`, an enum column's type is `staging.status`. Compared
as text, every one would differ.

PostgreSQL prints a name unqualified when its schema is on the search path. So a schema is read
inside one read-only transaction that first sets the search path to that schema alone
(`SET LOCAL search_path TO staging`): `pg_get_viewdef`, `pg_get_expr` (defaults, checks,
partial indexes' conditions) `pg_get_indexdef` and `format_type` then print the schema's own
objects without it, and other schemas' objects with theirs. Both sides are read the same way,
so `orders` matches `orders` and `audit.events` still says `audit`. The transaction is
`REPEATABLE READ`, so the several catalog queries see one moment of the schema.

A foreign key's target is kept the same way: a table in the compared schema by its name alone,
any other by `schema.name`. The script writes each one back with the schema it runs on.

### Serial columns

A `serial` column is an `integer` whose default calls a sequence the column owns. Compared as
written, a new table with a `serial` column would be created with a default naming a sequence
that doesn't exist on the side being changed. So a column whose default is `nextval` of a
sequence that the column owns (`pg_get_serial_sequence`) is read as `smallserial`, `serial` or
`bigserial` with no default, as it was most likely written, and `CREATE TABLE` creates the
sequence again. Identity columns (`GENERATED … AS IDENTITY`) are read as E4 reads them.

### Normalising text

Text the database prints is compared after the same small normalisation on both sides, done once
where the model is built, so a live schema and a snapshot of it compare equal by construction:
trailing spaces on each line and a final `;` are removed from queries, and types, defaults and
conditions are trimmed. Nothing else is rewritten: two views whose queries mean the same but
read differently are different, and the side-by-side view shows how.

## The model

A new crate, **`dbrover-schema`**, holds everything W13 and VC3 do without a database or a
window: the model, the comparison, the script and the snapshot file. It depends on
`dbrover-core` for the catalog's descriptions and on `dbrover-sql` for E-track's DDL builder.

- **`dbrover_core::SchemaDescription`**: what a driver reads, the tables (each a
  `TableDescription` and its `TableConstraints`, as Modify table loads them), indexes and views.
  Read by a new `Connection::describe_schema(schema)`, whose default refuses with "this database
  can't be compared yet"; PostgreSQL's reads the whole schema in a handful of catalog queries,
  not one per table.
- **`Schema`**: the description normalised as above, with every collection in a `BTreeMap` by
  name, so iteration order (and with it every list, file and script) is the same whatever order
  the catalog returned. Tables keep E-track's `TableSpec` for their columns and constraints,
  so a table needs no second description.
- **`Comparison`**: `compare(&left, &right)` gives, per kind, the objects only on one side and
  the ones that differ, each difference spelt out (a column added, a type changed from and to, a
  key's columns). Pure and fast: a schema of a thousand tables compares in milliseconds.
- **Engine:** `Command::DescribeSchema { data_source, schema }`, answered by
  `Event::SchemaDescribed`, on the explorer's connection, as `LoadTable` is; reading and writing
  snapshot files are commands of their own, so the window never waits on a disk or a network.

## The script

The script turns the **left** schema into the **right** one, like a diff's old and new: run on
the left, it makes it match the right. **⇄ Swap** turns it round. It is written by
`Comparison::script(dialect)`, from E-track's builder wherever one exists, and opened in a
console; it is never run by DbRover.

### Order

Statements come in phases, so each one finds what it needs and nothing it would trip over:

1. **Drop views** that go or change, and views that read a table about to change, in reverse
   dependency order (a view reading another view is dropped first). A view's dependencies are
   read from the catalog (`pg_depend` through its rewrite rule), within the schema.
2. **Drop indexes** that go or change.
3. **Drop foreign keys** that go or change, on every table, before any table is dropped, so a
   key between two tables never blocks either.
4. **Drop tables** that go, without `CASCADE`: anything still depending on one is a reason for
   the script to fail where the user can see it, not to drop more than was listed.
5. **Create tables** that are new, without their foreign keys.
6. **Change tables** that differ, without adding foreign keys: E4's
   `DdlOperation::AlterTable`, from the left's table to the right's, in E4's own order (drop
   constraints, drop, rename and add columns, change columns, add keys and checks).
7. **Add foreign keys,** new tables' and changed ones', once every table they point at exists.
8. **Create views** that are new or were dropped in phase 1, in dependency order.
9. **Create indexes** that are new or changed, after the views, since a materialized view can
   have indexes.

Phases 3, 5, 6 and 7 are E4's alteration run in stages: each table's change is split into
"without the foreign keys that go", "without the ones that come" and "as it ends", and each
stage is an `AlterTable` from the one before. So W13 adds no `ALTER TABLE` writer of its own,
and E4's golden tests already cover what each stage writes.

### What it looks like

```sql
-- Changes schema staging on shop-dev (left) to match schema public on shop (right).
-- Written by DbRover's schema compare. Read it before running it: nothing has run.
-- Columns are matched by name, so a renamed column is dropped and added again: edit the
-- script to rename it instead, or its data is lost.

BEGIN;
-- The definitions below name the schema's own objects without the schema.
SET LOCAL search_path TO staging;

-- Drops column orders.legacy_code and the values in it.
ALTER TABLE staging.orders DROP COLUMN legacy_code;

ALTER TABLE staging.orders ADD COLUMN note text;

CREATE INDEX orders_ordered_on_idx ON staging.orders USING btree (ordered_on);

COMMIT;
```

- **Names** are written for the left's schema, quoted as E-track quotes them (bare where that
  reads the same everywhere).
- **The search path** is set to the left's schema for the script's transaction
  (`SET LOCAL`), because the definitions it copies (a view's query, a default, a check, an
  enum column's type) name the schema's own objects without the schema, as they were read.
  Without it they would resolve to whatever schema the console's search path finds first.
  *(Added in W13d: the plan's example had no such line.)*
- **A primary key** keeps the right's name: one created or re-added under another name is
  renamed with `ALTER TABLE … RENAME CONSTRAINT`.
- **`BEGIN` and `COMMIT`** wrap it where DDL is transactional, as PostgreSQL's is, so a failure
  part way leaves the console in a failed transaction to roll back, with nothing half done. The
  console's transaction status shows it, as for any statement.
- **Destructive statements** (dropping a table, a column, a view's data in a materialized view)
  each get a comment saying what is lost.
- **What can't be written** is a comment in its place, with the reason, and the comparison marks
  the difference: "-- Not written: column orders.id becomes an identity column, which can't be
  changed in place; change it by hand." The rest of the script is still written. So far that
  is a column becoming or ceasing to be an identity column, a column changing to or from a
  serial type, and any change E4's builder refuses.
- **An empty comparison** has no script: the tab says the schemas are the same, and
  **Open script** is off.

### Running it

**Open script in console** opens a new console with the script, on the left's data source when
the left is live; when the left is a snapshot, the menu lists the data sources of the same
engine to pick from. The console is an ordinary one: it runs nothing until **Run** is clicked,
and a red data source's confirmations, a read-only data source's refusal and the history all
apply as for any statement. **Copy script** puts it on the clipboard.

After a run, **Compare again** reads both live sides again; the exit is that it comes back
empty.

## Snapshot files (VC3)

### The format

**TOML, written by DbRover's own writer, one object per table of the file and one column per
line,** read back with the `toml` crate. Version control's plan leant this way; W13 confirms it.

```toml
# A schema snapshot written by DbRover. Compare it with a live schema in DbRover's schema
# compare, or with `dbrover schema compare`. Regenerated whole each time; edits are lost.

[snapshot]
format = 1
engine = "postgresql"
schema = "public"

[tables.customers]
columns = [
  { name = "id", type = "integer", nullable = false, identity = "always" },
  { name = "email", type = "text", nullable = false },
  { name = "created_at", type = "timestamp with time zone", nullable = false, default = "now()" },
]
primary_key = { name = "customers_pkey", columns = ["id"] }
unique_keys = [
  { name = "customers_email_key", columns = ["email"] },
]

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
checks = [
  { name = "orders_total_check", condition = "total >= 0::numeric" },
]

[indexes.orders_customer_id_idx]
table = "orders"
method = "btree"
key = ["customer_id"]

[views.recent_orders]
kind = "view"
reads = ["orders"]
query = """
 SELECT id,
    customer_id,
    total
   FROM orders
  WHERE id > 1000"""
```

- **Deterministic:** tables, indexes and views sorted by name; columns in the table's order;
  constraints sorted by name; keys left out when they hold their default (`nullable = true`, no
  `default`, `unique = false`, actions of `no action`); no timestamp, server version, data
  source, host or object id. A snapshot taken twice of an unchanged schema is the same file,
  byte for byte, and a test says so.
- **Diffable:** one column, key or check per line, so adding a column is one added line in
  `git diff`, and a type change is one changed line.
- **Readable without DbRover:** the keys are the words the manual uses; a pull request's
  reviewer reads the diff as it is.
- **`engine`** is checked when comparing: a PostgreSQL snapshot compares only with a PostgreSQL
  schema. **`schema`** is informational: a snapshot of `public` may be compared with
  `staging`, as two live schemas may.
- **`format`** is the file's version. A newer format is refused with "written by a newer
  DbRover"; an older one is read, and rewritten in the new format on the next snapshot.
- **Why our own writer,** rather than `toml::to_string` of the model: the `toml` crate's
  serializer chooses between inline and standard tables, and its spacing, by its own rules,
  which can change with a version. The file's layout is a promise to Git; a hundred lines of
  writer keep it, with a round-trip test that what is read back writes the same bytes.

### Writing one

- **In the app:** right-click a schema → **Save schema snapshot…** opens a save dialog named
  `<schema>.schema.toml` by default; the engine reads the schema and writes the file through a
  temporary file and a rename, so a failure leaves any earlier file whole. The compare tab's
  sides also have **Save snapshot…**.
- **From the command line,** for a CI job that commits one after every migration:

  ```text
  dbrover schema snapshot <data source> <schema> [-o <file>] [--config-dir <folder>]
  ```

  The data source by its name, as in the app; the password from the keychain or the data
  source's password command, and the SSH tunnel if it has one, as `dbrover mcp` connects.
  Without `-o`, the file goes to standard output. Exits 0 when written and 2 otherwise (an
  argument it can't follow, a data source it can't find or connect to, a schema that doesn't
  exist), with the reason on standard error: the codes `dbrover schema compare` uses.

### Comparing with one

A side of the compare tab is a live schema **or** a snapshot file (**Snapshot file…** in the
side's picker), and `dbrover schema compare` takes either:

```text
dbrover schema compare <left> <right> [--script] [--config-dir <folder>]
```

Each side is `<data source>:<schema>` (split at the last `:`) or the path of a snapshot file.
It prints the differences, or with `--script` the script, and exits as `diff` does: **0** when
the schemas are the same, **1** when they differ, **2** when something went wrong. A CI job
that fails when production has drifted from the committed snapshot is one line.

## The tab

**Compare schema…** on a schema in the explorer opens a **Schema compare** tab with that schema
on the left, beside the ER diagram's entry. The tab has three parts:

- **The sides,** at the top: two pickers, **Left** and **Right**, each a data source and a
  schema (the data sources offered for the right are those of the left's engine), or a
  snapshot file. **⇄** swaps them, **Compare** reads both and compares, and **Compare again**
  re-reads them after a change. Each side has **Save snapshot…**.
- **The differences,** on the left below: grouped as **Tables**, **Indexes** and **Views**, each
  object with a mark (**+** only on the right, **−** only on the left, **~** different) and a
  one-line summary ("2 columns added, 1 changed"). Under a changed table, each column or key
  that differs, with its before and after ("total: numeric(10,2) → numeric(12,2)"). A filter
  hides kinds; "The schemas are the same." when there is nothing to list.
- **The definitions,** on the right below: the selected object's definition from each side,
  side by side, as SQL (`CREATE TABLE`, `CREATE INDEX`, `CREATE VIEW`, from E-track's builder),
  with the lines that differ marked, aligned by a line diff. An object on one side only shows
  an empty pane opposite it.

The toolbar has **Open script in console**, **Copy script**, and the number of differences.
Reading a large schema shows a spinner on its side; a side that fails shows the database's
error in its place, and the other side still loads.

The tab is not saved between runs, as the definition tab isn't: its sides are re-picked from
the explorer in a click, and a stale comparison read back from disk would mislead.

## Engines

PostgreSQL first, as the roadmap says. `Connection::describe_schema` is the one piece each
further engine adds; the model, the comparison, the file and the tab are the engine's dialect
away from working. The script is E-track's builder plus the order above, so an engine whose
`ALTER TABLE` E4 already writes (MySQL, SQL Server, DuckDB) needs only its reading and its
view dependencies. SQLite's tables rebuild under E4, which the script would reuse unchanged.
Redshift, which speaks PostgreSQL's protocol, reads its catalog differently and stays out until
asked.

The menu items show where the data source's driver reads schemas, from a new
`Capabilities::schema_compare`, which is on for PostgreSQL's dialect and off for Redshift's
driver.

## Options weighed

### The snapshot's format

| Option | Deterministic | Diff in Git | Read back for comparing | Verdict |
| --- | --- | --- | --- | --- |
| **TOML from the model, our own writer** (above) | By construction: sorted maps, no volatile fields | One line per column or key | Straight into the model with serde; compares empty with its own schema by construction | **Chosen.** |
| **DDL, as Copy DDL writes it** | Yes, if sorted | Reads well; a column is one line | Needs a parser per dialect to read back, or running it in a scratch database; and a snapshot that can be run invites applying it, which VC1 keeps out | **No.** The definitions pane shows DDL already, for reading. |
| **JSON** | Yes | Noisier: quotes and braces, no comments, a column split over lines | serde | **No.** TOML is every other DbRover file. |
| **`pg_dump --schema-only`** | No: order and `SET` lines vary with versions | Hundreds of lines that don't matter | A parser again | **No.** The problem this plan exists to fix. |
| **Both TOML and DDL** | Yes | Twice the diff | One of them is ignored | **No.** One source of truth. |

### Matching objects

| Option | Verdict |
| --- | --- |
| **By name** (chosen) | Deterministic and explainable: a renamed column or index shows as one removed and one added, and the script's header says so. |
| **Guessing renames** from similar names or identical definitions | **No.** A wrong guess turns a drop into a rename silently, the opposite mistake to the one E4 avoided by tracking renames in the dialog rather than guessing. A **Treat as renamed** pairing in the tab could come later, if asked. |
| **Constraints by content, ignoring names** | **No.** PostgreSQL names constraints the same way from the same DDL, so names match where the schemas were built alike; where they don't, a renamed constraint is cheap to drop and add. |

### Where the script comes from

| Option | Verdict |
| --- | --- |
| **E-track's builder, staged in phases** (chosen) | One `ALTER TABLE` writer for Modify table and schema compare; its golden tests cover both. |
| **A writer of its own, as migra's** | **No.** Two writers to keep right, which would drift. |
| **`CREATE OR REPLACE VIEW` for changed views** | **No.** PostgreSQL refuses it when the columns change, except by adding at the end; drop and create always works, and the script says why it drops. |

## Layers

One GitHub stack; each layer one pull request, tested on its own, on synthetic schemas only.

1. **W13a — The plan:** this document, and the roadmap's W13 and VC3 entries linking it.
2. **W13b — The model and the comparison:** `dbrover-schema` with `Schema`, `compare` and the
   differences, and `dbrover_core::SchemaDescription`; unit tests of every kind of difference
   and of normalising.
3. **W13c — Reading a PostgreSQL schema:** `Connection::describe_schema`, PostgreSQL's in one
   read-only transaction with the search path set, serial columns, view dependencies,
   `Capabilities::schema_compare`, and `Command::DescribeSchema`; driver tests on the PostgreSQL
   test server, comparing two schemas built alike as empty.
4. **W13d — The script:** the phases, E4's alteration in stages, indexes and views, what can't
   be written; golden tests per phase, and the **W13 exit** as an engine test on the PostgreSQL
   test server: two schemas differing by a column and an index list both, the script run on the
   left makes the comparison empty.
5. **VC3a — Snapshot files:** the writer and reader, `dbrover schema snapshot`, and the
   engine's command; round-trip tests, and the first half of the **VC3 exit** (two snapshots of
   `public` identical) on the PostgreSQL test server.
6. **W13e — The tab:** **Compare schema…**, the sides, the differences, the definitions side by
   side, **Open script in console**, **Copy script**; kittest tests of the tab from described
   schemas, and a whole-app test on the PostgreSQL test server.
7. **VC3b — Comparing with a snapshot:** a snapshot as a side in the tab, **Save schema
   snapshot…**, and `dbrover schema compare`; the rest of the **VC3 exit** on the PostgreSQL
   test server.
8. **W13f — Manual and demo notes:** a chapter, *Comparing schemas*, under *Browsing and changing
   databases*; the CLI in the command-line reference; W13's and VC3's demo notes.

The PostgreSQL tests run on the compose server CI already starts, in the test binaries that
already run there: no new CI job.

## Decisions

| Question | Decided | Consequence |
| --- | --- | --- |
| What is compared | **Tables, columns, keys, checks, indexes and views**; owners, grants, comments, storage and column order never | Copies of a schema compare empty; sequences, functions, triggers, types and partitions are listed in the manual as not compared yet. |
| Matching | **By name,** no rename guessing | A renamed column is a drop and an add, and the script's header says to edit it into a rename. |
| Names inside definitions | **Read with the search path set to the schema** | `staging.orders` and `public.orders` compare equal inside views, defaults and checks, with no text rewriting. |
| Serial columns | **Read back as `serial`** when the column owns its sequence | A new table's `serial` column creates its sequence. |
| Script direction | **Left into right,** like a diff; **⇄** swaps | One rule, the same in the tab and the CLI. |
| The script's writer | **E4's `AlterTable`, staged,** and E-track's create and drop | No second `ALTER TABLE` writer. |
| Running the script | **Never by DbRover:** opened in a console, or copied | Every guard a console has applies; nothing runs unseen. |
| Snapshot format | **TOML, DbRover's own writer, one column per line** | Byte-identical snapshots, one-line diffs, read back with serde. |
| Snapshot contents | **No timestamps, versions, hosts or ids**; `engine` and `schema` only | Snapshots taken on two machines of the same schema are the same file. |
| CLI | **`dbrover schema snapshot`** and **`dbrover schema compare`**, exiting as `diff` does | A CI job snapshots after a migration and fails on drift. |
| Engines | **PostgreSQL first,** through `Connection::describe_schema` | Each further engine adds its reading; the rest is shared. |

## Risks

- **A script that destroys data.** Dropping a column or table loses its rows, and a rename
  matched by name is a drop. The script is only ever opened, its header says how renames read,
  each destructive statement has a comment, and the console's confirmations still apply.
- **False differences.** Text the database prints can differ for the same meaning (a cast
  written two ways). Reading with the search path set removes the largest source; what's left
  shows in the definitions pane, where the reader can judge it. A test builds the same schema
  twice under different names and compares it empty.
- **A script that fails part way.** The phases order what PostgreSQL needs, but a dependency
  outside what W13 reads (a function using a dropped column, a trigger) can still stop it. The
  script runs in one transaction, so it stops with nothing applied, and the error names the
  object.
- **Snapshots that churn.** A field that changes on its own would make every snapshot a diff.
  The format holds nothing volatile, and the exit's test takes two snapshots and compares the
  bytes.
- **Scope creep towards migrations.** A snapshot is one side of a comparison, not a migration
  history; DbRover doesn't apply snapshots or track which scripts ran. Flyway, Liquibase,
  sqitch and Atlas stay the tools for that, as [Version control](version-control.md) says.

## What stays out

- Running the script, applying a snapshot, or keeping a history of what ran: migration tools'
  work.
- Guessing renames.
- Comparing data (rows), only structure.
- Comparing across engines (PostgreSQL with MySQL).
- Owners, grants, comments and storage settings.
- Schema compare over MCP; an assistant can run `dbrover schema compare` like anyone else, and
  a tool of its own can come later if asked.
