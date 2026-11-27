# Creating and changing database objects

Status: **E0, E1 and E2 built** (2026-10-26), **E3 built** (2026-10-27), **E4 built**
(2026-10-28). Proposed
on 2026-10-23; on 2026-10-26 indexes moved from Later into E1 and schemas into E2.

Today the explorer only reads: it browses the catalog, opens a table's data and shows
definitions. Creating or changing anything means writing the SQL by hand in a console. This
plan adds helpers like those in desktop database tools (pgAdmin, DBeaver, DataGrip,
TablePlus) for the kinds of object people manage most: **tables** and their **indexes**,
**views**, **schemas** and **users**. Each one is a dialog that writes the SQL, shows it, and
runs it.

## What the user sees

Right-clicking in the explorer gains a **New** submenu and object actions, depending on what
was clicked and what the database supports:

| Clicked on | New | Object actions |
| --- | --- | --- |
| A database, or the data source where schemas are its top level | Schema | — |
| A schema | Table, View, Materialized view (PostgreSQL) | Drop schema… |
| A schema's tables folder | Table | — |
| A schema's views folder | View, Materialized view (PostgreSQL) | — |
| A table | — | Modify table…, Rename…, Truncate…, Drop…, Copy DDL, New index… |
| A table's indexes folder | Index | — |
| An index | — | Drop index… |
| A view | — | Edit view…, Rename…, Drop…, Copy DDL |
| A materialized view | — | Refresh, Edit…, Rename…, Drop…, Copy DDL |
| The data source, or its roles folder (PostgreSQL) | User, Role | — |
| A user or role | — | Edit…, Rename…, Drop… |

Every dialog ends the same way: **the SQL it will run, shown before it runs.** The user can
**Execute** it, **Open in console** to edit it first, or **Copy** it. Nothing reaches the
database until one of the three is clicked. This keeps the helpers honest and teaches the SQL
behind them, which matches the project's learning goal.

After a change succeeds, the explorer refreshes the parent node and selects the new or changed
object. A failure shows the database's error in the dialog, which stays open to fix it.

## Shared groundwork (E0)

Everything the helpers have in common, built once.

- **Object specs and a DDL builder in `dbrover-sql`.** Plain data types describe what the user
  asked for: `TableSpec` (columns, primary key, unique and foreign keys, checks) and
  `IndexSpec` in E1, `ViewSpec` and `SchemaSpec` in E2, `RoleSpec` in E3. Pure functions turn
  a spec into statements for a dialect, so the builder is tested without a database.
  - Identifiers are quoted by the dialect's rules, which DB0 gathered in one place.
  - Each change is described as an operation (`AddColumn`, `RenameTable`, `DropView`, …), so
    one dialog can produce several statements in a known order.
- **Capabilities per dialect.** What each engine can do (materialized views, roles,
  transactional DDL, which `ALTER TABLE` forms) is a table of facts next to the dialect. The
  menus and dialogs read it, instead of checking driver names. SQLite gets no users or roles,
  and its menus don't show them.
- **A "review and run" panel** reused by every dialog:
  - the generated SQL, highlighted and read-only;
  - Execute, Open in console and Copy;
  - the database's error inline when it fails.
- **An engine command for changes**, `Command::ApplyDdl { data_source, statements }`,
  answered by `Event::DdlApplied`. It runs on the explorer's session of that data source.
  - **On PostgreSQL, SQLite, SQL Server and DuckDB**, whose DDL is transactional, the
    statements run inside one transaction, so a failure leaves nothing half done.
  - **On MySQL and ClickHouse**, where DDL commits as it goes, the panel says that a failure
    part way leaves the earlier statements applied, and the answer says how many.
  - A read-only data source refuses it, and every statement sent is recorded in the history.
  - **Statements holding a password** arrive with E3. W4's redaction keeps them out of the
    history and the log: every statement recorded has its secrets replaced by `'<redacted>'`
    (`dbrover_store::redact_secrets`), so `ApplyDdl` needs no flag of its own (see E3).
