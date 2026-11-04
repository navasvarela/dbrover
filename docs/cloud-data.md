# Cloud and big data

Status: **C0 built** (2026-11-02); **C1 built** (2026-11-03), tried only against local
stand-ins; **C2 started** (2026-11-04); C3 to C6 planned, not scheduled. Proposed on
2026-10-27; the order and the decisions below are confirmed as each milestone starts. The
providers' priority was set on 2026-10-28: AWS, then Google Cloud, then Azure. On 2026-11-01
DynamoDB was moved ahead of the track, as the NoSQL track's [N5](nosql.md#n5--dynamodb), with
the two parts of C0 it needs: the AWS sign-in and a cost guard against reading a whole table.

DbRover connects to databases you run or reach by host and port. Much of the data people query
today lives elsewhere: managed databases signed into with a cloud account instead of a password,
Parquet and table formats in object storage, and warehouses such as Snowflake and BigQuery that
answer over HTTPS and bill by the work a query does. This plan brings those in, starting with
what costs least: the engines DbRover already speaks, hosted in the cloud.

## What changes when the data is in the cloud

Each warehouse is a new engine, built like any other (see [Adding a database](adding-a-database.md)).
What is new is shared by all of them, and is built once:

- **Signing in with a cloud account.** Managed databases accept short-lived tokens from AWS IAM,
  Google Cloud IAM or Microsoft Entra ID instead of a stored password, and warehouses accept
  only tokens, key pairs or OAuth. DbRover should reuse the sign-in the user already has, never
  store a cloud secret itself, and refresh tokens before they expire.
- **Queries that cost money.** BigQuery bills by bytes scanned, Snowflake and Databricks by
  warehouse time, Athena by bytes scanned. Things DbRover does without asking on a local
  database (counting a table's rows, **Open data**, completion's catalog reads, W5's
  **Export every row**) can cost real money there. The user needs to see the cost before
  running, a limit per data source, and defaults that never scan a whole table unasked.
- **Queries that run as jobs.** Warehouses run a statement as a job: submit, poll, page through
  the results, cancel by the job's id. The engine's cursor and cancel model needs that shape,
  including results that arrive in many partitions.
- **Catalogs too large to load at once.** A BigQuery organisation or a Unity Catalog can have
  thousands of projects, datasets or schemas. The explorer must load lazily, page, and search on
  the server instead of walking everything.
- **No container to test against.** Snowflake, BigQuery and Databricks have no official local
  server, so CI can't run the conformance suite against them the way it does for the engines in
  `docker-compose.yml`.

## Which services, and in what order

The providers come in this order: **AWS, then Google Cloud, then Azure.** Within each milestone,
AWS's services are built and tried first, Google Cloud's next and Azure's last, and a warehouse
that belongs to one of them comes before the ones that run on any cloud. Services that aren't
tied to a provider (Neon, Supabase, ClickHouse Cloud, MotherDuck, Snowflake, Databricks) aren't
ranked by it.

1. **Managed versions of the engines DbRover already has.** They need no new protocol, only
   cloud sign-in and fixes for what each catalog does differently. The cheapest step, and it
   reaches the most users.
   - AWS: Amazon RDS and Aurora (PostgreSQL and MySQL), and Amazon Redshift, which speaks
     PostgreSQL's protocol with its own catalog.
   - Google Cloud: Cloud SQL for PostgreSQL, MySQL and SQL Server.
   - Azure: Azure Database for PostgreSQL and MySQL, Azure SQL, and Azure's warehouses that
     speak SQL Server's protocol: Synapse's dedicated and serverless SQL pools, and Microsoft
     Fabric's warehouse and SQL analytics endpoint.
   - Not tied to a provider: Neon, Supabase and ClickHouse Cloud.
2. **Object storage through DuckDB.** DuckDB, already compiled into DbRover, reads Parquet, CSV
   and JSON from S3, Google Cloud Storage and Azure Blob Storage, in that order, and Iceberg and
   Delta tables, through its own extensions. That is a data lake console with no new driver,
   plus MotherDuck (hosted DuckDB) through the same engine.
3. **Athena and Trino.** Athena is AWS's own query service and needs only C0's AWS sign-in;
   it speaks Trino's dialect through AWS's API, with results written to S3. Trino's HTTP
   protocol (submit, then follow `nextUri`) then covers Trino, Starburst and Presto with the same
   dialect.
4. **BigQuery.** Google's warehouse, with a REST jobs API, GoogleSQL, and a dry run that says
   how many bytes a query will scan before it runs.
5. **Snowflake.** The most used cloud warehouse among the tools DbRover is compared with, and
   available on all three clouds. Its SQL API is HTTPS with JSON or Arrow results, which fits the
   ClickHouse driver's shape.
