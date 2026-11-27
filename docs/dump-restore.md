# Dump and restore

Status: **built on 2026-11-27**, in one stack, #719 and the layers above it; the manual's chapter
is [Dump and restore](../book/src/dump-restore.md), and the departures are in the roadmap's demo
note. Planned the same day (W17a), its nine open questions answered as proposed. The [roadmap](roadmap.md#well-rounded-track) tracks it as W17
in the well-rounded track. It is the third gap of the competitor review of 2026-11-25
([Comparison](comparison.md#gaps-worth-closing-soon)), after server activity (W15) and searching
data (W16). It starts with PostgreSQL, then MySQL and MariaDB, then SQLite and DuckDB, which need
no program at all.

Before a migration, someone wants a copy of the database they can go back to. A bug only shows
on production's data, and a developer wants that data on their laptop. A table was dropped by
mistake, and last night's dump has it. Today DbRover can't help with any of this: the answer is
a terminal, `pg_dump` or `mysqldump`, a host, a port, a user, a password typed again or kept in
`~/.pgpass`, TLS flags looked up in the manual, and an SSH tunnel opened by hand when the server
is behind a jump host. DbRover already knows all of that for every data source.

W17 adds **Dump…** and **Restore…**: pick a data source, a database, some schemas or tables,
and DbRover runs the engine's own program (`pg_dump`, `pg_restore` and `psql` for PostgreSQL;
`mysqldump` and `mysql` for MySQL; `mariadb-dump` and `mariadb` for MariaDB) with the data
source's settings, its password, its TLS and its SSH tunnel, never putting a secret on a command
line. A restore always asks first, and goes into a new database unless told otherwise. A dump can
be saved and run on a schedule.

All seven tools we compared have it ([Comparison](comparison.md), the *Dump and restore* row):
DBeaver Community, DataGrip, TablePlus and DBX free, Beekeeper Studio in its paid tiers, DbGate
and Tabularis. DBeaver, DataGrip and DbGate run the engines' own programs; TablePlus and DBX also
run dumps on a schedule.

## The question

Four needs come up, in this order of frequency:

1. **"Keep a copy before I change it."** A whole database, or a few tables, written to a file
   before a migration or a bulk update, and read back if it goes wrong.
2. **"Give me production's data locally."** A dump of one server restored into another,
   usually into a new database on a local server, without production's owners and grants.
3. **"Restore this file someone gave me."** A `.dump`, `.sql` or `.sql.gz` from a colleague, a
   hosting provider's backup, or a CI artefact, into a database.
4. **"Every night."** The same dump on a schedule, the last few kept.

What the others do, read for this plan:

- **DBeaver** runs `pg_dump`, `pg_restore`, `mysqldump` and `mysql` from a wizard: the objects,
  the format and options, the output folder and a file name pattern, then a log of the program's
  output. It looks for the programs in a *local client* setting per connection, which the user
  points at an installed client, and offers to download one on Windows.
- **DataGrip** runs the same programs from **Export with 'pg_dump'** and **Restore with
  'pg_restore'** on a database or schema, with the program's path and arguments editable in the
  dialog, and the output in its Run window.
- **TablePlus** replaced its Backup and Restore with **Sync Database**, to and from dump files,
  and runs it on a schedule with **Schedule Tasks**.
- **DBX** runs scheduled backups of MySQL and PostgreSQL to a folder, keeping a number of them.
- **DbGate** writes its own dumps in JavaScript since 7.2.4, without the engines' programs.

DbRover's version follows the shape DBeaver and DataGrip share (the engine's program, a dialog of
options, the output shown) and keeps to its own rules: the command that runs is shown, secrets
never reach a command line, a restore never writes over a database without asking, the data
source's safety level holds, and History records what ran.

## What it looks like

### Dump

Right-click a data source, a database, a schema or a table → **Dump…** opens a dialog:

```text
+---------------------------------------------------------------------------------------+
| Dump from production                                                                  |
|                                                                                       |
| What:    shop › public (38 tables)                                    [Change…]       |
|          (•) Schema and data  ( ) Schema only  ( ) Data only                          |
| Format:  [Custom (.dump), compressed, any table restorable on its own        v]       |
| Options: [ ] Leave out owners    [ ] Leave out privileges    Jobs: [1]                |
| To:      [~/Documents/DbRover dumps/production/shop-2026-11-27-1412.dump] [Browse…]   |
|                                                                                       |
| Program: pg_dump 17.2  /opt/homebrew/opt/libpq/bin/pg_dump              [Change…]     |
|          production is PostgreSQL 17.4                                                |
|                                                                                       |
| [Copy command]                         [Save as a scheduled dump…]  [Cancel] [Dump]   |
+---------------------------------------------------------------------------------------+
```

- **What** is what was right-clicked: a database (every schema DbRover shows, system schemas
  left out), a schema, or a table. **Change…** opens the catalog's schemas and tables with tick
  boxes and a filter, the scope picker W16's search uses, to dump some of them.
- **Schema and data**, **Schema only** or **Data only**.
- **Format**, the engine's own (see [Formats and options](#formats-and-options)), with what each
  is good for said beside it.
- **To** a file, named from a pattern, `{database}-{date}-{time}` by default, in the last folder
  used for that data source.
- **Program** is the one DbRover chose, its version, and the server's (see [Finding the
  programs](#finding-the-programs)). One that can't dump that server says so here, with how to
  install one that can, and **Dump** is disabled.
- **Copy command** copies the command as it will run, without the password, which never is in
  it: what a user would type in a terminal where `~/.pgpass` or `~/.my.cnf` has the password.

**Dump** closes the dialog and the dump runs in the background, in the status bar beside exports
and imports, with **Cancel** (see [Progress, Cancel and History](#progress-cancel-and-history)).

### Restore

Right-click a data source → **Restore…**, or a database → **Restore into this database…**, and
choose a file. DbRover reads what the file is before anything runs:

```text
+---------------------------------------------------------------------------------------+
| Restore into local                                                                    |
|                                                                                       |
| File:    ~/Downloads/shop-2026-11-27-1412.dump                        [Browse…]       |
|          PostgreSQL custom dump of "shop", 38 tables, 142 MB                          |
|          made by pg_dump 17.2 from PostgreSQL 17.4 on 2026-11-27 14:12                |
|                                                                                       |
| Into:    (•) A new database  [shop_restored                    ]                      |
|          ( ) An existing database [ shop                   v]                         |
| What:    Everything in the file (38 tables)                          [Change…]        |
| Options: [x] Leave out owners and privileges   [x] All or nothing (one transaction)   |
|          [ ] Drop each object first (existing database only)                          |
|                                                                                       |
| Program: pg_restore 17.2  /opt/homebrew/opt/libpq/bin/pg_restore                      |
|          local is PostgreSQL 17.4                                                     |
|                                                                                       |
| [Copy command]                                              [Cancel] [Restore…]       |
+---------------------------------------------------------------------------------------+
```

- **The file is read first:** its kind (a custom or directory archive, plain SQL, gzipped or
  not), its engine, and for an archive its table of contents, the version of the program that
  wrote it and of the server it came from. A file for another engine is refused at once: a
  PostgreSQL dump can't be restored into MySQL.
- **Into a new database** by default, named after the dumped one with `_restored`. DbRover
  creates it (`CREATE DATABASE`, as the explorer's object helpers do) before the program runs.
- **What** restores some tables only, picked from the archive's table of contents (custom and
  directory archives only: plain SQL is all or nothing).
- **Restore…** always asks, naming the file, the database and the data source (see [Restore
  safety](#restore-safety)).

The restore runs in the status bar too, with **Cancel**. When it ends, the explorer refreshes the
data source, so the new database shows.

### Saved and scheduled dumps

**Save as a scheduled dump…** in the Dump dialog keeps its settings under a name, with a
schedule: *every day at 02:00*, *every Monday at 07:00*, *every 6 hours*, or none (a saved dump
run by hand). **Settings › Dumps** lists them: each one's data source and scope, its schedule,
its last run and how it ended, its next run, and **Run now**, **Edit** and **Delete**. See
[Schedules](#schedules).

## Finding the programs

DbRover doesn't ship the programs (see [Options weighed](#ship-the-programs-or-find-them)). It
finds them the way a user would, and says which one it chose.

### Where it looks

In order, for each program name (`pg_dump`, `pg_restore`, `psql`, `mysqldump`, `mysql`,
`mariadb-dump`, `mariadb`, with `.exe` on Windows):

1. **The path set in Settings › Dumps › Programs**, if any, which always wins.
2. **DbRover's `PATH`.** On macOS an app opened from the Finder gets a short `PATH`
   (`/usr/bin:/bin:/usr/sbin:/sbin`), not the one a shell profile sets, which is why the places
   below are searched as well. The manual says so, as it does for password commands.
3. **The places installers put them:**

   | System | PostgreSQL | MySQL | MariaDB |
   | --- | --- | --- | --- |
   | macOS | Homebrew's `opt/libpq/bin` and `opt/postgresql@*/bin` (under `/opt/homebrew` and `/usr/local`), Postgres.app's `/Applications/Postgres.app/Contents/Versions/*/bin`, EDB's `/Library/PostgreSQL/*/bin` | Homebrew's `opt/mysql-client/bin`, `opt/mysql/bin`, `opt/mysql@*/bin`, `/usr/local/mysql/bin` | Homebrew's `opt/mariadb/bin`, `opt/mariadb@*/bin` |
   | Linux | `/usr/lib/postgresql/*/bin` (Debian and Ubuntu, one folder per version), `/usr/pgsql-*/bin` (the PGDG packages for Red Hat and Fedora) | `/usr/bin` | `/usr/bin` |
   | Windows | `C:\Program Files\PostgreSQL\*\bin` | `C:\Program Files\MySQL\MySQL Server *\bin` | `C:\Program Files\MariaDB *\bin` |

Every copy found is asked its version (`pg_dump --version` prints `pg_dump (PostgreSQL) 17.2`;
`mysqldump --version` prints `mysqldump  Ver 8.4.3 for macos15 on arm64`, and MariaDB's
`mariadb-dump from 11.4.4-MariaDB`), once per run of DbRover and again when its file changes.
**Settings › Dumps › Programs** lists every copy found, its version and its path, which one each
engine uses, and a path field to set one by hand.

### Which one, for which server

The server's version is already known: every connection reports it (`ServerInfo::version`).

- **PostgreSQL:** `pg_dump` refuses a server newer than itself (*aborting because of server
  version mismatch*), and dumps older ones back to 9.2. So DbRover picks the copy whose major
  version is the server's, or failing that the newest one at least as new. `pg_restore` can't
  read an archive made by a newer `pg_dump` (*unsupported version in file header*), so for a
  restore it picks one at least as new as the archive's maker, which the file's header says.
  `psql` reads any plain dump, but a plain dump made for a newer server can hold statements an
  older one refuses (for example a `SET` of a setting it doesn't have); the dialog says when the
  file's server is newer than the target.
- **MySQL and MariaDB:** each family's own client, decided by the server (`ServerInfo::product`):
  `mysqldump` for MySQL, `mariadb-dump` for MariaDB. Across families they half work: MySQL 8's
  `mysqldump` reads `information_schema.COLUMN_STATISTICS`, which MariaDB doesn't have, and
  MariaDB's client sends options MySQL's server rejects. A client older than the server's major
  version is used with a warning, not refused, since `mysqldump` keeps working across them more
  often than not.
- **None fits:** the dialog says what it found and what it needs, with the commands that
  install it (`brew install libpq`, `apt install postgresql-client-17`, the EDB or MySQL
  installer on Windows). Nothing runs.

### Trust

The programs DbRover finds in the places above run without asking: DbRover chose them, and the
dialog shows which. A path typed in **Settings › Dumps › Programs** is kept in `settings.toml`,
which can come from a synced or copied settings folder, so it asks once before its first run and
is trusted by its exact path in `trusted_commands.toml`, in the data folder, as command actions
are ([Hackability](hackability.md)). A changed path asks again.

## Running a program for a data source

A dump or restore runs on the machinery the password command (W9) and command actions (H2)
already use (`dbrover-engine`'s `ProgramRunner` and `ProcessRunner`): the program started
directly, never through a shell, its arguments a list, its console window hidden on Windows
(`CREATE_NO_WINDOW`), killed if its run is dropped (`kill_on_drop`), and a missing program
reported with the `PATH` it was looked for in. What a dump adds:

- **Its output streams to a file**, not into memory: `ProcessRunner::run` keeps 4 MiB of stdout,
  which a dump passes at once. The export programs of W8d already start a program whose stdout
  goes to a file (`start_export`); a dump does the same, or names the file itself (`--file`,
  `--result-file`).
- **Its input streams from a file** for a plain restore, read in chunks, gunzipped on the way
  when the file is `.gz`, so a restore's progress is the bytes sent of the file's size.
- **No time limit.** A password command gets 60 s; a dump of a large database takes hours. It
  runs until it ends, is cancelled, or DbRover quits.
- **Its stderr is read line by line**, for progress and the log, not only kept for an error.
- **Its children too:** a parallel `pg_dump` or `pg_restore` (`--jobs`) starts worker processes on
  Unix, so the program is started in a process group of its own and Cancel stops the group; on
  Windows they are threads of one process. The machine is kept awake after two minutes, as long
  report runs are (`KeepAwake`).

### Secrets never on the command line

A process's arguments can be read by any user on the machine (`ps`), and end up in shell
histories and logs. The password is never in them, nor in an environment variable:

| Engine | How the password reaches the program |
| --- | --- |
| PostgreSQL | a password file, `PGPASSFILE` pointing at it, one line `host:port:database:user:password` with `:` and `\` escaped as libpq reads them |
| MySQL, MariaDB | an option file, `--defaults-file=` as the first argument, holding `[client]` `password=…` |

- **The file is DbRover's:** created with only the user able to read it (`0600` on macOS and
  Linux, where libpq refuses a password file anyone else can read; on Windows in the user's own
  temporary folder, which only they can read), in a folder of DbRover's in the data folder,
  removed as soon as the program has connected or ended, and any left behind by a crash removed
  at the next start.
- **Not `PGPASSWORD` or `MYSQL_PWD`:** an environment variable is inherited by every child the
  program starts and readable by the same user in `/proc` on Linux; PostgreSQL's manual
  advises against `PGPASSWORD`, and MySQL deprecated `MYSQL_PWD` in 8.0.
- **Not a pipe:** libpq refuses a password file that isn't a plain file, so `/dev/fd/3` doesn't
  work for PostgreSQL, and Windows has no such path.
- **Where the password comes from** is where connecting gets it: the keychain, the session's
  password, the data source's password command (W9), or its cloud sign-in token (an RDS IAM
  token, a Cloud SQL or an Entra ID token). One that isn't saved is asked for first, as
  connecting asks.
- **The user's own settings are left out:** the program gets DbRover's environment without
  `PG*` and `MYSQL_*` variables (a `PGPASSWORD`, `PGSERVICE` or `PGSSLMODE` left in a profile
  would otherwise win over the data source's settings), and `--defaults-file` keeps the MySQL
  clients from reading `~/.my.cnf`. What runs is the data source's settings and nothing else.
- **Never asked on a terminal:** `pg_dump --no-password` fails at once rather than waiting for a
  password on a terminal a GUI app doesn't have.

### Connection settings

The rest of the connection is on the command line, in full, so **Copy command** gives a command
that works in a terminal:

- **PostgreSQL** takes a connection string as `--dbname`:
  `host=db.example.com port=5432 user=ada dbname=shop sslmode=verify-full
  sslrootcert=/path/ca.pem application_name='DbRover dump'`. The data source's TLS mode maps to
  `sslmode` (`disable`, `prefer`, `require`, `verify-full`); a CA file to `sslrootcert`, and none
  with **verify-full** to `sslrootcert=system` (the system's authorities, libpq 16 and later; an
  older program is told to use a CA file).
- **MySQL** takes `--host`, `--port`, `--user`, `--protocol=TCP` (without it, `localhost` means the
  Unix socket, which DbRover's own connection never uses) and `--ssl-mode` (`DISABLED`,
  `PREFERRED`, `REQUIRED`, `VERIFY_IDENTITY`) with `--ssl-ca`. **MariaDB**'s client spells TLS its
  own way (`--ssl`, `--skip-ssl`, `--ssl-verify-server-cert`, `--ssl-ca`), and verifies the
  server's certificate by default since 11.4.
- **An SSH tunnel** is the one the data source's connections share (`Tunnels`), which listens on a
  port of `127.0.0.1`: the program connects there, and the tunnel is held open until it ends.
  PostgreSQL keeps checking the real server's name through it: `host=db.internal
  hostaddr=127.0.0.1 port=<the tunnel's>`, so `verify-full` checks the certificate against
  `db.internal`. The MySQL clients have no such split, so through a tunnel **verify-full**
  becomes `VERIFY_CA` (the certificate's authority checked, not its name), which the dialog says;
  the tunnel itself has checked the SSH server's key.
- **A cloud sign-in** that needs more than a password says so: RDS IAM on MySQL needs
  `--enable-cleartext-plugin` and TLS, added by DbRover when the data source signs in that way.

### What is recorded about a run

The log console shows the command (as **Copy command** gives it), then the program's stderr as
it comes. `tracing` logs the program's name at `info` and its arguments at `debug`, as program
runs do; neither ever holds the password, which is only in the file.

## Formats and options

### PostgreSQL

| Format | Made by | Restored by | Good for |
| --- | --- | --- | --- |
| **Custom** (`.dump`), the default | `pg_dump --format=custom` | `pg_restore` | One compressed file; any table restorable on its own; parallel restore |
| **Directory** (a folder) | `pg_dump --format=directory` | `pg_restore` | As custom, and a parallel dump with **Jobs** |
| **Plain SQL** (`.sql`, `.sql.gz`) | `pg_dump --format=plain` | `psql` | Reading, editing, diffing, running anywhere; all or nothing |

- **Tar** is left out: it is the custom format's features without its compression, and nothing
  reads it that doesn't read custom.
- **Schema only** is `--schema-only`, **Data only** `--data-only`.
- **Chosen schemas and tables** are `--schema` and `--table` patterns, each name double-quoted so
  that `*`, `?`, `.` and capitals in it are matched as themselves, with `--strict-names`, so a
  table dropped since it was picked fails the dump rather than being left out unseen. Many tables
  go in a filter file (`--filter`, pg_dump 17) where the program has it, so a long list doesn't
  meet Windows' 32,767-character command line.
- **Leave out owners** is `--no-owner`, **leave out privileges** `--no-privileges`. Both are off
  for a dump, which keeps everything, and on for a restore by default (see below).
- **Jobs** is `--jobs` for a directory dump and a custom or directory restore; 1 by default, since
  each job is another connection reading a whole table at once from a server someone else may be
  using.
- **Compression** is the format's default (gzip level 6 for custom and directory); the level and
  `zstd` or `lz4` (pg_dump 16) are in **More options**. Plain SQL is gzipped by DbRover when the
  file name ends in `.gz`, so both engines' plain dumps are compressed the same way.
- **Restore** uses `--exit-on-error` (`pg_restore` otherwise goes on and reports *errors
  ignored on restore* at the end) and `-v ON_ERROR_STOP=1` for `psql`; **All or nothing** is
  `--single-transaction` for both, which `pg_restore` can't combine with **Jobs**. **Drop each
  object first** is `--clean --if-exists`, offered only into an existing database.
- **Chosen tables on restore** are read from the archive's table of contents (`pg_restore
  --list`), and restored through a list file (`--use-list`) holding the schemas, types and
  functions too, so a restored table's columns have their types.

### MySQL and MariaDB

One format, plain SQL (`.sql`, `.sql.gz`), made by `mysqldump` or `mariadb-dump` and restored by
`mysql` or `mariadb` reading it on stdin.

- **Always:** `--single-transaction` (a consistent read of InnoDB tables without locking them),
  `--routines`, `--triggers`, `--events`, `--hex-blob`, `--default-character-set=utf8mb4`, and
  one database without `--databases`, so the file has no `CREATE DATABASE` or `USE` and restores
  into a database of any name.
- **On MySQL:** `--set-gtid-purged=OFF`, so a dump of a server with GTIDs on (RDS among them)
  restores on one without, by a login without `SUPER`; and `--column-statistics=0` for MySQL 8's
  `mysqldump` against a 5.7 server.
- **Schema only** is `--no-data`, **Data only** `--no-create-info --skip-triggers`.
- **Chosen tables** follow the database's name on the command line. MySQL has no schemas inside
  a database, so the scope is a database or its tables.
- **All or nothing** isn't offered: MySQL commits each `CREATE` and `DROP` as it runs it, so a
  restore that fails half way leaves what it did. The dialog says so, and a new database is
  what makes a restore safe to retry.
- **Owners on restore:** views, routines, triggers and events carry a `DEFINER` naming the
  account that made them, which a restore by another account fails on without `SET_USER_ID` or
  `SUPER`. **Leave out owners** removes the `DEFINER` clauses from the SQL as DbRover sends it to
  the client, by a rule golden tested on both families' output. (Question 4.)

### SQLite and DuckDB

Both run inside DbRover, so no program is needed:

- **SQLite:** a dump is a consistent copy of the file, made by SQLite's online backup API
  (rusqlite's `backup`), while the database stays open; a restore copies a dump into a new file
  and offers to add it as a data source. Writing over an open SQLite file isn't offered.
- **DuckDB:** `EXPORT DATABASE '<folder>' (FORMAT parquet)` (or CSV) writes the schema, a load
  script and one file per table; a restore is `IMPORT DATABASE` into a new DuckDB file. An
  attached database (RS7) is dumped as its own.

## Restore safety

A restore writes to a database, often all of it. The data source's safety level (W2) holds, and
a restore asks every time:

- **Read-only data sources** can't be restored into: **Restore…** is shown disabled, with the
  reason. They can be dumped: a dump only reads.
- **Every restore asks**, on every data source, naming the file, the database and the data
  source: *Restore shop-2026-11-27-1412.dump (38 tables) into a new database "shop_restored" on
  "local"?*
- **Into a new database** is the default, and the safe case: nothing that was there can be lost.
  A name already taken is refused before asking.
- **Into an existing database** asks with what it holds: *Restore … into "shop" on "staging",
  which has 38 tables? Objects that already exist will make the restore fail.* With **Drop each
  object first**: *… Every object in the file is dropped first, with its data.*
- **On a careful data source**, restoring into an existing database asks the user to type the
  database's name, as GitHub asks for a repository's before deleting it. (Question 5.)
- **A dump doesn't ask**, careful or not: it only reads, and the user has just chosen it in a
  dialog. It is the one thing the manual says to do before anything risky.
- **The MCP login**, where a data source has one, isn't used: a dump or restore runs as the
  user, as the explorer browses.
- **Leave out owners and privileges** is ticked by default on restore: the common case is
  production's data on a server without production's roles, where `ALTER … OWNER TO` and
  `GRANT` fail. Unticked, the archive's owners and grants are restored too.

## Progress, Cancel and History

- **In the status bar,** beside exports and imports (`BackgroundRuns`): *Dumping shop from
  production: 12 of 38 tables, 142 MB, 0:41*, and **Cancel**. A dump's table count comes from
  `pg_dump --verbose`'s *dumping contents of table* lines and `mysqldump --verbose`'s, against the
  number of tables in the scope; its size from the file as it grows. A restore's progress is the
  bytes of the file sent (plain SQL), or `pg_restore --verbose`'s *processing data for table*
  lines against the archive's table of contents.
- **The log console** shows the program's lines as they come, and **Show log** in the status bar
  opens it on them.
- **Cancel** stops the program (and its workers). A dump is written to a name of its own
  (`shop-….dump.partial`) and renamed when it ends well, so a cancelled or failed dump leaves no
  file that looks whole; the partial file is removed. A cancelled restore says what is left: with
  **All or nothing**, nothing; otherwise the tables restored so far, and, into a new database,
  **Drop the new database**.
- **When it ends:** the status bar says how (*Dumped shop to shop-2026-11-27-1412.dump, 142 MB in
  0:58*, with **Show in folder**), or why it failed with the program's last lines of stderr,
  which say far more than its exit code.
- **History** records each run **once**, with new origins **Dump** and **Restore**: when, how
  long, how it ended, and in `detail` the file, its size, the scope and the format. Its statement
  is the command, as **Copy command** gives it: no password is ever in it. **Run again** on such an
  entry opens the dialog with the same settings, rather than running it. Scheduled runs are
  recorded the same way, marked as the schedule's. (Question 6.)

## Schedules

A saved dump is a dump's settings under a name, kept in **`dumps.toml`** in the settings folder,
beside `exports.toml` and `actions.toml`, so it can be read, edited and kept in Git:

```toml
[[dump]]
id = "nightly-shop"
name = "Nightly shop"
data_source = "6f1c…"          # by id, as projects name them
database = "shop"
schemas = ["public"]           # or tables = […]; none for the whole database
content = "schema-and-data"    # or "schema-only", "data-only"
format = "custom"
folder = "~/Documents/DbRover dumps/production"
file = "{database}-{date}-{time}"
keep = 7
schedule = { every = "day", at = "02:00" }
```

- **Runs only while DbRover is open.** A schedule is checked every minute by the engine. A run
  missed while DbRover was closed or the machine asleep runs **once** at the next start, a minute
  after the window opens, if the last run that worked is older than the schedule's interval; more
  missed runs never run twice. A run still going when the next is due skips that one.
- **When DbRover isn't open**, the operating system's scheduler runs `dbrover dump run
  nightly-shop`, as `dbrover report run` runs reports for cron and CI (RP6): the same saved
  dump, the same history, an exit code per outcome. The manual gives a `crontab` line, a
  `launchd` agent and a Task Scheduler task. DbRover never installs one itself. (Question 7.)
- **Where they go:** the saved dump's `folder`, by default `Documents/DbRover dumps/<data
  source>` in the user's home, one file per run named from the pattern, never overwriting one.
- **Keeping the last few:** `keep = 7` removes the oldest runs' files past seven, after a run
  that worked, and only files that schedule wrote, as its runs in the history list them, never
  another file in the folder. `keep = 0` keeps every one.
- **On or off per machine:** whether a schedule runs is kept in the data folder, not in
  `dumps.toml`. A settings folder synced to a second machine brings the saved dumps without
  running each one twice; turning a schedule on there is a click in **Settings › Dumps**.
- **A failed scheduled run** shows in the status bar until dismissed, marks the saved dump in
  **Settings › Dumps**, and is in History. A data source whose password isn't saved can't run
  unattended: its schedule asks for it at the first run and keeps it for the session, and
  `dbrover dump run` fails with that reason.

## MCP

None in W17. A dump writes a whole database to the user's disk, and a restore writes a whole
database to a server; neither is a step an assistant needs to take for the user, and `query`
already reads what an assistant is allowed to. A later milestone can add running a **saved**
dump by its name, behind `admin`, if someone asks. (Question 8.)

## Engines

### First: PostgreSQL, MySQL and MariaDB

| Engine | Dump | Restore | Notes |
| --- | --- | --- | --- |
| PostgreSQL | `pg_dump` | `pg_restore`, `psql` | RDS, Aurora, Cloud SQL, Azure, Neon and Supabase alike |
| MySQL | `mysqldump` | `mysql` | RDS, Aurora and Azure alike; GTIDs left out |
| MariaDB | `mariadb-dump` | `mariadb` | `mysqldump` and `mysql` where an older MariaDB installs those names |

- **Managed services:** their servers are PostgreSQL's and MySQL's, so the programs work. Each
  has a note in the manual: Neon's pooled host runs each statement on any connection, so a dump
  goes to its direct host; Supabase's databases hold its own schemas (`auth`, `storage`,
  `realtime`), which only restore into Supabase, so a dump of `public` is the usual choice;
  RDS's master user isn't a superuser, so **Leave out owners and privileges** matters there.
- **Behind PgBouncer** in transaction mode a dump can fail: `pg_dump` needs its session for the
  whole run. DbRover can't tell it is behind one (found in W15), so the manual says to use the
  server's own port.

### Then: SQLite and DuckDB

No program, so nothing to find, and tested on every CI runner, macOS and Windows included. A
layer of their own, after the program-based engines, since their dialogs reuse what those build.

### Later, each with its needs recorded

- **SQL Server:** `BACKUP DATABASE … TO DISK` writes a `.bak` on the **server's** disk, not the
  user's, and Azure SQL Database refuses it outright. The client-side tool is `sqlpackage`
  (`/Action:Export` to a `.bacpac`, schema and data; `/Action:Import` back), a separate download
  on every system, slow on large tables, and the one that works with Azure SQL. A layer of its
  own, finding `sqlpackage` as above, after W17; `BACKUP` to a server path a following step for
  self-hosted servers.
- **MongoDB:** `mongodump` and `mongorestore` take a connection string and fit this machinery
  closely (the password in a config file, `--config`). The natural next engine.
- **ClickHouse:** `BACKUP DATABASE … TO Disk(…)` or `S3(…)` runs on the server, to the server's
  disks or a bucket; `clickhouse-client` can read tables out as `Native`, one at a time. Neither is
  a dump to the user's file in the sense above; left for later.
- **Redis and Valkey:** `BGSAVE` writes the server's RDB file on the server. **Cassandra and
  ScyllaDB:** `nodetool snapshot` per node. **Elasticsearch and OpenSearch:** snapshot
  repositories on the cluster. All server administration, left out ([Vision](vision.md)'s
  non-goals).

### Left out: billed and cloud engines

BigQuery, Snowflake, Databricks, Athena, Trino, Redshift, Synapse, Fabric and DynamoDB have no
dump program of this kind: each exports to object storage with its own statement (`EXPORT DATA`,
`COPY INTO <location>`, `UNLOAD`), billed as a scan of what it reads. **Redshift** speaks
PostgreSQL's protocol, but `pg_dump` fails on its catalog; `ServerInfo::service` turns **Dump…**
off there, as it turns off Server activity and Search data, and C7 (#693), which is about
Redshift, is where an `UNLOAD` handoff belongs. Synapse and Fabric wait with SQL Server.

## How it's built

- **`dbrover-core`:** the model: `DumpRequest` (data source, database, `DumpScope`,
  `DumpContent::{SchemaAndData, SchemaOnly, DataOnly}`, `DumpFormat`, options, file),
  `RestoreRequest` (file, `RestoreTarget::{NewDatabase, Existing}`, chosen entries, options),
  `DumpFile` (what a file was found to be), `ProgramVersion`, and `SavedDump` with its `Schedule`.
  Pure: when a schedule is next due, given the last run and the clock.
- **`dbrover-dump`, a new crate:** what knows the programs, pure and golden tested: the places to
  look per system, reading `--version` output, choosing a program for a server's version, the
  arguments for each engine, format and option (the PostgreSQL connection string and its
  escaping, the pattern quoting, MySQL's options and the MariaDB spellings), the password and
  option file's contents and escaping, reading a file's kind and an archive's table of contents
  (`pg_restore --list` output), the `DEFINER` filter, and the progress read from each program's
  verbose lines. No I/O but reading files it is given, so it is tested with fixtures of each
  program's real output, as `dbrover-export` is tested on its formats.
- **`dbrover-engine`:** `Command::StartDump`, `StartRestore`, `CancelBackground` (the run's id),
  answered by `Event::DumpProgress` and `Event::DumpEnded`; running the program with its secret
  file, tunnel and process group; the `.partial` file and its rename; `CREATE DATABASE` before a
  restore; the history entries; the schedule's clock; finding programs on the blocking pool and
  caching what was found. SQLite's backup and DuckDB's `EXPORT DATABASE` run on the session, as a
  statement does.
- **`dbrover-store`:** `HistorySource::Dump { file }` and `Restore { file }`, origins `dump` and
  `restore`; `dumps.toml` (`SavedDumpsFile`, with `format_version = 1`); `[dumps]` in
  `settings.toml` for program paths and the default folder; which schedules are on, in the data
  folder; the program paths' trust, in `trusted_commands.toml`.
- **`dbrover-app`:** the **Dump…** and **Restore…** dialogs (D8's dialog style), the scope picker
  shared with W16's search, the confirmations (`hazard_dialog`), the runs in `BackgroundRuns`,
  **Settings › Dumps** (programs and saved dumps), the explorer's menu items, and `dbrover dump
  run` beside `dbrover report run`.

## Options weighed

### Ship the programs, or find them

Shipping `pg_dump`, `pg_restore` and `psql` with DbRover would mean one copy per system and
architecture, with libpq and OpenSSL, in every release, and a new release for every PostgreSQL
major version, since an old `pg_dump` can't dump a new server. The MySQL clients are GPL, which
DbRover's MIT and Apache licences would ship beside but not under. Every user who dumps databases
from a terminal has them already; the others install them with one command, which the dialog
gives. Finding them is also what DBeaver and DataGrip do.

### The engines' programs, or DbRover's own dump

DbGate and Beekeeper write their own dumps from queries. That needs no program, but every
engine's DDL, sequences, extensions, large objects, triggers, routines and their order would have
to be written again, and kept right as engines change: the very thing `pg_dump` and `mysqldump`
are maintained for. A dump DbRover wrote would also be restorable only by DbRover. The engines'
programs write files every other tool, script and hosting provider reads.

### A temporary password file, or the environment

`PGPASSWORD` and `MYSQL_PWD` are one line each, but they reach every process the program starts,
can be read in `/proc/<pid>/environ` by the same user on Linux, and MySQL has deprecated its
variable. A file only the user can read, removed once the program has connected, costs a few
lines and works the same on both engines. A pipe (`/dev/fd/N`) would avoid the disk but libpq
refuses it, and Windows has none.

### Schedules in DbRover, or only in the operating system's scheduler

Running only through cron, `launchd` or Task Scheduler would follow RP6 exactly, and would run
with DbRover closed, but asks every user to write a crontab line for what other tools offer as a
dropdown. Installing scheduler entries for the user would mean writing to `launchd`'s and Task
Scheduler's configuration, and keychain prompts from a process nobody is watching. So schedules
run inside DbRover while it is open, which covers a laptop used every day, and `dbrover dump run`
is there for a machine that must dump at night with DbRover closed, set up by the user who wants
that.

### A tab, or dialogs and the status bar

A dump is set up once and then runs on its own, as an export does: a dialog to set it up, the
status bar while it runs, and History after. The saved dumps are settings, and live in the
Settings tab (W14). A tab of its own would hold nothing between runs.

## Layers

1. **W17a — The plan:** this design, and the roadmap's entry.
2. **W17b — Finding the programs:** the core model, the `dbrover-dump` crate's search places,
   version reading and choice, the arguments for PostgreSQL, MySQL and MariaDB, the password and
   option files, golden tested: a name with `"`, `*` and capitals; a password with `:`, `\`, `#`
   and a newline; a TLS mode of each kind; a tunnel's `hostaddr`; a pg_dump older than the
   server refused; MariaDB's client chosen for MariaDB. Discovery tested on a fake tree of
   install folders per system.
3. **W17c — PostgreSQL dump:** the engine's run (secret file, tunnel, process group, `.partial`
   and rename, progress from `--verbose`, Cancel, `HistorySource::Dump`), with tests against the
   compose PostgreSQL: a dump in each format, schema only, chosen tables, through the SSH test
   server, with TLS, cancelled half way (no file left), and a password with every awkward
   character.
4. **W17d — PostgreSQL restore:** reading a file's kind and an archive's table of contents,
   `CREATE DATABASE`, `pg_restore` and `psql`, all or nothing, chosen tables, owners left out,
   `HistorySource::Restore`; tests: each format restored into a new database and compared
   with the original table by table, a failing restore leaving nothing with **All or nothing**,
   a newer archive refused by an older `pg_restore`.
5. **W17e — The dialogs:** **Dump…** and **Restore…** in the explorer's menus, the scope picker,
   the program line, **Copy command**, the confirmations, the runs in the status bar, and
   **Settings › Dumps › Programs**. Kittest tests: a read-only data source's Restore disabled with
   its reason, the careful question with the typed name, a missing program's install commands,
   a dump's progress and Cancel.
6. **W17f — MySQL and MariaDB:** `mysqldump`, `mariadb-dump` and their clients, the `DEFINER`
   filter, through the same tests on the compose MySQL and MariaDB.
7. **W17g — SQLite and DuckDB:** the backup API and `EXPORT DATABASE`, restored into new files,
   tested on every runner.
8. **W17h — Saved and scheduled dumps:** `dumps.toml`, **Settings › Dumps**, the schedule's clock
   and catch-up, keeping the last few, per-machine on and off, and `dbrover dump run`. Tests of
   when a schedule is due (pure, with a clock), of `keep` removing only its own files, and of the
   command's exit codes.
9. **W17i — Manual and demo note:** a chapter, *Dump and restore*, in `book/src`, with pictures
   from new scenes (`dump-dialog`, `restore-dialog`, `settings-dumps`) on the PostgreSQL test
   server; the explorer's menus in [Data sources](../book/src/data-sources.md); History's new
   origins; the scheduler examples; and the exit as an engine test, `w17_exit`.

Each layer compiles and passes on its own.

**Testing and CI.** The server tests run on the Linux runner, against the containers
`docker-compose.yml` already starts, so no service is added. They need the client programs, of
the servers' versions (PostgreSQL 17, MySQL 8.4, MariaDB 11.4). The proposal (question 9) is to
take them from the server images CI has already pulled: small scripts in `docker/clients/`
(`pg_dump`, `pg_restore`, `psql`, `mysqldump`, `mysql`, `mariadb-dump`, `mariadb`) run the
program in its server's image with `docker run --rm --network host`, the run's temporary folder
mounted at the same path, so the program reads and writes the files DbRover names. The tests find
them through `DBROVER_TEST_CLIENTS`, and skip, as the server tests do, when it isn't set. That
installs nothing and costs a container's start per program run, about a second, so the dump and
restore tests should add about a minute to the Linux job, to be measured on W17c's pull request
and said there. The other way, installing `postgresql-client-17` from PostgreSQL's apt repository
and MySQL's and MariaDB's clients (which conflict as Ubuntu packages), would cost an `apt-get`
of 30 s to a minute on every run, and the runner image's own PostgreSQL client (16, *to check*)
already shows the version refusal. Discovery and the arguments are unit tests and run on every
runner; SQLite and DuckDB need nothing.

**Exit:** on the PostgreSQL test server, **Dump…** on a database writes a custom dump, through
the program DbRover found and named, with no password on its command line or in its environment;
**Restore…** of that file into a new database asks first, naming it, and gives the same tables
and rows; a plain dump restored **All or nothing** with an error in it leaves the new database
empty; a cancelled dump leaves no file; restoring into a read-only data source is refused, and
into an existing database of a careful one asks for its name; a `pg_dump` older than the server
is refused with how to install one that fits; History lists the dump and the restore once each;
a saved dump due while DbRover was closed runs once at the next start.

## Decisions

The nine open questions below were answered on 2026-11-27, all as proposed; the rest are
confirmed as their layers are built.

| Question | Proposed | Consequence |
| --- | --- | --- |
| The engines | **PostgreSQL, MySQL, MariaDB**, then SQLite and DuckDB in-process; SQL Server (`sqlpackage`), MongoDB and ClickHouse later; billed and cloud engines out | The programs most users have first; nothing that writes to a server's own disk. |
| The programs | **Found, not shipped**: the settings' path, `PATH`, then known install places; chosen by the server's version | No libpq or GPL binaries in DbRover's releases; a missing or old program said with how to install one. |
| Secrets | **A file only the user can read, removed after**; never the command line or environment; the user's `PG*` variables and `~/.my.cnf` ignored | `ps` and `/proc` show nothing secret; what runs is the data source's settings. |
| Connection | **The data source's own TLS and SSH tunnel**, PostgreSQL checking the real host through the tunnel | A dump reaches every server DbRover reaches, the same way. |
| Formats | **PostgreSQL custom by default, directory and plain SQL; MySQL plain SQL**; `.gz` by DbRover; tar left out | One file that restores any table by default; SQL where it is wanted. |
| Restore safety | **Read-only refused; every restore asks; new database by default; a careful existing database asks for its name**; dumps never ask | No database is written over by a click. |
| Owners on restore | **Left out by default**, `DEFINER` filtered on MySQL | Production's data restores on a server without production's roles. |
| Progress | **The status bar, the log console, Cancel, a `.partial` file** | A long dump doesn't hold the app; a cancelled one leaves no half file. |
| History | **One entry per run, origins Dump and Restore**, the command as the statement | What ran, and where it went, without a password. |
| Schedules | **While DbRover is open, one catch-up run; `dbrover dump run` for the OS scheduler** | Nightly dumps on a laptop with no setup; a server's cron when needed. |
| MCP | **None in W17** | No assistant writes a whole database anywhere. |

## Risks

- **The wrong program.** A `pg_dump` older than the server, MySQL's client against MariaDB, a
  Debian wrapper at `/usr/bin/pg_dump` that picks a version by cluster. Every copy's version is
  read before it is chosen, the choice is shown, and the refusals are tested.
- **A secret left on disk.** The password file lives for the program's start, in a folder only the
  user can read, and is removed at the next start if a crash left it. Tested by a dump whose
  password is searched for afterwards in the folder, the log, the history and the program's
  arguments, as W1's export test searches for every saved password.
- **A restore that destroys data.** New database by default, a question every time, the typed
  name on careful data sources, read-only refused, **Drop each object first** off by default and
  only into an existing database.
- **A half-written dump taken for a whole one.** The `.partial` name until the program has ended
  well, and History saying how each run ended.
- **Load on production.** A dump reads every table. One job by default, and the manual says a
  dump of a large database is a long read, best from a replica.
- **Schedules that silently stop.** A failed run stays in the status bar and on the saved dump
  until seen; a missed one runs at the next start; a schedule on a machine that is never open
  is what `dbrover dump run` and the manual's scheduler examples are for.
- **Version drift in program output.** Progress is read from verbose lines, which change between
  versions. Unknown lines are only logged, so progress falls back to bytes and time, and never
  fails a run.
- **Windows.** Paths with spaces, `.exe` names, the 32,767-character command line (the filter file),
  processes without a console. The arguments are tested on Windows paths; the programs themselves
  are tried by hand there, since CI's Windows runner has no servers.

## What stays out

- **Dumps to object storage** (S3, GCS, Azure Blob): a dump goes to a file; a user's own sync
  tool can take it from there.
- **Encrypting dumps.** The file is as safe as the folder it is in; `pg_dump` has no encryption,
  and adding DbRover's own would make the file unreadable to every other tool.
- **Copying between servers in one step** (`pg_dump | psql` with no file between): a dump, then a
  restore, keeps a file to retry from. TablePlus's *Sync Database* and row data compare (gap 6)
  are other milestones.
- **Point-in-time recovery, replication, physical backups** (`pg_basebackup`, `xtrabackup`):
  server administration ([Vision](vision.md)).
- **Restoring over an open SQLite or DuckDB file.**
- **Editing the program's arguments by hand** in the dialog: the options are DbRover's, and the
  command is copied to a terminal for anything else.

## Open questions

Answered on 2026-11-27: all nine as proposed. Kept as they were asked.

1. **Engines.** PostgreSQL, MySQL and MariaDB first, then SQLite and DuckDB (no program needed)
   in W17g, with SQL Server (`sqlpackage` `.bacpac`), MongoDB (`mongodump`) and ClickHouse left
   for a following milestone and the billed and cloud engines left out. Should SQLite and DuckDB
   wait too, which drops W17g, or should SQL Server's `sqlpackage` come into W17?
2. **Finding the programs.** Search the path set in Settings, then `PATH`, then the known install
   places, and pick by the server's version, refusing a `pg_dump` older than the server with how
   to install one; never ship the programs. Or should DbRover offer to download PostgreSQL's
   client tools for the user, as DBeaver does on Windows?
3. **Secrets and connections.** The password in a file only the user can read
   (`PGPASSFILE`, `--defaults-file`), removed once the program has connected; never on the
   command line or in `PGPASSWORD`/`MYSQL_PWD`; the user's own `PG*` variables and `~/.my.cnf`
   ignored; TLS and the SSH tunnel taken from the data source, with MySQL through a tunnel
   checking the certificate's authority but not its name. Agreed?
4. **Formats and options.** PostgreSQL custom by default, directory and plain SQL offered, tar
   left out; MySQL and MariaDB plain SQL; `.sql.gz` gzipped by DbRover; schema only, data only,
   chosen schemas and tables; one job by default. On restore, owners and privileges left out by
   default, and on MySQL the `DEFINER` clauses removed from the SQL as it is sent. Is filtering
   `DEFINER` acceptable, or should a restore that hits one fail with the server's message?
5. **Restore safety.** Restoring into a read-only data source refused; every restore asks,
   naming the file, database and data source; a new database by default; into an existing
   database on a careful data source, the database's name typed; **Drop each object first** off
   by default. Dumps never ask, even on a careful data source. Is the typed name right for
   careful, or should every restore into an existing database ask for it?
6. **Progress and History.** Runs in the status bar with Cancel, as exports, the program's lines
   in the log console, a `.partial` file renamed when it ends well; one History entry per run,
   origins **Dump** and **Restore**, the command (without password) as the statement, and **Run
   again** opening the dialog rather than running. Agreed?
7. **Schedules.** Run by DbRover only while it is open, with one catch-up run at the next start;
   `dbrover dump run <saved dump>` for cron, `launchd` or Task Scheduler, set up by the user, which
   DbRover never installs; saved dumps in `dumps.toml` in the settings folder, on or off per
   machine in the data folder; by default into `Documents/DbRover dumps/<data source>`, keeping
   the newest 7. Should DbRover install the operating system's scheduler entry itself instead,
   so a schedule runs with DbRover closed?
8. **MCP.** No MCP tool in W17; running a saved dump by name behind `admin` later if asked. Or
   should W17 add it?
9. **CI.** Test against the compose servers with their images' own clients through
   `docker run` scripts (nothing installed, about a minute added to the Linux job), rather than
   installing the three clients with `apt-get` on every run. Agreed?