- **Confirmations for destructive actions.** Drop and Truncate name the object and what goes
  with it (`CASCADE` drops dependent views). On a data source coloured red, the user types the
  object's name to confirm. The colour is how the M1 follow-up marks production.

**Exit:** a UI test builds a one-column table spec, runs it from the review panel, and sees the
table appear in the refreshed tree, on both engines.

**As built:** the helpers act only inside the database the data source connects to, where
their statements run (PostgreSQL's explorer browses other databases on connections of their
own). `describe_table` gained each column's type, nullability and default on every engine,
which Copy DDL's rebuild and E4 need. The UI tests run on SQLite; PostgreSQL and the other
engines are covered by the engine's round trip.

## E1 — Tables

- **New table** dialog:
  - name and schema;
  - a grid of columns: name, type, nullable, default, and primary key;
  - tabs for unique keys, foreign keys and checks.
  - The type field offers the dialect's common types (from the dialect facts) and accepts any
    text, so arrays, domains and extension types still work.
  - A foreign key picks its target table and columns from the cached catalog, the same
    source completion uses.
- **Rename…**: `ALTER TABLE … RENAME TO …`.
- **Truncate…**, and **Drop…** with an optional `CASCADE` (PostgreSQL), both behind the
  confirmations above.
- **New index…** on a table, or on its indexes folder:
  - name, columns in order, and **Unique**;
  - the method where the dialect has one (PostgreSQL's `btree`, `hash`, `gin`, `gist`, `brin`;
    MySQL's `BTREE` and `HASH`), from the capabilities;
  - ClickHouse's data-skipping indexes need a type and granularity, so it gets no New index
    until a later milestone.
- **Drop index…**, behind the confirmations above. MySQL and SQL Server name the table in
  `DROP INDEX … ON …`, which the builder writes from the index's place in the tree.
- **Copy DDL**: the `CREATE TABLE` statement for an existing table.
  - SQLite stores it, so it comes straight from the catalog.
  - PostgreSQL has no `pg_get_tabledef`, which is an open question in the explorer doc. The
    statement is rebuilt from `describe_table` plus the constraints the catalog already
    loads, through the same builder. This settles that M2 follow-up.

**Exit:**
- I create a table with a foreign key to another one, add an index on it, rename it, and drop
  it, on PostgreSQL and SQLite.
- A conformance check builds a table from a spec, then compares `describe_table` with the spec.

## E2 — Views and schemas

- **New view**: name, schema, and the query in the console's own editor (highlighting,
  completion, problems while typing). The dialog writes `CREATE VIEW`; it offers
  `CREATE OR REPLACE` when the name is taken and the dialect allows it.
- **New materialized view** (PostgreSQL): the same, plus "with data" or `WITH NO DATA`.
- **Edit view…**: opens the current definition, which `Show definition` already loads, in the
  dialog.
  - PostgreSQL saves it with `CREATE OR REPLACE VIEW`.
  - SQLite has no replace, so it drops and recreates the view inside one transaction.
- **Refresh** for materialized views: `REFRESH MATERIALIZED VIEW`, with `CONCURRENTLY` offered
  when the view has a unique index.