6. **Databricks SQL.** A SQL warehouse over Unity Catalog's three-level names, through the
   Statement Execution API (HTTPS, like Snowflake's), also on all three clouds.

Azure's warehouses don't need a milestone of their own: Synapse and Fabric answer over SQL
Server's protocol, so they come in C1 through the SQL Server driver, with Fabric signing in only
with Entra ID. The order between Snowflake and Databricks is a proposal; it is checked against
current usage numbers, as [More databases](databases.md) did for its engines, when the track
starts.

Left out on purpose:

- **Hadoop-era engines** (Hive through Thrift, Impala, Spark Thrift Server). Fewer new users,
  Thrift and SASL protocols, and the same data usually reachable through Trino or Databricks.
- **NoSQL cloud stores** (Cosmos DB, Bigtable, Firestore), for now. The
  [NoSQL track](nosql.md) builds what they need first: query languages beside SQL and
  document results. DynamoDB, the first of them, is that track's N5, built ahead of C0 with
  C0's AWS sign-in and its own cost guard; Cosmos DB's MongoDB API may already connect through
  that track's MongoDB driver.
- **Running or managing cloud resources** (creating instances, scaling warehouses, IAM policy).
  DbRover queries data; the cloud consoles and infrastructure-as-code tools manage it.

## Milestones

### C0 — Cloud sign-in, cost guards and jobs

The groundwork every later milestone needs. Started on 2026-11-02, right after W8, with AWS's
sign-in and DynamoDB's cost guard already built by [N5](nosql.md#n5--dynamodb); C0 builds the
rest on them rather than beside them.

- **Credential providers** in the `dbrover-cloud` crate, chosen per data source, built in the
  providers' order:
  - AWS: the default credential chain of AWS's own SDK, `aws-config`, with a profile and a
    region per data source, for AWS's APIs, built first for DynamoDB in N5 as the crate's
    `aws` module; then, from the same chain, an IAM authentication token for RDS and Aurora
    (PostgreSQL and MySQL);
  - Google Cloud: an access token from Application Default Credentials, for Cloud SQL's IAM
    database authentication;
  - Microsoft Entra ID: an access token for Azure SQL, Synapse, Fabric and Azure Database for
    PostgreSQL and MySQL, which also completes SQL Server's planned Entra ID sign-in (see
    [More databases](databases.md#decisions)).
  Tokens are fetched when connecting and refreshed before they expire; none is stored,
  logged or written to the history.
- **Cost guards**, as a per-data-source setting next to the safety level (W2): a limit (bytes
  scanned, or a time budget) above which a statement asks first, and quiet defaults for the
  explorer and the grid, so that no automatic action scans a whole table. DynamoDB's is the
  first (N5): the data source's `CostGuard` asks before a statement reads a whole table, and
  bounds what each page reads; C0 adds the limits beside it.
- **Jobs in the engine:** a cursor that submits, polls and pages, with cancel by job id, and a
  status line showing what the job has done so far (bytes, rows, elapsed time). ClickHouse's
  `query_id` cancel is the first user.
- **Lazy, searchable catalogs** in the explorer, for catalogs with thousands of children.

#### What N5 already built

- `dbrover-cloud::aws`: `AwsSignIn` (default chain, a profile alone, or a local emulator's
  made-up keys), on rustls with ring, with a test that no AWS crate brings aws-lc-rs.
- `CostGuard` on `DynamoDbConfig` (`cost_guard = "off"` turns it off), read through
  `ConnectionConfig::cost_guard`; `Connection::full_scan` and `FullScan`; the engine's
  `session/cost.rs`, `Event::ScanHeld` and `ScanApproval`; the console's **Scan anyway**; Run
  to file and the MCP server refusing.
- Paging and the server's filter in the explorer came earlier still, with
  [N0a·4](nosql.md#n0a--groundwork-languages-safety-and-the-explorer): every level loads when it
  is expanded, a large one a thousand nodes at a time, and the filter box asks the server again
  for a partial level.

#### Layers

Each layer is one pull request in a stack, useful and tested on its own, with no cloud account:
providers are tried with local fakes (a token endpoint on `127.0.0.1`, a program standing in for
a CLI) and with tokens the tests make up. None reads the machine's own sign-in.

- **C0a — Plan:** this section, C0's rows in [Decisions](#decisions) and [Risks](#risks), and
  the roadmap's entry in layers.
- **C0b — Google Cloud sign-in** (built): a provider-neutral `AccessToken` (the secret, which never
  prints, and when it expires) and a cache that fetches a new one when less than five minutes
  are left, shared by every provider. A `google` module signs in with Application Default
  Credentials as Google's own libraries find them: the file `GOOGLE_APPLICATION_CREDENTIALS`
  names, else the one `gcloud auth application-default login` writes. Its two common kinds are
  read by DbRover: a user's refresh token (`authorized_user`) and a service account's key
  (`service_account`, a JWT signed with RS256 by ring). The other kinds (workload identity
  federation, impersonation) and the metadata server of a Google Cloud VM are refused with
  their names, and wait for someone to need them. Requests go over hyper and rustls with ring,
  as the Elasticsearch driver's do.
  **Exit:** both kinds of file get a token from a fake token endpoint, signed and asked for as
  Google documents; a token is reused until five minutes before it expires; an unknown kind
  and a missing file are errors naming what to do.
- **C0c — Microsoft Entra ID sign-in** (built): an `entra` module that asks the sign-in of the Azure
  CLI (`az account get-access-token`) or of the Azure Developer CLI (`azd auth token`) for a
  token, with the program and its arguments as a list (no shell), an optional tenant, a
  timeout, and the resource a database expects (Azure SQL's or Azure Database's). The token
  and its expiry come from the program's JSON. Managed identity, a service principal's secret
  and a browser sign-in come later, when someone needs them.
  **Exit:** a stand-in program's JSON becomes a token with the right expiry, for each CLI; a
  program that isn't installed, fails, says nothing or runs past the timeout is an error that
  says which, with its standard error.
- **C0d — Signing in to a database with a cloud account** (built): the password of a PostgreSQL,
  MySQL or SQL Server data source can come from a cloud sign-in instead of the keychain or a command
  (W9), chosen in the dialog's **Password** section as **AWS IAM**, **Google Cloud** or **Microsoft
  Entra ID**:
  - AWS: an RDS IAM authentication token, made on the machine from the profile's credentials
    with AWS's own signer (`aws-sigv4`, already in the build for N5) for the data source's
    host, port, user and region; nothing is sent to AWS to make it.
  - Google Cloud: the ADC token, as the password of Cloud SQL's IAM users.
  - Entra ID: the CLI's token, as the password on PostgreSQL and MySQL, and through TDS's own
    token sign-in (`tiberius`'s `AuthMethod::AADToken`) on SQL Server.
  The token is fetched on every connection, as W9's command is run, so reconnecting and each
  new console sign in afresh; it is never written to the keychain, a file, the log or the
  history. A data source has either a password command or a cloud sign-in, never both. On
  MySQL, IAM users sign in with the `mysql_clear_password` plugin, so a cloud sign-in needs TLS
  that can't fall back to plain text (**require** or **verify-full**).
  **Exit:** an RDS token for fixed credentials and a fixed time matches the one AWS's own
  signer makes, with `X-Amz-Expires=900`; on the PostgreSQL test server, a data source whose
  sign-in is a stand-in provider connects with the token it hands out and asks for a new one
  on the next connection; the dialog's choice is written to and read from
  `data_sources.toml`.
- **C0e — Cost guards per data source** (built): `Connection::full_scan` becomes
  `Connection::estimate_cost`, answering with a `CostEstimate`: a whole-table read (N5's
  `FullScan`), or the bytes a statement will scan (BigQuery's dry run in C4; Athena reports
  them after the run). The data source's `CostGuard` gains a byte limit beside its whole-table
  switch, and judges an estimate in one pure function that every caller uses: the console's
  question, Run to file's and the MCP server's refusals, and the engine's automatic runs.
  `Event::ScanHeld` and `ScanApproval` become `Event::CostHeld` and `CostApproval`, with
  **Open data**'s approval (reads a page at a time) still asking about bytes, since a page of a
  byte-billed result costs the whole query. No engine estimates bytes yet, so the limit has no
  field in the dialog until C3 or C4 brings one; it is read from and written to the file as
  `ask_above_bytes`, beside N5's `cost_guard`, which keeps its key.
  **Exit:** DynamoDB's guard works as before through the new names, with its tests unchanged
  in substance; the judging function's table of cases covers both kinds of estimate, each
  approval and the limit's edges; a `cost_guard = "off"` written before C0 still reads.
- **C0f — Queries as jobs** (built): a `job` module in `dbrover-core` with a `JobService` interface
  (submit a statement, poll it by its `JobId`, page through its result, cancel it) and a
  `JobCursor` and `JobCancel` built once on it, which C3 to C6's engines implement instead of
  each writing its own polling. While a statement runs, the engine asks the connection's
  progress handle what it has done (rows and bytes read, elapsed time) once a second and sends
  it as `Event::StatementProgress`; the console's status line shows it beside the elapsed time.
  ClickHouse is the first user: its progress is read from `system.processes` by the statement's
  `query_id`, the same id its cancel already kills it by.
  **Exit:** a fake service in `dbrover-core`'s tests submits, polls with a growing pause,
  pages through three pages and cancels by id, and a failed job's message reaches the cursor;
  on the ClickHouse test server, a slow `SELECT` reports rows read before it ends, and cancel
  still stops it.
- **C0g — Searching a catalog without loading it** (built): `Connection::search_catalog` finds
  objects by name anywhere in the connected database, on the server, a hundred at most; drivers
  that can't return `None`, and the explorer filters what it has loaded, as before. PostgreSQL
  searches the tables, views, materialized views and sequences of every schema of its database
  (functions are left out: their names carry their arguments), and SQLite the tables and views
  of every attached schema, so the explorer is tested without a server. Once the filter box's
  typing pauses, each connected data source that searches shows **Found on the server** under
  its row: the matches the tree doesn't already show, each with its schema beside it, acting as
  the node itself (Open data, Show definition, its menu, expanding it). Typing never connects a
  data source, and levels still load only when expanded. The filter box's hint becomes **Filter,
  or search the server**.
  **Exit:** on SQLite in a kittest, a table in a schema never expanded is found from the filter
  box and opened; on the PostgreSQL test server, a search among 2,500 tables and a second schema
  returns the matches with their paths, capped, with the server's case-insensitive match; a
  conformance check that each match is the node its level lists.
- **C0h — Manual and demo note** (built): a chapter, **Cloud sign-in and costs**, on signing in with
  a cloud account, the cost guard, the progress line and searching the explorer, and the C0 demo
  note in the roadmap.

**Exit:** an RDS PostgreSQL database connects with an AWS profile and no password, and keeps
working past the token's 15-minute lifetime. C0 can only show this with stand-ins (C0d's exit);
trying it against RDS, Cloud SQL and Azure Database with real accounts is left to the user
before release, as N5's IAM Identity Center check was.

### C1 — Managed databases

Started on 2026-11-03, after C0. Every service in
[Which services](#which-services-and-in-what-order), item 1, through a driver DbRover already
has: the work is in what each service's catalog, defaults and sign-in do differently, and in
presets that fill the data source dialog with them.

What differs, service by service, as their documentation describes it (none of it tried
against the services while planning):

| Service | Driver | Port | TLS | Sign-in | What differs |
| --- | --- | --- | --- | --- | --- |
| Amazon RDS and Aurora, PostgreSQL | PostgreSQL | 5432 | `require` | password, AWS IAM | Certificates from Amazon RDS's own authority, so `verify-full` needs its bundle. |
| Amazon RDS and Aurora, MySQL | MySQL | 3306 | `require` | password, AWS IAM | The same authority; IAM needs TLS (C0d). |
| Amazon Redshift, provisioned and Serverless | PostgreSQL | 5439 | `verify-full` | password | PostgreSQL 8.0's protocol with a catalog of its own: no sequences, indexes or triggers in `pg_class`, external (Spectrum) tables and late-binding views outside it, the `svv_all_*` views covering all of them, `SHOW TABLE` for a table's DDL, and no arrays as parameters. |
| Cloud SQL for PostgreSQL and MySQL | PostgreSQL, MySQL | 5432, 3306 | `require` | password, Google Cloud | A certificate per instance, signed by the instance's own authority and naming no host, so `verify-full` needs the Auth Proxy or its `server-ca.pem`. |
| Cloud SQL for SQL Server | SQL Server | 1433 | `require` | password | No IAM sign-in for SQL Server. |
| Azure Database for PostgreSQL and MySQL (flexible server) | PostgreSQL, MySQL | 5432, 3306 | `verify-full` | password, Entra ID | Public authorities; the user is the plain name, since single server's `user@server` is retired. |
| Azure SQL Database | SQL Server | 1433 | `verify-full` | password, Entra ID | Inside Azure the gateway redirects the login to the database's node (ports 11000 to 11999), which the driver already follows once on a direct route. |
| Azure Synapse, dedicated and serverless SQL pools | SQL Server | 1433 | `verify-full` | password, Entra ID | Part of T-SQL's catalog views and DDL: no foreign keys, check constraints, triggers, sequences or synonyms, and no reading another database's catalog in a dedicated pool. |
| Microsoft Fabric, warehouse and SQL analytics endpoint | SQL Server | 1433 | `verify-full` | Entra ID only | As Synapse, and no indexes; the SQL analytics endpoint's tables are read-only. |
| Neon | PostgreSQL | 5432 | `verify-full` | password | A host with `-pooler` is PgBouncer in transaction mode; the endpoint is found from TLS's server name, which DbRover always sends. |
| Supabase | PostgreSQL | 5432 | `require` | password | The pooler (Supavisor) keeps a session on 5432 and is in transaction mode on 6543, and its user is `postgres.<project>`; the certificate is from Supabase's own authority. |
| ClickHouse Cloud | ClickHouse | 8443 | `verify-full` | password | HTTPS only, with public authorities; user `default`; an idle service wakes on the first request, which can take many seconds. |

Two fixes the list asked for are smaller than the plan feared: Azure SQL's gateway redirect is
already followed by the SQL Server driver (one redirect, on a direct route; a tunnel says it
can't reach the node), and ClickHouse Cloud's TLS is HTTPS on its own port, which the
ClickHouse driver and its TLS test server already cover. C1 gives both their presets and their
manual pages, and nothing more.

#### Recognising a service

The driver, not the data source's settings, decides what a server is: PostgreSQL's
`version()` names Redshift, and SQL Server's `SERVERPROPERTY('EngineEdition')` tells Azure SQL
(5), a Synapse dedicated pool (6), and a Synapse serverless pool or Fabric (11), told apart by
Fabric's host name. A data source that reaches the service through an SSH tunnel, a private
endpoint or a host name of its own is recognised all the same. The connection's `ServerInfo`
carries what was found, so the explorer and the object helpers read it as they read the
engine's capabilities. Presets only fill in the dialog; nothing names the service in
`data_sources.toml`. A pooler can't be recognised (it passes the server's own answers on), so
the PostgreSQL driver is made to work behind one whether or not there is one (C1c).

#### Testing without an account

No layer uses a cloud account, reads the machine's cloud sign-ins (`~/.aws`,
`~/.config/gcloud`, the Azure CLI's caches) or signs in anywhere. Each difference is tried on
the closest local stand-in, and the plan and the demo note say which it was:

- **Poolers:** PgBouncer in transaction mode with prepared statements off, in front of the
  PostgreSQL test server, stands in for Neon's and Supabase's poolers. Supavisor isn't
  PgBouncer, so Supabase's own pooler is tried by hand.
- **Redshift:** its catalog code runs against PostgreSQL with stand-in `svv_*` views made from
  `information_schema`, in a schema of the test database, so the queries and the tree are
  tried; `SHOW TABLE` and the real views' columns are checked against Redshift's documentation
  and tried by hand.
- **Synapse and Fabric:** the narrowed catalog and helpers run on the SQL Server test server
  with the service forced, which shows their queries are T-SQL that only reads the views each
  warehouse has; recognising them is tested on `SERVERPROPERTY` values from Microsoft's
  documentation.
- **Presets:** a table test per service (driver, port, TLS, sign-ins, the host names it
  recognises), and a check that each preset makes a data source that saves and reads back.
- **ClickHouse Cloud's TLS:** the existing ClickHouse TLS test server, with the preset's
  `verify-full` and the test authority.

What only a real account shows is the C1 demo note's list for the user: each service connects,
browses and runs the conformance suite (`dbrover-core::conformance`).

#### Layers

Each layer is one pull request in a stack, useful and tested on its own, with no cloud account.

- **C1a — Plan:** this section, C1's rows in [Decisions](#decisions) and [Risks](#risks), and
  the roadmap's entry in layers.
- **C1b — Managed services** (built): a `ManagedService` table in `dbrover-core`, one row per service
  above, in the providers' order: its driver, port, TLS mode, the sign-ins it takes and the
  host names that recognise it (`*.rds.amazonaws.com`, `*.redshift-serverless.amazonaws.com`,
  `*.database.windows.net`, `*-pooler.*.neon.tech`, `*.clickhouse.cloud`, …). Applying a
  service to a data source's settings sets those, and keeps the host, user and database the
  user typed. `ServerInfo` gains the service a connection found itself on, `None` for a plain
  engine.
  **Exit:** a table test per service; a host name of each pattern is recognised and others
  aren't; each service's settings save to and read from `data_sources.toml`.
- **C1c — PostgreSQL behind a transaction pooler** (built): the driver's own queries (the explorer,
  table descriptions, roles) become unnamed statements, sent with their types in one round
  trip (tokio-postgres's `query_typed`), so no prepared statement outlives the transaction a
  pooler gives the connection, and each is one round trip instead of two. Checking a
  console's statement becomes `PREPARE` and `DEALLOCATE` in one message, so both run on the
  same server connection. Nothing is saved to say a pooler is there: the driver behaves the
  same with or without one. Read-only data sources work as before, since their guard is a
  read-only transaction (W11) and not a setting of the session. A `pgbouncer` test server
  joins `docker-compose.yml` in transaction mode with prepared statements off.
  **Exit:** through PgBouncer, two connections browse the catalog, describe tables and check
  statements in turn on the same server connection without "prepared statement does not
  exist" or "already exists"; a read-only console still refuses writes; the conformance suite
  passes through it.
  **As built:** the stand-in is PgBouncer with two server connections used in turn, which
  reproduced `prepared statement "s2" does not exist` before the change (one server connection
  hid it). The conformance suite's checks keep temporary tables across statements, which no
  transaction pooler keeps, so they run on the direct server; through PgBouncer the test checks
  each of the driver's own queries on two connections in turn, a statement check with its
  error position, and a read-only transaction refusing a write.
- **C1d — Redshift** (built): the PostgreSQL driver recognises Redshift from `version()` and browses it
  through `svv_redshift_databases`, `svv_all_schemas`, `svv_all_tables` and
  `svv_all_columns`: databases, schemas, then tables (local and external) and views with their
  columns. Definitions come from `SHOW TABLE` and `SHOW VIEW`; searching the server (C0g)
  uses `svv_all_tables`. Materialized views, functions and procedures, and the object
  helpers' Redshift DDL, wait for someone to need them; the explorer offers no helper on
  Redshift rather than PostgreSQL's.
  **Exit:** the Redshift catalog walks a test database with stand-in `svv_*` views, and every
  node it lists is one its level lists again (the conformance walk); `version()` strings from
  Redshift's documentation are recognised and PostgreSQL's aren't.
  **As built:** Redshift has no roles folder, and its tables open read-only: PostgreSQL's table
  description reads catalogs Redshift lacks, and Redshift's own is left for later. The walk
  checks every node's path but not definitions, since `SHOW TABLE` is Redshift's alone.
- **C1e — Synapse and Fabric** (built): the SQL Server driver recognises them (`EngineEdition` and the
  host), lists only the folders each has (no sequences, synonyms or triggers; no indexes on
  Fabric), leaves out the constraint kinds each lacks, and reads only the connected database's
  catalog where another's can't be read (a dedicated pool). The object helpers' capabilities
  are narrowed for them: no foreign keys or checks on Synapse, no indexes on Fabric, foreign
  keys `NOT ENFORCED` on Fabric. Fabric's preset signs in only with Entra ID, and the dialog
  refuses a password for a Fabric host.
  **Exit:** recognition from each `EngineEdition` and host; on the SQL Server test server with
  each warehouse forced, the explorer walks the narrowed tree with no query failing; the
  narrowed capabilities hide the helpers each lacks.
- **C1f — Presets in the data source dialog** (built): **New data source** gains a **Cloud service**
  submenu, by provider (AWS, Google Cloud, Azure, then the others), opening the dialog with
  the service's settings; the dialog's **Service** row shows the service its host is
  recognised as, and choosing one there applies it. A hint under the host says what the service expects (`verify-full` needs RDS's
  bundle, Supabase's user is `postgres.<project>`).
  **Exit:** a kittest opens each service from the menu and sees its driver, port, TLS and
  sign-in in the form; typing a Neon pooler host shows Neon; saving writes them.
  **As built:** a submenu per cloud in **New data source** (**Amazon Web Services**, **Google
  Cloud**, **Microsoft Azure**, **Other cloud services**) rather than one **Cloud service**
  submenu, which with fifteen services was taller than a 740-point window. The **Service** row
  shows only for a data source on a service (chosen there, or named by its host), so a plain
  server's dialog keeps the height a 900-point window only just holds. Each service's preset is
  checked by a table test of the form; the kittest adds a Fabric warehouse from its menu.
- **C1g — Manual and demo note** (built): a page per provider in the Data sources part (Amazon Web
  Services, Google Cloud, Microsoft Azure, and Neon, Supabase and ClickHouse Cloud), each
  saying what DbRover does differently there and what wasn't tried, and the C1 demo note in
  the roadmap.

**Exit:** each service on the list connects, browses and runs the conformance suite where an
account is available; a short page in the manual per provider. C1 can only show this with
stand-ins (each layer's exit); trying the services with real accounts is left to the user
before release, as C0's sign-ins were.

### C2 — Object storage and table formats (DuckDB)

Started on 2026-11-04, after C1. DuckDB reads Parquet, CSV and JSON from S3, Google Cloud
Storage and Azure Blob Storage, and Iceberg and Delta tables, through extensions it downloads
on first use. C2 gives a DuckDB data source a bucket: C0's sign-ins become DuckDB's secrets,
the bucket shows in the explorer, and its files and tables open like any table. MotherDuck
comes through the same driver, as a third kind of DuckDB database.

#### What DuckDB does, as tried on 2026-11-04

Tried with the library DbRover links (DuckDB 1.5.6's prebuilt library for macOS arm64, as CI
and the release link it; `docs/duckdb-prebuilt.md`) against MinIO, fake-gcs-server and
Azurite in local containers, with no cloud account:

| Question | What DuckDB does |
| --- | --- |
| Extensions in the build | Only `core_functions`, `parquet`, `json`, `icu` and `autocomplete` are linked in. `httpfs` (S3, Google Cloud Storage, HTTP), `azure`, `iceberg` (and `avro`, which it loads), `delta` and `motherduck` are downloaded from `extensions.duckdb.org`, signed by DuckDB and checked against that signature, the first time a statement needs them (`autoinstall_known_extensions`, on by default). All of them installed and loaded. |
| Their size | `httpfs` 16 MB, `iceberg` 34 MB and `avro` 8 MB, `delta` 65 MB, `azure` 13 MB, `motherduck` 10 MB, each downloaded once per DuckDB version. |
| Where they are kept | DuckDB's own folder, `~/.duckdb/extensions/v1.5.6/<platform>/` (`osx_arm64`, `linux_amd64`, `windows_amd64`), shared with DuckDB's command-line client and Python package; a new DuckDB version downloads its own. |
| Offline | The statement that needs the extension fails with DuckDB's *Extension Autoloading Error*, naming the extension and the address it tried; nothing else is affected, and the next statement tries again. Making an S3 or Google Cloud Storage secret needs `httpfs` too, so it fails the same way. |
| Credentials | A temporary secret, kept in memory: `CREATE TEMPORARY SECRET (TYPE s3, KEY_ID …, SECRET …, SESSION_TOKEN …, REGION …, ENDPOINT …)`; `TYPE gcs` takes a `BEARER_TOKEN`, which is C0's Google token; `TYPE azure` with `PROVIDER access_token` takes an `ACCESS_TOKEN`, which is C0's Entra ID token. `duckdb_secrets()` shows every secret's values as `redacted`. |
| MinIO and other S3-compatible stores | `ENDPOINT`, `URL_STYLE 'path'` and `USE_SSL false` reach MinIO on `localhost`. Reading and writing Parquet, and globs such as `s3://bucket/sales/*/*.parquet`, work. |
| Listing a bucket | `glob('s3://bucket/*')` lists only the files at that level and no folders, and `glob('s3://bucket/**')` lists every key under the prefix: DuckDB asks S3 for every key below the glob's fixed prefix, with no delimiter, and matches them itself. Fine for a query, no good for a tree. |
| Iceberg | `iceberg_scan` reads a table from its metadata file (`…/metadata/00002-….metadata.json`) on MinIO. Given the table's folder alone, it needs `metadata/version-hint.text`, which Hadoop-style catalogs write and pyiceberg's SQL catalog, Glue and REST catalogs don't; otherwise it refuses unless `unsafe_enable_version_guessing` is set. |
| Delta | `delta_scan('s3://bucket/products')` reads a delta-rs table on MinIO with the same S3 secret. |
| Google Cloud Storage stand-in | fake-gcs-server (25 MB to download) answers DuckDB's reads and its listing with a made-up token, but not its writes. |
| Azure stand-in | Azurite answers the `azure` extension, but its image is 427 MB. |

#### How a bucket is browsed

DuckDB can't list a level of a bucket, so DbRover lists it itself, over HTTPS with the same
credentials it gives DuckDB: S3's `ListObjectsV2`, signed with SigV4 (AWS's own signer, already
in the build for C0's RDS tokens), Google Cloud Storage's JSON API and Azure's List Blobs, each
with `/` as the delimiter. One request lists a level, a thousand entries at a time
(N0a's page), so a bucket of millions of keys loads only the levels that are expanded, as fast
as the first level of a small one.

- The bucket is a node beside the DuckDB database's own, labelled with its name. Prefixes are
  folders, and files are leaves with their size beside them.
- A file DuckDB reads (`.parquet`, `.csv`, `.tsv`, `.json`, `.jsonl`, `.ndjson`, compressed or
  not) opens with **Open data** as `SELECT * FROM 's3://bucket/path/file.parquet'`, and expands
  to its columns, which `DESCRIBE` reads from the file's footer or first rows.
- A folder holding `_delta_log/` is a Delta table, and one holding `metadata/` with
  `*.metadata.json` files an Iceberg table: the folder's first child is then the table, which
  opens with `delta_scan` or with `iceberg_scan` on the newest metadata file, found by listing
  the metadata folder. A `*.metadata.json` file opens with `iceberg_scan` too.
- The explorer's filter is applied to each page of a level as it loads. The providers filter
  only by a key's start, with case, and the explorer's filter keeps any name that contains it,
  without, so sending it would change what it finds.
- Nothing is read to draw the tree but the listings: no file is opened until it is expanded
  or opened.

#### Signing in to a bucket

| Provider | Sign-ins | DuckDB secret |
| --- | --- | --- |
| Amazon S3 | an AWS profile or the default chain (C0), an access key whose secret is in the keychain (MinIO, Cloudflare R2 and other S3-compatible stores), or none, for public buckets | `TYPE s3` with the key, secret and session token |
| Google Cloud Storage | Application Default Credentials or a key file (C0b), or none | `TYPE gcs` with the access token as `BEARER_TOKEN` |
| Azure Blob Storage | the Azure CLI's or Azure Developer CLI's sign-in (C0c), for the storage resource, or none | `TYPE azure`, `PROVIDER access_token`, and the account name |

The secret is made when the connection opens and made again before a statement once the
credentials have less than five minutes left (C0's margin): AWS's SDK says when a profile's
role or SSO credentials expire, and C0's tokens carry their expiry. It is always temporary,
and `allow_persistent_secrets` is turned off, so DuckDB never writes one to
`~/.duckdb/stored_secrets` nor loads one from there. The statement that makes it is DbRover's
own, so it reaches neither the history nor the log, and an error making it is reported without
DuckDB's echo of the statement.

#### Testing without an account

- **S3:** MinIO in `docker-compose.yml`, with a bucket that starts with checked-in fixtures:
  Parquet files under a partitioned path, a delta-rs table, and an Iceberg table written by
  pyiceberg against MinIO (its metadata names `s3://dbrover/…`, so the bucket keeps that
  name). MinIO's own images were withdrawn from Docker Hub and Quay in 2025; the
  `pgsty/minio` build of the same server (54 MB to download, `mc` included) stands in, pinned
  to a release.
- **Google Cloud Storage:** fake-gcs-server, which DuckDB reads from; listing is tried against
  it too. Writing isn't, as it doesn't take DuckDB's.
- **Azure Blob Storage:** not in CI, since Azurite is 427 MB. The listing's request and its
  answers are unit-tested with answers in Azure's documented XML, and reading is tried by hand
  against Azurite.
- **Credentials:** the AWS sign-in with C0's test credentials files and an empty environment;
  Google and Entra ID with C0's stand-ins. None reads the machine's own sign-in.
- **MotherDuck:** needs an account, so only the connection string and the token's handling are
  tested; connecting is left to the user, as the demo note says.
- **CI:** the tests that read buckets run where the MinIO and fake-gcs-server variables are
  set, which is the Linux check, as for every test server. They download `httpfs`, `iceberg`,
  `avro` and `delta` (about 125 MB, a few seconds on GitHub's runners) from DuckDB on each
  run; if that turns out flaky, the extension folder is cached by DuckDB version.

#### Layers

Each layer is one pull request in a stack, useful and tested on its own, with no cloud account.

- **C2a — Plan:** this section, C2's rows in [Decisions](#decisions) and [Risks](#risks), and
  the roadmap's entry in layers.
- **C2b — A bucket in a DuckDB data source's settings:** `DuckDbConfig` gains an optional
  `storage`: the provider (S3, Google Cloud Storage or Azure Blob Storage), the bucket (an
  Azure account and container), a region, an endpoint for S3-compatible stores and stand-ins,
  and the sign-in. Written to `data_sources.toml` as `[data_source.storage]`, with
  placeholders (W9) resolved in each field; an access key's secret goes to the keychain as the
  data source's password.
  **Exit:** each provider and sign-in saves and reads back; a data source written before C2
  reads as before; the `Display` of a data source names the bucket and never a secret.
- **C2c — Reading S3 through DuckDB:** the DuckDB driver makes the bucket's secret from the
  sign-in (C0's AWS chain or profile, the access key and its secret, or none), makes it again
  before it expires, and turns persistent secrets off. An extension that couldn't be
  downloaded is reported as such, with what to do offline. MinIO joins the test servers with
  its fixtures.
  **Exit:** on MinIO, `SELECT count(*) FROM 's3://dbrover/sales/*/*.parquet'` counts the
  fixtures' rows through a data source with an access key, and fails with MinIO's refusal
  without it; a secret near its expiry is made again before the next statement; no secret is
  persistent; the error for an extension that can't be downloaded names it.
  **As built:** the secret is scoped to the bucket (`SCOPE 's3://<bucket>'`), so the data
  source's credentials and endpoint apply to it alone, and a console can still read another
  bucket with a secret of its own. Every DuckDB database DbRover opens, buckets or not, starts
  with `allow_persistent_secrets` off. A statement naming a remote address (`://`) isn't
  checked as it is typed, since preparing it reads the file's footer from the bucket; its
  errors show when it runs. The `minio` test server is healthy 1.8 s after `docker compose up`
  on a laptop, and its image is 54 MB to download.
- **C2d — The bucket in the explorer:** S3's listing with a delimiter, a level at a time and a
  thousand entries a page; folders, files and their columns; **Open data** on a file.
  **Exit:** on MinIO, the explorer's levels list the fixtures' folders and files with paging
  and a prefix filter; a Parquet file's columns are its schema's; the conformance walk passes
  on every node; the listing's request is signed as AWS's signer signs it.
  **As built:** the listing is `dbrover-cloud`'s `storage` module: `ListObjectsV2` signed by
  `aws-sigv4` (the signature checked against a separate SigV4 in Python's standard library),
  read with `roxmltree`, virtual-hosted on AWS unless the bucket's name has a dot, by path at
  an endpoint, and asked again in the bucket's own region when S3 answers with a redirect
  naming it. The bucket's node is named by its address (`s3://dbrover`) and labelled by its
  name, so a path alone says where an object is, and **Open data** on a file is
  `SELECT * FROM '<address>'`. A file's columns are DuckDB's `DESCRIBE` of that, so a file in
  a Hive-style folder (`year=2025/`) has the partition's column too, as its rows do.
  `Connection::children`, which wants a level whole (the conformance walk, the MCP server),
  lists ten pages at most and logs that it stopped; the explorer pages through all of it.
- **C2e — Iceberg and Delta tables:** the explorer's table nodes for both formats, opened with
  `iceberg_scan` and `delta_scan`.
  **Exit:** on MinIO, the fixtures' Iceberg and Delta tables are found from their folders and
  **Open data** reads their rows: C2's exit, in CI.
  **As built:** the table is the first node of its folder's first page, labelled with the
  folder's name, beside the folder's own keys (`_delta_log/`, `data/`, `metadata/`), which stay
  browsable. Its path's last segment names what DuckDB reads (the newest metadata file, or
  nothing for Delta), so `object_source` in `dbrover-core` turns any bucket path into the
  `FROM` clause that both **Open data** and the table's columns use. The newest Iceberg
  metadata file is the highest version among `NNNNN-….metadata.json` or `vN.metadata.json`,
  from at most twenty pages of the metadata folder. The fixtures: an Iceberg table written by
  pyiceberg's SQL catalog against MinIO, and a Delta table written by delta-rs, 60 KB in all
  (`docker/minio/README.md`).
- **C2f — Google Cloud Storage and Azure Blob Storage:** the `gcs` secret from C0's Google
  token and the listing over the JSON API, tried on fake-gcs-server, which joins the test
  servers; the `azure` secret from an Entra ID token for Azure Storage, and List Blobs.
  **Exit:** on fake-gcs-server, a Parquet file is read and its bucket listed with a stand-in
  token; Azure's listing reads documented answers, pages with its marker, and sends the token
  only over HTTPS.
- **C2g — MotherDuck:** a DuckDB data source's database can be a MotherDuck database (`md:`
  and its name), signed in with a token kept in the keychain as its password and handed to
  DuckDB as `motherduck_token`, never in the saved settings.
  **Exit:** the connection string for each case, and the token's absence from every `Debug`,
  `Display` and error; connecting to MotherDuck is tried by hand only.
- **C2h — The data source dialog:** the DuckDB dialog gains a **Bucket** section (provider,
  bucket, region, endpoint, sign-in) and MotherDuck beside **File** and **In memory**.
  **Exit:** a kittest adds an S3 bucket with an access key and a MotherDuck database, and
  each saves as typed.
- **C2i — Manual and demo note:** a page in the manual's Data sources part, **Object storage
  and MotherDuck**, and the C2 demo note in the roadmap.

**Exit:** query `s3://bucket/path/*.parquet` and an Iceberg table from a MinIO container in CI
with no AWS account (C2c and C2e). S3 itself, Google Cloud Storage, Azure Blob Storage and
MotherDuck are tried by hand with real accounts, before release, as C0's and C1's services
were.

### C3 — Athena and Trino

- Athena, through AWS's API with C0's AWS credentials and its workgroup's output location:
  the Glue Data Catalog's databases and tables in the explorer, the bytes each query scanned,
  and C0's cost limit as the workgroup's per-query limit where one isn't set.
- Then a driver over Trino's HTTP protocol, with catalogs as the top level, sharing Athena's
  dialect.
- **Exit:** Athena tested with recorded exchanges and tried live; the conformance suite against
  Trino's container, in a job that runs nightly if the container is too large for every pull
  request.

### C4 — BigQuery

- A driver over the REST jobs API: projects, datasets and tables in the explorer, GoogleSQL,
  a dry run before every statement that shows the bytes it will scan, and
  `maximumBytesBilled` from C0's limit.

### C5 — Snowflake

- A driver over Snowflake's SQL API: key-pair (JWT) and OAuth sign-in, asynchronous statements,
  partitioned results, cancel.
- The Snowflake dialect, catalog (databases, schemas, tables, views, stages), and the warehouse
  and role per console.

### C6 — Databricks SQL

- A driver over the Statement Execution API, with Unity Catalog's catalog, schema and table
  levels, and a personal access token or OAuth.

## Decisions

Proposed defaults; each is confirmed when its milestone starts.

| Question | Proposed | Consequence |
| --- | --- | --- |
| Which provider first | **AWS, then Google Cloud, then Azure.** Settled on 2026-10-28, not only proposed. | Each milestone builds and tries AWS's services first, and Athena moves ahead of the warehouses that run on any cloud. Azure's services come last in each milestone, but its SQL Server-protocol warehouses (Synapse, Fabric) join C1 instead of waiting for a driver of their own. |
| How DbRover gets cloud credentials | **From the provider's own sign-in on the machine first:** AWS profiles and SSO cache, Google Application Default Credentials, Azure CLI's sign-in, read or invoked (`aws`, `gcloud`, `az`) without a shell. A browser OAuth flow later, for warehouses that need it. | Uses the login people already have, and no cloud secret ever lives in DbRover's files. Users without the CLIs need them installed, or a key file. |
| Cloud SDKs | **No full SDKs,** except AWS's: small HTTP clients on the hyper and rustls stack the ClickHouse driver already uses for Google Cloud and Azure. For AWS, `aws-config` and each service's SDK crate, without default features and on ring, agreed on 2026-11-01 for DynamoDB (N5), which measures what it costs. | AWS's credential chain (SSO, `aws login`, roles, refreshing) is AWS's own code, not DbRover's. The cost in build time and download is in [N5](nosql.md#n5--dynamodb); each later AWS service adds its own SDK crate. |
| Warehouse protocols | **Their HTTPS APIs** (Snowflake SQL API, BigQuery REST, Databricks Statement Execution, Trino HTTP), not ODBC or JDBC. | Pure Rust, no drivers to install, works through proxies; some services' fastest result paths (BigQuery's Storage Read API, Arrow Flight) come later if needed. |
| Testing services with no container | **Recorded HTTP exchanges replayed by a fake server in CI**, plus live tests run by hand (or nightly) with credentials kept in repository secrets. | Every pull request stays fast and needs no cloud account; the live tests catch API drift. |
| DuckDB extensions | **Downloaded by DuckDB on first use**, signed by DuckDB, rather than compiled in. | Downloads stay the same size; the first use of S3 or Iceberg needs network access. |
| What runs without asking | **Nothing that scans a whole table** on a data source with a cost limit: no row counts, **Open data** with a `LIMIT`, export and import asking first. | A few conveniences are slower to reach on warehouses, in exchange for no surprise bills. |

C0's own decisions, taken on 2026-11-02 when it started; each can be overturned in review.

| Question | Chosen | Consequence |
| --- | --- | --- |
| How Google Cloud signs in | **Application Default Credentials, read by DbRover** for the two kinds `gcloud` and the console write (a user's refresh token, a service account's key), not by running `gcloud`. | Works with only a key file and no `gcloud` installed, as Google's libraries do. Workload identity federation, impersonation and a VM's metadata server are refused by name until someone needs them. |
| How Entra ID signs in | **The Azure CLI's or the Azure Developer CLI's own sign-in, asked for a token** by running `az` or `azd` without a shell, not by reading their token caches. | The caches are encrypted on Windows and macOS and their format is the CLI's own; running the program is what Azure's own libraries do. Users need `az` or `azd` signed in; managed identity and a browser sign-in come later. |
| Where a cloud sign-in goes in a data source | **In the server's settings, in place of the password**, beside W9's password command and exclusive of it: `cloud_sign_in = { provider = "aws", profile = "…", region = "…" }`. | The token is the password the driver already sends, so PostgreSQL and MySQL need no change; only SQL Server's Entra token takes TDS's own path. |
| When a token is fetched | **On every connection**, from a cache that refreshes it five minutes before it expires; RDS tokens are made on the machine each time. | A connection lasts past its token, as the databases allow; reconnecting signs in afresh. |
| The cost guard's limit | **A byte limit beside N5's whole-table switch,** in the engine's own settings as N5's switch is; a time budget waits for Snowflake and Databricks (C5, C6), which bill by time and have their own statement timeouts to set. | One judging function for every estimate; the limit has no field in the dialog until an engine estimates bytes. |
| Jobs | **One `JobService` interface and one `JobCursor`**, polling with a growing pause (0.1 s doubling to 2 s), rather than each warehouse driver writing its own loop. | C3 to C6 implement four calls each; the engine and the console see an ordinary cursor and a progress handle. |
| Searching a catalog | **A search on the server, a hundred matches at most, shown apart from the tree** under **Found on the server**, rather than expanding the tree to each match. | Nothing loads that wasn't asked for; a match is usable at once without loading its parents' levels. |

C1's own decisions, taken on 2026-11-03 when it started; each can be overturned in review.

| Question | Chosen | Consequence |
| --- | --- | --- |
| How a service is recognised | **By the driver, from what the server says** (`version()`, `SERVERPROPERTY('EngineEdition')`), with the host name only to tell Fabric from a Synapse serverless pool. Presets fill in the dialog and are not saved. | A tunnel, a private endpoint or a custom host name changes nothing; the dialog shows the service from the host, and an unrecognised host is just its engine. |
| Poolers | **Unnamed statements for everything the driver asks, always,** rather than a setting that says a pooler is there. | One code path whether or not a pooler is there, and one round trip fewer per catalog query; nothing for the user to get wrong. A user's own `SET` still lands on whichever server connection the pooler picks, as with any client, and the manual says so. |
| Redshift's catalog | **The `svv_all_*` views,** not `pg_catalog`. | External tables and late-binding views show up; Redshift's materialized views, functions and object helpers come later. |
| Redshift's sign-in | **A password in C1.** C0d's RDS token doesn't sign in to Redshift, which hands out temporary credentials through its own API (`GetClusterCredentials`, Serverless's `GetCredentials`) instead; that needs a signed request to Redshift and is left for later. | Redshift users with IAM users only can use W9's password command meanwhile, or wait for that follow-up. |
| Synapse and Fabric | **The SQL Server driver, narrowed by a capability table** per warehouse from Microsoft's documentation, rather than a driver of their own. | The same TDS, sign-in and grid; a view or DDL form the table wrongly allows fails with the server's own message. |
| TLS in presets | **`verify-full` where the service's certificate chains to a public authority** (Azure, Redshift, Neon, ClickHouse Cloud), **`require` where it is the provider's own** (RDS, Cloud SQL, Supabase), with the CA file to download named in the manual. | Presets connect on the first try; checking the server's name on RDS, Cloud SQL and Supabase is one file away. |
| Testing | **Stand-ins only** (PgBouncer, views imitating Redshift's, SQL Server with a warehouse forced), and a list of live checks in the demo note. | CI needs no account; what only the services show waits for the user's own accounts. |

C2's own decisions, taken on 2026-11-04 when it started; each can be overturned in review.

| Question | Chosen | Consequence |
| --- | --- | --- |
| A data source of its own, or a DuckDB option | **An optional bucket on a DuckDB data source**, in a file, in memory or on MotherDuck, rather than a new kind of data source. | A query joins a bucket's files with the database's tables, and everything DuckDB data sources have (the console, the grid, exports, reports, the MCP server) works unchanged. One bucket per data source; two buckets are two data sources, or a second secret made in a console. |
| How DuckDB is linked | **Unchanged:** the prebuilt library (`docs/duckdb-prebuilt.md`) in CI and the release, compiled locally, with the extensions downloaded on first use (the proposed default above, confirmed). | Downloads stay the size they are; the first S3, Iceberg or Delta query needs the network once per DuckDB version. Compiling `httpfs` and the others in would add a C++ build and their own dependencies (curl, OpenSSL, the Azure SDK, delta-kernel in Rust) to every build. |
| Where extensions are kept | **DuckDB's own folder,** `~/.duckdb/extensions/<version>/<platform>`, shared with DuckDB's other clients. | An extension one installed is the other's too; DbRover adds no folder of its own. The report's DuckDB (RP2) still never downloads one. |
| Offline | **The statement fails with DuckDB's own error, and DbRover says what it means:** which extension, that it is downloaded once, and how to install it from a file. Connecting doesn't fail when the bucket's secret can't be made for that reason; it is made again before the next statement. | A DuckDB file with a bucket still opens on a plane; its local tables work. |
| Extensions under read-only and cost guards | **Downloading stays allowed:** it changes no data and costs nothing on the user's bill. `INSTALL`, `LOAD`, `CREATE SECRET`, `ATTACH` and `COPY` stay refused on read-only data sources and in read-only MCP projects, as W11 classifies them; a `SELECT` that needs an extension still loads it. No setting. | A read-only data source reads its bucket. An administrator who wants no downloads at all sets DuckDB's `autoinstall_known_extensions` off in a console; a setting for it can follow if asked for. |
| Cost | **No byte limit for buckets in C2.** DuckDB doesn't estimate what a statement reads, and object storage bills requests and transfer rather than scans; the explorer's own actions read a listing, a file's footer, or a page of rows. | A query over `**/*.parquet` reads what it reads, as in DuckDB's own client. An estimate from the listing's sizes could feed C0's byte limit later. |
| Credentials for DuckDB | **Temporary secrets made by DbRover from C0's sign-ins,** never DuckDB's own credential chains (the `aws` extension, `PROVIDER credential_chain`), and persistent secrets off. | One sign-in code path for databases and buckets; no 20 MB `aws` extension to download; nothing written to disk; tokens refreshed by DbRover before they expire. |
| Listing a bucket | **DbRover's own requests with a delimiter,** a level and a thousand keys at a time, rather than DuckDB's `glob`. Signed S3 requests may go over plain HTTP, as DuckDB's do, since a signature doesn't reveal the secret; a bearer token goes only over HTTPS, or to this machine. | A bucket of millions of keys browses as fast as a small one. Three small clients to keep, one per provider's API. |
| Iceberg without a catalog | **The newest `*.metadata.json` in the table's `metadata/` folder,** found by listing it, rather than DuckDB's version guessing. | Tables written by any catalog open; a table whose metadata folder holds many thousands of files takes a request per thousand to open. Iceberg REST catalogs (Glue, Polaris, S3 Tables) come later, through DuckDB's `ATTACH … (TYPE iceberg)`. |
| S3 stand-in | **MinIO, as the `pgsty/minio` build,** pinned to a release, since MinIO's own images are gone from Docker Hub and Quay. | The server the plan named, maintained by a third party; swapping in another S3-compatible server (versitygw, SeaweedFS) changes only the compose file. |

## Risks

- **Surprise bills.** A missed automatic query on a large table costs the user money. Every
  automatic statement goes through one place that checks the data source's cost guard, with a
  test per engine.
- **APIs that change.** Cloud APIs version and deprecate on their own schedules. Recorded
  exchanges pin the behaviour tested; the live tests show when they drift.
- **CI time.** New containers (MinIO, Trino) add to the Linux job; anything over a minute or two
  moves to a nightly job, as [More databases](databases.md#risks) plans for large engines.
- **Accounts to test with.** Some services need a paid account to try at all. A milestone ships
  only what was tried against the real service, and says what wasn't.
- **Tokens in the wrong place.** A token is a password for its lifetime. It lives in memory only,
  in the type that never prints its secret, and is never put in an error message, the log, a
  file or the history; a test checks the `Debug` of every type that holds one.
- **Sign-ins that can't be tried here.** C0 is built with stand-ins and no cloud account. What
  only the real services can show (RDS's and Cloud SQL's IAM users, Azure Database's Entra
  sign-in, MySQL's cleartext plugin over TLS for RDS) is listed in C0's demo note for the user
  to try before release.
- **Stand-ins that differ from the service.** PgBouncer isn't Supavisor, PostgreSQL views
  aren't Redshift's, and SQL Server with a warehouse forced isn't Synapse or Fabric. Each
  stand-in shows DbRover's own queries are well formed and avoid what the service lacks; only
  the services show the rest, and C1's demo note lists each one to try.
- **Catalog differences documented wrongly or changing.** Synapse's and Fabric's supported
  views and DDL change often (Fabric is young). The capability table is one place to correct,
  and anything it wrongly allows fails with the warehouse's own error rather than silently.
- **Extensions DbRover doesn't build.** `httpfs`, `iceberg`, `delta` and `azure` are DuckDB's,
  downloaded at run time: a DuckDB release that changes one changes DbRover's behaviour
  without a DbRover release, and an extension that isn't published for a platform yet
  (as happens for a few days after a DuckDB release) fails there. The extension's version is
  tied to DuckDB's, which DbRover pins, and C2's tests on MinIO run on every pull request.
- **Secrets in DuckDB's memory.** A temporary secret lives in the connection's DuckDB, where a
  console or an MCP client could read it if DuckDB let it: `duckdb_secrets()` redacts it, and
  neither `allow_unredacted_secrets` nor `allow_persistent_secrets` can be turned on while
  DuckDB runs (tried: *Cannot change allow_unredacted_secrets setting while database is
  running*). An error making the secret is reported without the statement, which holds it.
- **Stand-ins for buckets.** MinIO, fake-gcs-server and documented XML answers aren't S3,
  Google Cloud Storage and Azure. C2's demo note lists what to try with real accounts.
- **Session state behind a transaction pooler.** A console's `SET`, temporary table or
  `LISTEN` lasts only for its transaction's server connection, and the next statement may get
  another. DbRover sets nothing of its own on the session; the manual's Neon and Supabase
  pages say to use the session port (Supabase's 5432, Neon's host without `-pooler`) for work
  that needs it.
