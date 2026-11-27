# Server activity

Status: **built on 2026-11-25**, in one stack, #694, #695 and #697 to #703; the manual's chapter
is [Server activity](../book/src/server-activity.md), and the departures are in the roadmap's
demo note. Planned the same day (W15a), its open questions answered as proposed. The [roadmap](roadmap.md#well-rounded-track) tracks it
as W15 in the well-rounded track. It starts with PostgreSQL, then MySQL and MariaDB, SQL Server
and ClickHouse. Each layer's decisions are confirmed as it is built, and departures go in the
demo note.

A report that used to take a minute has run for twenty. A migration's `ALTER TABLE` hangs and
takes the application down with it, because everything after it queues behind its lock. A
colleague left a console open in a transaction on Friday, and its row locks are still there.
Today DbRover can't help with any of this: finding out means typing
`SELECT * FROM pg_stat_activity` (or `SHOW PROCESSLIST`, or `sp_who2`) from memory, reading a
wide grid of mostly idle connections, working out who blocks whom by hand, and then typing
`SELECT pg_terminate_backend(…)` with the number copied from the grid.

W15 adds a **Server activity** tab per data source: the server's sessions and what each one is
running, who is waiting on whose locks, and **Cancel** and **Kill**, under the data source's
safety level and recorded in the history.

## The question

Three questions come up, in this order of frequency:

1. **"What is running right now, and for how long?"** The long query, the session that has sat
   in an open transaction for an hour, the import still going.
2. **"Why is this stuck?"** A statement waiting on a lock, the session holding it, and what that
   one is doing (often nothing: *idle in transaction*).
3. **"Stop it."** Cancel the statement and keep the session, or end the session and roll back
   its transaction.

Other tools answer them in a similar shape. pgAdmin's dashboard calls it *Server activity*: the
sessions, their locks, and a button each to cancel a query or terminate a session. DBeaver has a
Session Manager, a list of sessions with the selected one's details beneath, a switch to hide
idle ones, and buttons to cancel or terminate, and a Lock Manager that draws who waits on whom.
TablePlus's Process List is a plain list that refreshes itself, with a kill button. DBX shows a
process list beside a server dashboard of counters. DataGrip shows the sessions DataGrip itself
has open. DbRover's version follows the shape they share (a list, a selected session's details,
the blocking chain, two buttons) and keeps to its own rules: what Cancel and Kill send is shown,
a careful data source asks first, a read-only one refuses, and the history records what was
done.

## What the tab shows

Right-click a data source → **Server activity** opens a tab, *Activity on* the data source,
beside the consoles, as History does (D8). One per data source: opening it again focuses it.

```text
+------------------------------------------------------------------------------------------+
| Activity on production    ● Every 5 s  [Pause]  [Refresh]   [ ] Idle  [ ] Background     |
| ! You see other users' sessions but not their statements: that needs pg_read_all_stats.  |
+------+-----------+--------+---------------------+-----------+---------+-------------------+
| Pid  | User      | DB     | Application         | State     | For     | Statement         |
+------+-----------+--------+---------------------+-----------+---------+-------------------+
| 4211 | app       | shop   | orders-api          | idle in   | 41 min  | UPDATE orders SET |
|      |           |        |                     | trans.    |         |  … (last run)     |
|  └ 4388 | report | shop   | DbRover: Orders.sql | waiting   | 3 min   | UPDATE orders SET |
|  └ 4390 | app    | shop   | orders-api          | waiting   | 2 min   | SELECT … FOR UPD… |
| 4302 | etl       | shop   | airflow             | active    | 19 min  | INSERT INTO daily |
| 4415 | you       | shop   | DbRover: this tab   | active    | 0 s     | SELECT … pg_stat… |
+------+-----------+--------+---------------------+-----------+---------+-------------------+
| 4388  report@10.0.3.7 · DbRover: Orders.sql · started 09:12 · transaction 3 min          |
| Waiting for a RowExclusiveLock on shop.public.orders, held by 4211 (idle in transaction) |
|   UPDATE orders SET status = 'paid' WHERE id = 3                                         |
| [Copy statement] [Open in console] [Go to blocker]               [Cancel query] [Kill…]  |
+------------------------------------------------------------------------------------------+
```

