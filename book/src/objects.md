# Creating and changing tables

The explorer can create tables and indexes, and rename, empty and drop tables, without you
writing the SQL. Each of these helpers is a small dialog that writes the statement, **shows it
to you**, and runs it only when you click **Execute**. You can also take the SQL to a console
and change it there first, or just copy it: the helpers are a quick way to learn how each
database spells a statement.

## Where to find them

Right-click in the explorer. What the menu offers depends on what you clicked and on what the
database can do:

| Right-click on | Menu items |
| --- | --- |
| A schema, or its **tables** folder | **New › Table…** |
| A table | **Modify table…**, **New index…**, **Rename…**, **Copy DDL**, **Truncate…**, **Drop…** |
| A table's **indexes** folder | **New › Index…** |
| An index | **Drop index…** |

![A table's menu, with the object helpers below Refresh](images/explorer-menu.png)

- **ClickHouse** has no index helpers: its data-skipping indexes need a type and a granularity;
  write `ALTER TABLE … ADD INDEX` in a console.
- **Copy DDL** is offered where there is a `CREATE TABLE` statement to copy (see
  [Copy DDL](#copy-ddl)).
- Views, materialized views and schemas have helpers of their own: see
  [Views and schemas](views-schemas.md).
- On PostgreSQL, SQL Server and DuckDB, the helpers, **Copy DDL** and **Import CSV…** appear
  only inside the database the data source connects to, which is where their statements run. Browse another database's objects
  freely, but change them from a data source that connects to it.

## Review and run

Every helper ends the same way, with the SQL it will run:

![New table, with the statement it will run](images/new-table.png)

- **Execute** runs the statement. If the database refuses it, its error shows in red under the
  SQL and the dialog stays open, so you can fix what you typed and try again. Once it succeeds
  the dialog closes, and the explorer reloads the folder that changed and opens it, so you see
  the new table or index straight away.
- **Open in console** puts the SQL in a new console instead, to edit or run it there. The
  dialog closes.
- **Copy** copies the SQL.

The SQL changes as you fill in the dialog. While something is missing (a table name, a type,
the columns of a key) the dialog says what instead of showing SQL, and **Execute** waits.

Names are quoted only when they need to be: when they have capitals, spaces or other characters
a bare name can't have, or are a word any of the databases DbRover supports reserves, such as
`order` or `user`. `customer_id` stays `customer_id`; `Order Items` becomes `"Order Items"`
(`` `Order Items` `` on MySQL and ClickHouse, `[Order Items]` on SQL Server).

### When a change does several things

Some changes take more than one statement. On **PostgreSQL**, **SQLite**, **SQL Server** and
**DuckDB** they run in one transaction: if one fails, the ones before it are undone and
nothing has changed. **MySQL**, **MariaDB** and **ClickHouse** commit each statement as it
runs, so the panel warns you before you run several, and after a failure says how many of
them stay applied.

### Safety levels and colours

Helpers follow the data source's [safety level](data-sources.md#safety-levels):

- **Read-only**: **Execute** is greyed out and the panel says why. **Open in console** and
  **Copy** still work, so you can take the SQL somewhere it may run.
- **Careful**: **Execute** asks once more, naming the data source, before anything runs.
- **Normal**: **Execute** runs at once.

Truncating or dropping something says exactly what goes: the table and every row in it, with
its indexes, keys and triggers. On a data source coloured **red**, the colour that marks
production, you also type the object's name before **Execute** can be clicked:

![Drop on a red data source, waiting for the table's name](images/drop-table.png)

Every statement a helper runs is kept in the [query history](history.md), like those
you run in a console.

## New table

Right-click a schema, or its **tables** folder, then **New › Table…**. The dialog starts with
one column, `id`, of the database's most common type, as the primary key: change or remove it
as you like.

- **Schema** and **Name**: where the table goes, and what it is called. The schema is the one
  you clicked.
- **Columns**, one row each:
  - **Name**.
  - **Type**, typed as the database spells it, so arrays, domains and extension types work
    too. The ▾ beside it lists the database's common types (`integer`, `text`, `timestamptz`
    and so on on PostgreSQL; `UInt64`, `String`, `DateTime` on ClickHouse).
  - **Nullable**: whether the column may hold `NULL`. A primary key column never may.
  - **Default**: an SQL expression, written as it goes after `DEFAULT`: `0`, `'new'` (with
    quotes, for text), `now()`. SQLite and MySQL want parentheses around an expression that
    calls a function: `(datetime('now'))`.
  - **Primary key**: tick each column of the key. The key's columns are in table order.
  - **Add column** adds a row; the ✕ at the end of a row removes it.
- **Unique keys**: an optional name, and the columns that must be unique together, in the order
  you tick them.
- **Foreign keys**: an optional name, the columns, the table they point at, and that table's
  columns, in the same order. The table is picked from those the explorer has loaded: expand a
  schema's **tables** folder first to point at its tables, or pick **This table** for a key
  between the table's own rows. Its columns load once you pick it. **On delete** and **On
  update** say what happens to these rows when the row they point at goes or changes: refuse
  (`NO ACTION`, the default), `CASCADE`, `SET NULL` and so on, as the database offers them.
- **Checks**: an optional name and a condition every row must meet, such as `quantity > 0`.

What differs between databases:

- **ClickHouse** has no unique or foreign keys, so those tabs aren't shown. Its tables are
  created with the general-purpose `MergeTree` engine, sorted by the primary key's columns
  (`ORDER BY`), and a nullable column's type is wrapped in `Nullable(…)`.
- **DuckDB**'s foreign keys only refuse a change: there is no **On delete** or **On update**.
- **SQLite**'s foreign keys can only point at a table in the same schema (the same database
  file). SQLite checks them only on connections that switch them on with
  `PRAGMA foreign_keys = ON`.
- **SQL Server** gets an explicit `NULL` on nullable columns, since whether a column is
  nullable by default there depends on the connection's settings.

## Modify table

**Modify table…** on a table opens the New table dialog filled from the table as it is: its
columns with their types, nullability and defaults, its primary key, and its unique keys,
foreign keys and checks under the names the database gave them. Change what you like; the SQL
below shows the statements that turn the table into what the dialog now says.

![Modify table, with a column renamed and one added](images/modify-table.png)

- **Rename a column** by typing its new name. DbRover remembers which row each column came
  from, so this is always a rename that keeps the column's values, never a drop and an add.
  Keys that list the column follow its new name.
- **Drop a column** with the ✕ at the end of its row, and **add one** with **Add column**.
  Dropping a column loses its values, so the dialog says which columns go before **Execute**,
  and on a red data source asks for the table's name.
- **Change a column's type, nullability or default** in its row. On **PostgreSQL** and
  **DuckDB**, a retyped column gets a **USING** field below the grid, for an expression that
  converts the old values (`CASE status WHEN 'new' THEN 0 ELSE 1 END`); left empty, the
  database converts them its own way, or refuses.
- **Keys and checks**: tick or untick primary key columns, and add or remove unique keys,
  foreign keys and checks in their tabs. One you leave alone is kept, even if one of its
  columns is renamed; one you change is dropped and added again.
- **Rename the table** with **Name**. The table stays in its schema.
- A column the database numbers itself (an identity or `AUTO_INCREMENT` column) says
  **identity** beside its type, and stays so.

The column order doesn't change: a new column goes at the end. Statements run in an order that
works everywhere: constraints that go are dropped first, then columns are dropped, renamed,
added and changed, then the new keys and checks are added, and the table is renamed last.

Not every database can make every change in place. What one can't do, the dialog says instead
of showing SQL, and **Execute** waits; the rest of the change can still go to a console.

| Database | In place | Refused |
| --- | --- | --- |
| PostgreSQL | everything | — |
| MySQL, MariaDB | everything; a new type or nullability restates the column (`MODIFY COLUMN`), so its comment and character set go back to the defaults | — |
| SQL Server | everything; a column's default is a named constraint, dropped and added again around a change to the column | — |
| DuckDB | columns: rename, add, drop, type, nullability, default | keys and checks, the primary key |
| ClickHouse | columns (nullability is its `Nullable(…)` type) and checks | the primary key, which is the table's sort order |
| SQLite | rename, add a nullable column or one with a default, drop | — (see below) |

### SQLite's table rebuild

SQLite's `ALTER TABLE` does little in place, so for anything else (a new type, a column made
`NOT NULL`, a new key or check) DbRover rebuilds the table the way SQLite's documentation
describes, in one transaction, and says so before **Execute**:

1. It creates the new table as `new_orders`, copies every row into it, drops the old table and
   gives the new one its name.
2. It creates the table's indexes and triggers again, from their definitions.
3. Only then does it rename and drop columns, in place, so that SQLite itself updates the
   indexes, triggers, views and other tables' foreign keys that name a renamed column, and
   refuses to drop a column one of them still uses.

Foreign keys are switched off while it runs, so dropping the old table can't delete rows that
point at it (`ON DELETE CASCADE`). If your connection had them on, DbRover checks them before
committing: a change that would leave a row pointing at nothing is undone, and the dialog
names the table.

A rebuild writes the table from what DbRover reads of it, so a table with something it can't
read back is refused, with the reason: a column with its own `COLLATE`, a generated column, a
`WITHOUT ROWID` or a `STRICT` table. Make those changes in a console.

## New index

**New index…** on a table, or **New › Index…** on its **indexes** folder.

- **Columns**: tick them in the order the index should have them.
- **Unique**: the index also refuses two rows with the same values.
- **Method**, where the database lets you choose: on PostgreSQL B-tree (the default), Hash,
  GIN (for arrays, `jsonb` and full-text search), GiST and BRIN; on MySQL and MariaDB, B-tree
  or Hash.
- **Name**: filled in from the table and columns (`orders_customer_idx`) until you change it.

**Drop index…** on an index drops it, after saying so. The table and its rows stay. An index
that a primary key or unique constraint owns can't be dropped on its own; the database's error
says so.

## Rename, truncate and drop

- **Rename…** gives the table a new name in the same schema: `ALTER TABLE … RENAME TO …`, or
  `RENAME TABLE` on MySQL and ClickHouse, or `sp_rename` on SQL Server.
- **Truncate…** deletes every row and keeps the table. SQLite has no `TRUNCATE`, so it writes
  `DELETE FROM` the table, which does the same.
- **Drop…** drops the table. On PostgreSQL, **Also drop what depends on it (CASCADE)** drops
  the views that read the table and the foreign keys of other tables that point at it too;
  without it, PostgreSQL refuses while anything depends on the table.

## Copy DDL

**Copy DDL** on a table copies its `CREATE TABLE` statement.

- **SQLite**, **MySQL**, **MariaDB**, **DuckDB** and **ClickHouse** keep the statement, so
  DbRover copies theirs, the same one **Show definition** shows.
- **PostgreSQL** and **SQL Server** keep no such statement, so DbRover writes one from the
  table's columns (their types, defaults and `NOT NULL`) and primary key, the same way New
  table does, followed by an `ALTER TABLE … ADD CONSTRAINT …` for each of its unique keys,
  foreign keys, checks and (on PostgreSQL) exclusion constraints, as the database defines
  them. Running it recreates the table, identity columns included (PostgreSQL's
  `GENERATED … AS IDENTITY`, SQL Server's `IDENTITY(seed, increment)`). What it leaves out:
  indexes that aren't constraints, comments, storage parameters and partitioning.