- **Rename…** and **Drop…** as for tables.
- **New schema…** on a database, or on the data source where schemas are its top level: the
  name, and the owner where the dialect has one (PostgreSQL's `AUTHORIZATION`).
  - SQLite has no `CREATE SCHEMA`: its schemas are attached files, so it gets none.
  - MySQL's schemas are its databases, so New schema writes `CREATE DATABASE` and sits on the
    data source.
- **Drop schema…**, with an optional `CASCADE` where the dialect has it, behind the
  confirmations above; the confirmation counts what the schema holds, from the loaded tree.

**Exit:** I create a view from a query, edit its query, and drop it, on both engines. On
PostgreSQL I also create and refresh a materialized view, and create a schema, put a table in
it, and drop it with `CASCADE`.

**As built:**
- The query is typed in an editor highlighted like the console's, without its completion and
  problems, which belong to the console's own state; sharing them is a follow-up.
- Edit view finds the query in the definition **Show definition** loads (`view_query`), so
  options before `AS` (SQL Server's `WITH SCHEMABINDING`, MySQL's `DEFINER`) are not kept.
  MySQL, DuckDB and ClickHouse use `CREATE OR REPLACE` like PostgreSQL; SQL Server uses
  `ALTER VIEW`. A materialized view has no `OR REPLACE`, so editing one drops and creates it
  in one transaction, losing its indexes, which the confirmation says.
- Schemas are offered on SQL Server and DuckDB too, and on ClickHouse, whose databases are
  the tree's schemas as MySQL's are. ClickHouse's materialized views (insert triggers into a
  table) get no helpers.
- E1's gap closed: SQL Server tables get Copy DDL, rebuilt like PostgreSQL's; the driver now
  writes each constraint's definition from its catalog view.

## E3 — Users and roles (PostgreSQL)

PostgreSQL has one kind of object for both: a **user** is a role that may log in. The menu
offers both words, and **New user** starts with LOGIN ticked. pgAdmin likewise uses one
"Login/Group Role" dialog for both.

- **Capabilities:** `Capabilities::users` says how an engine models its users. PostgreSQL's is
  `UserModel::Roles`; every other engine's is `UserModel::Unsupported` for now, and their menus
  show no users or roles. MySQL's `'name'@'host'` accounts and SQL Server's logins become
  variants of their own when they are built, without changing PostgreSQL's.
- **Catalog:** a **roles** folder at the top of a PostgreSQL data source, before its
  databases, because roles belong to the whole server. Each role shows whether it can log in
  and whether it is a superuser, and lists the roles it belongs to. This needs two new node
  kinds, `NodeKind::Role` and `FolderKind::Roles`. The `pg_*` roles PostgreSQL predefines
  are system objects, hidden unless the explorer shows them.
- **New user / New role** dialog:
  - name, and the password typed twice;
  - attributes: LOGIN, SUPERUSER, CREATEDB, CREATEROLE, INHERIT, REPLICATION, BYPASSRLS;
  - connection limit and expiry date (`VALID UNTIL`);
  - the roles it is a member of, picked from the roles folder (`IN ROLE`).
- **The password never travels or gets stored in clear.**
  - The dialog hashes it on the client into a SCRAM-SHA-256 verifier, which PostgreSQL stores
    as it is in place of a password (`PASSWORD 'SCRAM-SHA-256$4096:…'`). The hashing function
    is the one in `postgres-protocol`, which tokio-postgres already depends on, so DbRover
    builds no crate it didn't before.
  - **The review panel shows `PASSWORD '<redacted>'`, not the verifier.** A verifier lets
    whoever holds it guess the password offline, and it teaches nothing to read. Execute sends
    the verifier; **Copy** and **Open in console** are disabled while the SQL holds a password,
    since the redacted text would set a password nobody knows, and the real one would put the
    verifier on the clipboard.
  - **No `sensitive` flag**: the panel redacts with the function the history uses
    (`dbrover_store::redact_secrets`, from W4), which every history write and the DDL log line
    already go through, whatever sent the statement. A flag could be forgotten by the next
    caller; the redaction can't. Error messages name statements by position, never by text.
  - Needs PostgreSQL 10 or later, which accepts a verifier whatever `password_encryption`
    says. DbRover never falls back to sending the password as typed.
- **Edit…**: the same dialog, filled from the role's description (`Connection::describe_role`,
  `Command::DescribeRole`), producing one `ALTER ROLE` for what changed, plus `GRANT`/`REVOKE`
  for changed memberships. The password is left as it is unless a new one is typed, or
  **Remove password** is ticked (`PASSWORD NULL`).