- **One row per session**, with its id (PostgreSQL's pid, MySQL's thread id, SQL Server's
  session id, ClickHouse's query id), user, database, application and client address, its
  state, how long it has been in that state, and its statement on one line. Columns the engine
  doesn't have are left out, not shown empty; engine-specific facts (ClickHouse's rows read and
  memory, SQL Server's percent complete) are extra columns on that engine only.
- **States** are DbRover's, mapped from each engine's words so the colours mean the same
  everywhere: **active** (running a statement), **waiting** (running, but waiting on a lock),
  **idle in transaction** (nothing running, a transaction open: the usual blocker), **idle**,
  and **background** (the server's own processes: autovacuum, replication, event scheduler,
  system sessions). The engine's own word is in the details.
- **Blocking as a tree.** A session that waits on another's lock is drawn under it, indented,
  with the lock it waits for; the root of each chain is the session to look at. A session
  blocked by several is drawn under the first and lists the others in its details. Sessions
  outside any chain follow, longest-running first. A cycle (a deadlock the server hasn't broken
  yet) is drawn once, from the session with the oldest transaction, and marked.
- **DbRover's own sessions** are named: *DbRover: this tab*, *DbRover: Orders.sql* (a
  console, by its file), *DbRover: explorer*. The tab's own row can't be cancelled or killed;
  a console's can, and that console then notices it lost its session, as W3 built.
- **Filters:** **Idle** and **Background** are off by default, so the list starts with what
  is doing something; a session in a blocking chain is always shown, idle or not. A text filter
  matches user, application, database and statement.