- **Rename…**: `ALTER ROLE … RENAME TO …`.
- **Drop…**: PostgreSQL refuses to drop a role that still owns objects or holds privileges.
  The confirmation counts what the role owns in the connected database and names the
  databases it owns, and offers, in one transaction before `DROP ROLE`:
  - **Keep them**, which fails while the role owns anything;
  - **Give them to** another role: `REASSIGN OWNED BY … TO …`, then `DROP OWNED BY …` for the
    privileges left;
  - **Drop them**: `DROP OWNED BY …`, spelt out in the confirmation.

  `REASSIGN OWNED` and `DROP OWNED` act only in the connected database, which the
  confirmation says when the role owns databases.

Privileges on tables and schemas (a grants editor) are not part of E3. Each new engine adds its
own users when its driver lands:
- **MySQL** (DB1) has `'name'@'host'` accounts.
- **SQL Server** (DB2) has logins plus database users.

**Layers**, one stack:
1. This plan.
2. `RoleSpec` and the role operations in `dbrover_sql::ddl`, `ScramVerifier` and
   `Capabilities::users`, with golden tests.
3. `NodeKind::Role`, `FolderKind::Roles`, `Connection::describe_role` on PostgreSQL and
   `Command::DescribeRole`: round trips on the PostgreSQL test server, including logging in as
   the new role, and a test that no password or verifier reaches the history.
4. The roles folder in the explorer.
5. New user, New role and Edit.
6. Rename and Drop.
7. The manual's chapter and the demo note.

**Exit:**
- I create a login user with a password and a membership, log in as it from a new data
  source, change its attributes, and drop it.
- Its password appears in no history entry, log line or review panel.

**As built:**
- The tree needed little of its own once the driver listed the roles, so its layer also
  gives roles **Copy DDL**, rebuilt from the role's description through the same builder as
  New role, without its password.
- The dialog hashes the password as soon as both fields match, so a form never holds an
  operation with the password in it; a mismatch reads "The two passwords differ." and keeps
  **Execute** off.
- Drop starts on **Give it to** when the role owns something or holds privileges, and waits
  for the new owner to be picked.
- A role whose expiry was cleared reads back as never, not `infinity`.
- Built after W4, whose `redact_secrets` already kept passwords out of every history entry
  and the DDL log line, so E3's own masking became the panel's call to the same function;
  the builder writes `PASSWORD` after every other option, since the redaction hides every
  string to the end of the clause.

## E4 — Modify table

Changing an existing table is the hardest helper, so it comes last.

- **Modify table…** on a table opens the New table dialog filled from the live table: its
  columns, primary key, unique keys, foreign keys and checks. The dialog compares what the
  user changed with what it loaded, and the review panel shows the `ALTER TABLE` statements
  that turn one into the other:
  - add, drop and rename columns;
  - change a column's type, nullability and default;
  - change the primary key; add and drop unique keys, foreign keys and checks;
  - rename the table.
- **Diffing is pure code in `dbrover-sql`:** `DdlOperation::AlterTable(TableAlteration)` takes
  the table as loaded (`ExistingTable`), the spec the dialog ends with, and which columns were
  renamed, and writes each dialect's statements, with golden tests per dialect.
  - **A rename is never a drop plus an add.** Each row of the dialog remembers the column it
    was loaded from, so the alteration names its renames (old name → new name); a column
    whose row was removed is dropped, a row with no origin is added.
  - A constraint the user left alone is kept, even when one of its columns was renamed (the
    comparison follows the renames). A changed constraint is dropped and added again, since
    a constraint holds no data.
  - Statements come in an order that works on every engine: drop the constraints that go,
    rename columns, drop columns, add columns, change columns, add the new primary key,
    unique keys, checks and foreign keys, and rename the table last.