- **The selected session's details** below the list: everything the engine said about it, its
  statement in full (in the editor's colours, read-only), the lock it waits for and what holds
  it, and the buttons. **Copy statement**, **Open in console** (in a new console, not run), **Go
  to blocker**, and **Cancel query** and **Kill…**.
- **Selection survives a refresh** by session id. A selected session that has ended stays in
  the details, greyed, as *Ended* (with its last statement), until something else is selected.

### Refreshing

- **On a timer, every 5 s by default:** 1, 2, 5, 10 or 30 s from the toolbar, for that tab.
  **Settings › Server activity** sets what a new tab starts with, paused included, saved in
  `settings.toml` as `[activity] refresh_seconds` (W15d: the toolbar doesn't rewrite the
  setting, so trying 1 s on one busy server doesn't change every other tab).
  **Pause** stops it and **Refresh** reads once.
- **Only while the tab is shown.** A tab behind another, or in a window that is minimised,
  isn't read; it reads again when shown. A server isn't polled for a list no one is looking at.
- **One read at a time.** A read that takes longer than the interval delays the next instead of
  queuing more; the toolbar's dot turns amber while one is out, and a read that fails says why
  and keeps the last list, greyed, with the time it was read.
- **Each read has a 10 s time limit,** the engine's own where it has one (PostgreSQL's
  `statement_timeout`, MariaDB's `max_statement_time`, ClickHouse's `max_execution_time`) and
  the driver's cancel elsewhere. A read stuck behind a server in trouble must not leave the tab
  spinning.
- **Reads aren't recorded in the history.** Twelve statements a minute would bury everything
  else; the history keeps what Cancel and Kill send.

### Its own connection

The tab opens a connection of its own, as a console does, and closes it with the tab. A read
never waits behind the explorer loading a large schema, or behind a console's long query, which
is often the very thing the user came to stop. The tab isn't restored at start-up: restoring it
would connect to every server whose tab was open when DbRover quit.

## Cancel and Kill

| | PostgreSQL | MySQL, MariaDB | SQL Server | ClickHouse |
| --- | --- | --- | --- | --- |
| **Cancel query** | `pg_cancel_backend(pid)` | `KILL QUERY id` | — | `KILL QUERY WHERE query_id = '…' ASYNC` |
| **Kill** | `pg_terminate_backend(pid)` | `KILL CONNECTION id` | `KILL id` | — |

- **Cancel query** stops the running statement and keeps the session. PostgreSQL leaves its
  transaction open and failed until the client rolls it back; MySQL's statement is rolled
  back, the transaction stays.
- **Kill** ends the session: the server rolls back its open transaction and releases its locks.
  On SQL Server, rolling back a large transaction can take as long as it took to make, and the
  session shows *killed/rollback* meanwhile, with `KILL … WITH STATUSONLY`'s estimate in the
  details.
- **SQL Server has no Cancel** for another session: stopping a statement and keeping the
  session is an attention signal only the session's own client can send. The button isn't
  shown, and the manual says so. A DbRover console's own statement is still stopped by that
  console's Cancel.
- **ClickHouse has no sessions to kill** over HTTP: each query is its own request. Only
  **Cancel query** is shown.

### Asking first, and refusing

The rules are the data source's safety level (W2), applied as the object helpers apply them:

- **Read-only:** both buttons are disabled, saying *"production" is read-only*, and the engine
  refuses the command too, so an MCP client or a custom action can't get round it.
- **Careful:** both ask, naming the data source and the session: *Kill session 4211 on
  "production"?*, with the user, application, how long its transaction has been open, its
  statement, and what will be sent (`SELECT pg_terminate_backend(4211) …`).
- **Normal:** **Cancel query** runs at once; **Kill…** asks, as above, because it rolls back
  someone's transaction.
- **What can't work isn't offered.** A session of another user, when the login may only end
  its own, has the buttons disabled with the reason (*Ending another user's session needs
  pg_signal_backend*). The server still has the last word, and its refusal is shown as is.

### The session that was listed, not one that took its number

Between the read and the click, the session may have ended and its id been given to another.
PostgreSQL reuses pids, and SQL Server reuses session ids within seconds. So each stop names the
session as it was read, and nothing is sent to another:

- **PostgreSQL:** `SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE pid = $1 AND
  backend_start = $2`; Cancel also requires the same `query_start`, so it stops the statement
  that was listed, not the one the session started since.
- **SQL Server:** `KILL` takes no variables, so it runs inside a check of the session's
  `login_time`: `IF EXISTS (SELECT 1 FROM sys.dm_exec_sessions WHERE session_id = 52 AND
  login_time = '…') KILL 52`. A window of microseconds is left, between the check and the kill.
- **MySQL and MariaDB** thread ids only grow until the server restarts; ClickHouse's query ids
  are unique. Nothing extra is needed.
- **When it no longer matches,** nothing is sent and the tab says *Session 4211 had already
  ended*, then refreshes.

### Recorded in the history

Cancel and Kill are recorded in the query history (W4) with a new origin, **Activity**: the
statement sent, when, how it ended, and in `detail` what was stopped (*Killed 4211: app on
orders-api, idle in transaction for 41 min, last ran UPDATE orders SET …*). The statement in the
detail is redacted as W4b redacts statements, since it was typed by someone else. The History
tab gets the **Activity** badge and filter; older DbRovers sharing the file read the row's
`source`, `app`, as they read every other.

## What a login may see

Every engine shows less to a login without the right privilege, and each in its own way. The
tab says what is hidden and what would show it, in a line under the toolbar, and never passes a
partial list off as the whole server:

| Engine | Without the privilege | Shows everything | May end others' sessions |
| --- | --- | --- | --- |
| PostgreSQL | Every session is listed, but other roles' statements, states and times read `<insufficient privilege>` | Superuser, or member of `pg_read_all_stats` (or `pg_monitor`) | Superuser, or member of `pg_signal_backend` (not superusers' sessions); a role may always end its own |
| MySQL | Only the login's own threads | `PROCESS` | `CONNECTION_ADMIN` (or `SUPER`) |
| MariaDB | Only the login's own threads | `PROCESS` | `CONNECTION ADMIN` (or `SUPER`) |
| SQL Server | Only the login's own session | `VIEW SERVER STATE` (2019), `VIEW SERVER PERFORMANCE STATE` (2022) | `ALTER ANY CONNECTION` |
| ClickHouse | Nothing: the read is refused, own queries included (25.8, found in W15g) | `SELECT` on `system.processes` | `KILL QUERY` on other users' queries; a user may always stop its own. Not asked beforehand (`system.grants` needs a grant of its own): the server refuses |