- **Per dialect**, from `Capabilities::table_changes`, which gains `ChangePrimaryKey` and
  splits constraints by kind:
  - **PostgreSQL** does all of it with `ALTER TABLE`; a type change can say how to convert
    the values (`USING`), in a field the dialog shows for each retyped column. DuckDB takes
    `USING` too.
  - **MySQL and MariaDB** change a column's type or nullability with `MODIFY COLUMN` and its
    full definition, and only its default with `ALTER COLUMN … SET DEFAULT`; they drop a
    primary key with `DROP PRIMARY KEY`, a unique key with `DROP INDEX`, a foreign key with
    `DROP FOREIGN KEY` and a check with `DROP CONSTRAINT`.
  - **SQL Server** renames columns with `sp_rename`, and keeps each default as a named
    constraint, which the loaded table names: changing a default, or the type of a column
    that has one, drops that constraint and adds it again.
  - **DuckDB** changes columns in place but can't add or drop constraints, nor change the
    primary key.
  - **ClickHouse** changes columns with `MODIFY COLUMN` (nullability is part of the type,
    `Nullable(…)`), adds and drops checks, and can't change its key, which is the table's
    sort order.
  - What a dialect can't do is refused in the dialog with the reason ("DuckDB can't add or
    drop constraints on an existing table."), not attempted: the review panel shows it
    instead of SQL, and **Execute** stays off. **Open in console** is still there for the
    rest.
  - The statements run in one transaction where DDL is transactional (PostgreSQL, SQLite,
    SQL Server, DuckDB); MySQL and ClickHouse commit each as it runs, as for every helper.
- **Reading the live table:** `Command::LoadTable` answers `Event::TableLoaded` with the
  `ExistingTable`, built from `describe_table` and a new `Connection::describe_constraints`,
  which each driver answers from its catalog: constraint names, unique keys, foreign keys with
  their `ON DELETE`/`ON UPDATE` actions, checks, the primary key's name and SQL Server's
  named defaults. MySQL's `describe_table` now reports a default as an SQL expression
  (MySQL prints a literal default unquoted), so writing it back means the same.