- **How it's told:** PostgreSQL's `pg_has_role(…, 'member')` and `rolsuper`; SQL Server's
  `HAS_PERMS_BY_NAME(NULL, NULL, 'VIEW SERVER STATE')`; MySQL's and MariaDB's
  `information_schema.USER_PRIVILEGES` for the current user, which misses a privilege that only
  comes through a role, so there the line is worded as what was seen (*Only this login's
  sessions are listed. Seeing others' needs the PROCESS privilege.*) when every row is the
  login's own. Asked once, when the tab connects, and again on **Refresh** after a failure.
- **Locks need more on MySQL:** `performance_schema.data_lock_waits` needs `SELECT` on
  `performance_schema`, and `sys.innodb_lock_waits` on `sys`; without them the list still shows,
  and the tree says the blocking can't be read and why.
- **Managed services** grant less than a superuser. On Amazon RDS and Aurora for MySQL the
  master user ends others' sessions with `CALL mysql.rds_kill(id)` and
  `mysql.rds_kill_query(id)` (*to check* whether `KILL` works for it on 8.0); on RDS for
  PostgreSQL, `rds_superuser` may end anything but `rdsadmin`'s sessions; on Azure SQL Database
  the scope is one database and `VIEW DATABASE STATE`. Each is tried against the service in the
  layer that adds its engine, and what differs goes in the manual.

## Engines

### PostgreSQL

- **Sessions:** `pg_stat_activity` (`pid`, `usename`, `datname`, `application_name`,
  `client_addr`, `backend_start`, `xact_start`, `query_start`, `state_change`, `state`,
  `wait_event_type`, `wait_event`, `backend_type`, `query`; `leader_pid` from 13 and
  `query_id` from 14 where present). `backend_type` other than `client backend` is
  **background**; a parallel worker is folded under its leader.
- **Blocking:** `pg_blocking_pids(pid)`, called only for the sessions whose
  `wait_event_type = 'Lock'`, since it takes the lock manager's locks and costs on a busy server.
  The lock waited for, from `pg_locks` where `NOT granted`: its kind, mode and relation
  (`relation::regclass`), or the transaction id for a row lock.
- **Its statement is cut short** at `track_activity_query_size` (1,024 bytes by default); a
  statement exactly that long is marked *cut short by the server*.
- **Redshift**, reached through the PostgreSQL driver, has its own tables (`STV_RECENTS`,
  `STV_SESSIONS`, `SVV_TRANSACTIONS`, `STV_LOCKS`) and `CANCEL pid`; the menu item isn't shown
  there in W15 (see Later).
- **PgBouncer** in front of the server: the sessions are the server's connections, not the
  clients', and in transaction pooling one connection serves many clients, so Kill ends
  whichever client is using it. DbRover can't tell it is behind PgBouncer: through the
  `pgbouncer` test service (W15b), the server shows the client's `application_name` and
  PgBouncer's address, and nothing marks the session as pooled. So the manual says it, and
  *DbRover: this tab* may name another pooled session than the tab's.

### MySQL and MariaDB

- **Sessions:** MySQL 8.0.22 and later read `performance_schema.processlist`, which doesn't
  take the global mutex `information_schema.PROCESSLIST` takes (and that MySQL deprecates);
  older MySQL and MariaDB read `information_schema.PROCESSLIST` (MariaDB adds `TIME_MS` and
  `QUERY_ID`). `COMMAND = 'Sleep'` is **idle**, `Daemon`, `Binlog Dump` and the event scheduler
  **background**.
- **Open transactions** from `information_schema.INNODB_TRX` (`trx_mysql_thread_id`,
  `trx_started`): a sleeping thread with one is **idle in transaction**.
- **Blocking, MySQL 8:** row locks from `sys.innodb_lock_waits` (waiting and blocking thread,
  table, index, both lock modes), metadata locks from `sys.schema_table_lock_waits` (the
  `ALTER TABLE` waiting for a table's metadata lock). Both read `performance_schema`, on by
  default.
- **Blocking, MariaDB:** row locks from `information_schema.INNODB_LOCK_WAITS` joined to
  `INNODB_TRX` for thread ids, and `INNODB_LOCKS` for the table and modes. Metadata locks need
  the `METADATA_LOCK_INFO` plugin, which isn't installed by default: without it, a thread in
  *Waiting for table metadata lock* is **waiting** with no blocker named, and its details say
  why.

### SQL Server

- **Sessions:** `sys.dm_exec_sessions` left-joined to `sys.dm_exec_requests`, since a session
  that holds locks with nothing running (the usual head blocker) has no request. The statement
  is the request's, cut to its own `statement_start_offset` and `statement_end_offset` from
  `sys.dm_exec_sql_text`, or, for a session with nothing running, its last batch from
  `sys.dm_exec_input_buffer` (2016 and later). `is_user_process = 0` is **background**.
- **Blocking:** `blocking_session_id`, with `wait_type`, `wait_time` and `wait_resource`; an
  `open_transaction_count` above 0 with no request is **idle in transaction**.
- **Durations:** milliseconds from the request's `start_time` (or the session's
  `last_request_end_time`) and from `transaction_begin_time` to `SYSDATETIME()`. Those columns
  are `datetime`, rounded to 1/300 of a second and so up to about 1.7 ms late, and their clock
  can run a little ahead: a moment that has only just passed can come out a few milliseconds
  in the future, and is shown as zero.
- **Extra columns:** `percent_complete` for the commands that report it (backups, `DBCC`,
  rollbacks).

### ClickHouse

- **Queries, not sessions:** `system.processes` (`query_id`, `user`, `address`, `elapsed`,
  `read_rows`, `total_rows_approx`, `memory_usage`, `query`, `client_name`,
  `current_database` where present). Over HTTP there's nothing between queries, so there are
  no idle rows and no transactions.
- **One server:** `system.processes` is the node DbRover is connected to. A cluster's queries
  are on each node; `clusterAllReplicas` and `KILL QUERY ON CLUSTER` are left for later.
- **No locks to show.** Long mutations (`system.mutations`, `KILL MUTATION`) are the nearest
  thing, and are left for later.

### Not in W15

- **SQLite and DuckDB** run inside DbRover: there is no server and no one else's session.
- **Trino:** `system.runtime.queries` and `CALL system.runtime.kill_query(query_id => …,
  message => …)`, both SQL, on the container the tests already start. The cheapest next engine.
- **Redshift:** the tables above and `CANCEL`, and `PG_TERMINATE_BACKEND`; best built with
  C7, which is about Redshift.
- **Snowflake:** `SHOW TRANSACTIONS`, `SHOW LOCKS`, the `QUERY_HISTORY` table functions for
  running queries, `SYSTEM$CANCEL_QUERY` and `SYSTEM$ABORT_SESSION`; seeing others' queries
  needs `MONITOR` on the warehouse.
- **BigQuery, Athena and Databricks** run statements as jobs, which DbRover already lists and
  stops for its own (C0's `JobService`): `INFORMATION_SCHEMA.JOBS_BY_PROJECT` and `jobs.cancel`
  (with `bigquery.jobs.listAll`), `ListQueryExecutions` and `StopQueryExecution` per
  workgroup, and Databricks's query history API (statements started elsewhere can't be
  stopped through the API DbRover uses). Each is a list of jobs rather than sessions, with no
  locks.
- **MongoDB:** `$currentOp` and `killOp`, with the `inprog` and `killop` privileges.
  **Redis and Valkey:** `CLIENT LIST` and `CLIENT KILL ID`, connections rather than queries.
  **Elasticsearch and OpenSearch:** the `_tasks` API and `_tasks/<id>/_cancel`. Each needs its
  own columns, and an ACL its MCP login shouldn't have.
- **Cassandra and ScyllaDB** can't stop another client's query; **DynamoDB** has no sessions.

## MCP

Two tools, from the MCP2 permissions' rules:

- **`server_activity`** lists a data source's sessions as the tab reads them, with the blocking
  chains, and says what the login can't see. Other users' statements can hold anything they
  typed (customer data in a literal, a password), so it isn't covered by `read`: a new
  permission, **`activity`**, off by default, lists it. Statements in the reply are redacted
  as the history's are.
- **`stop_session`** (`cancel` or `kill`) needs `activity` and `admin`, which already covers
  "killing sessions" and `pg_terminate_backend` in `execute`. It is refused on a read-only data
  source, whatever the project allows, and recorded in the history as the MCP client's, with
  what was stopped in `detail`.
- **Through the MCP login,** when the data source has one: the database's grants decide what an
  assistant sees and may end, as they do for statements.

## How it's built

- **`dbrover-core`:** the model (`ServerActivity`: the sessions, what the login can't see, when
  it was read; `ServerSession`; `SessionState`; `LockWait`; `SessionStop::{CancelQuery,
  EndSession}`; a `SessionKey` holding the id with what identifies it, `backend_start` or
  `login_time`), and the blocking tree, built from `blocked_by` by a pure function, cycles
  included. Three `Connection` methods, defaulting to `Unsupported` as `describe_schema` does:
  `server_activity`, `stop_session(&SessionKey, SessionStop)`, and `own_session`, the id the
  server gives this connection (`pg_backend_pid()`, `CONNECTION_ID()`, `@@SPID`), which is how
  DbRover finds its own sessions.
- **`dbrover-sql`'s `Capabilities`** gains `server_activity`: whether the explorer offers the
  tab, and which of Cancel and Kill the engine has. A managed service whose tables differ
  (Redshift) turns it off through `ServerInfo::service`.
- **Each driver** reads with its own catalog queries, typed as `describe_role` reads, version
  by version where columns differ, under the 10 s limit.
- **`dbrover-engine`:** `Command::OpenActivity`, `ReadActivity`, `StopSession` and
  `CloseActivity`, answered by `Event::ActivityRead` and `Event::SessionStopped`, on a session
  task of the tab's own. `StopSession` checks the safety level (read-only refused), runs the
  stop, and records it with `HistorySource::Activity`. The engine names DbRover's own sessions
  from its consoles' and explorer's `own_session`, asked once per connection.
- **`dbrover-app`:** `Tab::Activity(DataSourceId)`, not restored; the timer in the tab (it
  sends `ReadActivity` when due, shown, and not waiting for one), the list and its tree drawn
  as the history tab draws its table, the details, and the confirmation through the hazard dialog's
  kit.
- **`dbrover-store`:** `HistorySource::Activity`, origin `activity`; `[activity]
  refresh_seconds` in `settings.toml`; `activity` in `McpPermissions`, defaulting to off for
  `projects.toml` files that don't have it.

## Options weighed

### A driver method, or SQL in `dbrover-sql`

The listing could be one SQL statement per dialect in `dbrover-sql`, run through `execute` and
read from its cells, like the DDL builder's statements. It would be tested without a server, but
the queries differ by version (MySQL 8.0.22, PostgreSQL 13 and 14, SQL Server 2016 SP2), some
take several statements (PostgreSQL's blocking pids only for waiting sessions), and the cells
would be parsed back into types the driver already had. A driver method reads with typed rows,
as `describe_role` and `describe_schema` do, and the conformance suite checks every engine the
same way. The stop statements are still written by pure functions, golden-tested per dialect,
so the confirmation can show what will be sent.

### Polling on the explorer's connection, or its own

The explorer's connection is idle most of the time, and a second connection is one more on a
server that may be short of them. But the explorer's requests run in order: a read would wait
behind a large schema's listing, and a refresh every few seconds would delay expanding a node.
The tab's own connection is one connection while the tab is open, which is what a console costs.

## Layers

1. **W15a — The plan:** this design, and the roadmap's entry.
2. **W15b — The model and PostgreSQL:** the core types, the blocking tree, the three
   `Connection` methods, PostgreSQL's reads and stops, and the conformance check
   `check_server_activity`: two connections, one holding a row lock in an open transaction, the
   other waiting on it; a third reads the waiter under its blocker, cancels the waiter (its
   statement fails, the blocker still holds), kills the blocker (its connection is lost, its
   update rolled back), and is refused a stop of a session that has since ended. A login
   without `pg_read_all_stats` sees `<insufficient privilege>` and is told so.
3. **W15c — The engine and the history:** the commands and events, the tab's session, the
   safety check, `HistorySource::Activity` and its badge and filter in History. Engine tests on
   the PostgreSQL test server, including a refusal on a read-only data source.
4. **W15d — The tab:** **Server activity** in the data source menu, the list, the tree, the
   filters, the details, the timer and Pause, the setting, and Cancel and Kill with their
   confirmation. Kittest tests on PostgreSQL: a blocked session drawn under its blocker, a
   careful data source asking by name, a read-only one's disabled buttons, the timer paused
   while the tab is hidden.
5. **W15e — MySQL and MariaDB:** both through the same driver tests, row locks and MySQL's
   metadata locks, a login without `PROCESS`.
6. **W15f — SQL Server:** sessions without requests, `KILL` behind the `login_time` check, no
   Cancel, a login without `VIEW SERVER STATE`.
7. **W15g — ClickHouse:** `system.processes`, Cancel only, a user's own and others' queries.
8. **W15h — MCP:** the `activity` permission on the project's page in Settings, the two tools,
   their tests in `dbrover-mcp`.
9. **W15i — Manual and demo note:** a chapter, *Server activity*, in `book/src`, with
   pictures from new scenes (`server-activity`, `server-activity-kill`) that set up a blocker
   and a waiter on the PostgreSQL test server; the data source menu's table in
   [Data sources](../book/src/data-sources.md); History's new origin; MCP's permission; the
   exit as an engine test.

The blocking tests use the containers `docker-compose.yml` already starts, in the driver and
engine test binaries CI already runs, so no job is added. Each waits on a lock for well under a
second: the waiter is read as soon as the server shows it waiting, polled every 50 ms, not after
a fixed sleep.

**Exit:** on PostgreSQL, with one session holding a row lock in an open transaction and another
waiting on it, Server activity shows the waiter under its blocker with the lock it waits for;
**Cancel query** on the waiter stops its statement; **Kill…** on the blocker of a careful data
source asks, naming the data source, then ends the session and releases the lock, and History
lists the kill as **Activity**; on a read-only data source both are refused.

## Decisions

The first five were asked on 2026-11-25 and confirmed as proposed; the rest are confirmed as
their layers are built.

| Question | Proposed | Consequence |
| --- | --- | --- |
| Kill on a normal data source | **Asks**, as a drop does; Cancel runs at once | One click never rolls back someone's hour of work; stopping a runaway query stays quick. |
| Reading activity over MCP | **A new permission, `activity`, off by default**; stopping needs `admin` too | Other users' statements aren't shown to an assistant that may only read tables. |
| The refresh | **Every 5 s, only while the tab is shown**, saved in Settings | Quick enough to watch a lock clear, and nothing polls a server no one is looking at. |
| What shows at first | **Idle and background sessions hidden**, except in a blocking chain | The list starts with what is doing something; the idle blocker still shows. |
| The engines | **PostgreSQL, MySQL, MariaDB, SQL Server, ClickHouse**; Trino next; Redshift with C7 | The servers DbRover's users run themselves first. |
| How many sessions one Kill stops | **One at a time** | A confirmation that names what it ends; ending a blocker usually frees the rest. |
| Where reads run | **The tab's own connection** | Never behind the explorer or a console; one connection per open tab. |
| Restoring the tab at start-up | **No** | Starting DbRover doesn't connect to every server whose tab was open. |
| Stopping the session listed | **Matched by its start time** (PostgreSQL, SQL Server) | A reused pid or session id is never stopped by mistake. |
| Reads in the history | **Not recorded**; Cancel and Kill are | The history stays readable; what changed something is there. |
| SQL Server's Cancel | **Not offered** for other sessions | Only the session's own client can stop a statement and keep the session. |
| Where the listing is written | **A driver method**, with the stop statements as pure, golden-tested functions | Typed rows per version; the confirmation shows what will be sent. |

## Risks

- **Killing the wrong thing.** A pid reused between read and click, a pooled connection behind
  PgBouncer, a DbRover console the user forgot was theirs. The start-time match, the PgBouncer
  line, and the *DbRover: …* names cover each, and the confirmation names user, application
  and statement.
- **Load on a server in trouble.** Polling a struggling server makes it worse. One read at a
  time, a 10 s limit, `pg_blocking_pids` only for waiting sessions, MySQL's
  `performance_schema` rather than the mutex-holding `PROCESSLIST`, and nothing read while the
  tab is hidden. The interval goes up to 30 s, and Pause stops it.
- **Secrets in other people's statements.** The tab shows what the server shows the login,
  which is what `SELECT * FROM pg_stat_activity` would; MCP gets it only with `activity`, and
  redacted; the history's detail is redacted.
- **A partial list taken for the whole server.** Every engine hides others' sessions
  differently; the line under the toolbar says what is hidden and the privilege that shows it.
- **SQL Server's long rollback.** A killed session that made a large change rolls it back for as
  long as it took; the session stays listed as *killed/rollback* with its estimate, so a second
  Kill isn't mistaken for needed.
- **Flaky blocking tests.** Two connections and a lock are timing-sensitive. Tests wait for the
  server to report the wait rather than sleeping, set a lock timeout on the waiter so a broken
  test fails in seconds rather than hanging CI, and are run in a loop before each layer lands,
  as the cancel tests were in M3.
- **Managed services' privileges.** RDS, Aurora, Azure SQL and Cloud SQL each grant a little
  less, or by other means. Tried against each service in its engine's layer; the manual lists
  what differs, and the server's refusal is always shown as it is.

## What stays out

- **A server dashboard:** connections, transactions per second, cache hit rates over time, as
  DBX and pgAdmin draw them. Counters over time are another feature, and a natural follow-up.
- **Long-running query alerts**, and stopping queries on a rule.
- **Killing many sessions at once.**
- **A lock graph** beyond the tree, and locks held by sessions no one waits on.
- **Prepared (two-phase) transactions**, replication slots, and vacuum progress.
- **Search across a database's data**, a separate milestone: W16, [Searching a database's
  data](data-search.md).