- **Identity and auto-increment columns** (E2's leftover): `ColumnDescription` and
  `ColumnSpec` gain the column's identity, so Copy DDL on PostgreSQL and SQL Server writes
  `GENERATED … AS IDENTITY` and `IDENTITY(seed, increment)`, and Modify table keeps them
  (MySQL's `MODIFY COLUMN` would otherwise drop `AUTO_INCREMENT`). The dialog shows a
  column's identity but doesn't change it; changing it is refused.
- **SQLite** only renames tables and columns, adds simple columns and drops columns in place.
  Anything else rebuilds the table, as SQLite's documentation describes, in one transaction:
  1. create the new table under a temporary name, with the columns it keeps under their old
     names and the columns being dropped still in it;
  2. copy the rows into it;
  3. drop the old table and give the new one its name;
  4. create the old table's indexes and triggers again, from their saved definitions;
  5. rename and drop columns in place, so SQLite itself rewrites the indexes, triggers,
     views and other tables' foreign keys that name a renamed column, and refuses to drop a
     column one of them still uses;
  6. where foreign keys were on, `PRAGMA foreign_key_check` before committing.

  `PRAGMA foreign_keys` can't change inside a transaction, so the engine switches foreign
  keys off before `BEGIN` and back on after, for every helper's DDL on SQLite: with them on,
  dropping the old table would run its `ON DELETE` actions on other tables' rows. A table
  the rebuild can't write back as it is (generated columns, `COLLATE`, `WITHOUT ROWID`,
  `STRICT`) is refused with the reason. It ships last, in its own layer, once its tests
  show rows, indexes, triggers and views survive.
- **Tests:** golden tests per dialect for every kind of change and every refusal; engine
  round trips on every test server that load a table, alter it through the builder, and
  compare `describe_table` and the loaded constraints with the new spec; SQLite rebuilds
  checked for rows, indexes, triggers, views and other tables' foreign keys; kittest tests of
  the dialog on SQLite.

**Layers**, one stack:
1. This plan.
2. Identity columns: `ColumnDescription::identity` in every driver, `ColumnSpec::identity`
   written by `CREATE TABLE`, and Copy DDL keeping them.
3. `TableAlteration` and each dialect's `ALTER TABLE`, with golden tests.
4. `Connection::describe_constraints`, `Command::LoadTable` and engine round trips.
5. Modify table in the dialog.
6. SQLite's rebuild.
7. The manual's chapter and the demo note.

**Exit:** I change a column's type and add a foreign key on a table with rows, on both engines,
and nothing is lost.

**As built:**
- The layers went as planned. Identity columns came first, so the spec the diff compares was
  complete before it was written.
- SQLite's `ALTER TABLE … RENAME TO` checks every view and trigger first, and refuses while one
  names the table the rebuilt one is about to replace, so the rebuild wraps that one rename in
  `PRAGMA legacy_alter_table = ON` and `OFF`; the renames and drops in place after it run with
  it off, so SQLite still rewrites and checks what names their columns.
- A rebuild is confirmed like a destructive change (the table's name typed on a red data
  source), since it drops the table after copying it; so is dropping columns.
- Renaming a column in the grid renames it in the dialog's key lists too, so its keys keep it.
- Key columns are never nullable in a loaded spec, as in a new one: SQLite reports its
  `INTEGER PRIMARY KEY` as nullable.
- SQLite keeps no names for unique and foreign keys outside the `CREATE TABLE` text, so a
  rebuild writes them unnamed. Its checks are read from that text.
- ClickHouse's sort key isn't read back, so its tables load without a primary key, and ticking
  one is refused, as any key change there is.
- Not built: changing a column's identity, reordering columns, and moving a table to another
  schema, each refused with the reason.

## Later

- **A privileges editor** for PostgreSQL.
- **Users** for MySQL (DB1) and SQL Server (DB2): planned as W18 in
  [Users beyond PostgreSQL](users-beyond-postgres.md).
- **Virtual views** (a saved query shown as if it were a view, never created on the
  server): a candidate once consoles can be pinned to the tree.

## Decisions

Proposed defaults; each is confirmed when its milestone starts.

| Question | Proposed | Consequence |
| --- | --- | --- |
| Run straight away, or always review first | **Always review first**, with Execute in the review panel. | One extra click per change; nothing runs unseen. |
| Where the DDL builder lives | **`dbrover-sql`**, beside the dialects. | Pure and testable without a database or the UI. |
| Users and roles in one dialog | **One dialog**: New user is New role with LOGIN ticked. | Matches PostgreSQL's model, and pgAdmin's single Login/Group Role dialog. |
| Passwords | **Hashed into a SCRAM verifier on the client**, redacted in the review panel, the history and the log. | Needs PostgreSQL 10 or later, which stores a verifier as it is whatever `password_encryption` says. Nothing older is supported, so the password is never sent as typed. |
| SQLite table rebuilds | **Last, in E4**, behind round-trip tests. | Until then, Modify table on SQLite offers only what `ALTER TABLE` does in place, and refuses the rest with the reason. |
| What Modify table can't do on a dialect | **Refused in the dialog with the reason**, never attempted. | The user takes the SQL to a console for anything else. |
| Renamed columns | **Tracked by the dialog's rows**, never guessed from names. | A rename keeps the column's data; it is never written as a drop and an add. |

## Risks

- **DDL breadth.** Every engine has many forms of `CREATE TABLE` and `ALTER TABLE`. The
  dialogs cover the common ones. Anything else goes through **Open in console**, so the user is
  never blocked.
- **Losing data in a rebuild.** SQLite's rebuild copies every row. It runs in one transaction
  and must recreate everything that pointed at the old table. E4 ships only once its tests
  show nothing is lost.
- **Dropping the wrong thing.** Typing the name for red (production) data sources,
  `CASCADE` spelled out in the confirmation, and the review panel all guard against it.
- **Passwords leaking.** The redaction has to apply everywhere a helper's statement is
  recorded: the history and the log; error messages name statements by position. A test creates a
  role with a password through `ApplyDdl` and then searches the history for the password
  and its verifier.
