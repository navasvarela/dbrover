# Cloud and big data

Status: **C0 in progress** (started 2026-11-02); C1 to C6 planned, not scheduled. Proposed on
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
- **C0c — Microsoft Entra ID sign-in:** an `entra` module that asks the sign-in of the Azure
  CLI (`az account get-access-token`) or of the Azure Developer CLI (`azd auth token`) for a
  token, with the program and its arguments as a list (no shell), an optional tenant, a
  timeout, and the resource a database expects (Azure SQL's or Azure Database's). The token
  and its expiry come from the program's JSON. Managed identity, a service principal's secret
  and a browser sign-in come later, when someone needs them.
  **Exit:** a stand-in program's JSON becomes a token with the right expiry, for each CLI; a
  program that isn't installed, fails, says nothing or runs past the timeout is an error that
  says which, with its standard error.
- **C0d — Signing in to a database with a cloud account:** the password of a PostgreSQL, MySQL
  or SQL Server data source can come from a cloud sign-in instead of the keychain or a command
  (W9), chosen in the dialog's **Password** section as **AWS IAM**, **Google Cloud** or
  **Microsoft Entra ID**:
  - AWS: an RDS IAM authentication token, made on the machine from the profile's credentials
    with AWS's own signer (`aws-sigv4`, already in the build for N5) for the data source's
    host, port, user and region; nothing is sent to AWS to make it.
  - Google Cloud: the ADC token, as the password of Cloud SQL's IAM users.
  - Entra ID: the CLI's token, as the password on PostgreSQL and MySQL, and through TDS's own
    token sign-in (`tiberius`'s `AuthMethod::AADToken`) on SQL Server.
  The token is fetched on every connection, as W9's command is run, so reconnecting and each
  new console sign in afresh; it is never written to the keychain, a file, the log or the
  history. A data source has either a password command or a cloud sign-in, never both.
  **Exit:** an RDS token for fixed credentials and a fixed time matches the one AWS's own
  signer makes, with `X-Amz-Expires=900`; on the PostgreSQL test server, a data source whose
  sign-in is a stand-in provider connects with the token it hands out and asks for a new one
  on the next connection; the dialog's choice is written to and read from
  `data_sources.toml`.
- **C0e — Cost guards per data source:** `Connection::full_scan` becomes
  `Connection::estimate_cost`, answering with a `CostEstimate`: a whole-table read (N5's
  `FullScan`), or the bytes a statement will scan (BigQuery's dry run in C4; Athena reports
  them after the run). The data source's `CostGuard` gains a byte limit beside its whole-table
  switch, and judges an estimate in one pure function that every caller uses: the console's
  question, Run to file's and the MCP server's refusals, and the engine's automatic runs.
  `Event::ScanHeld` and `ScanApproval` become `Event::CostHeld` and `CostApproval`, with
  **Open data**'s approval (reads a page at a time) still asking about bytes, since a page of a
  byte-billed result costs the whole query. No engine estimates bytes yet, so the limit has no
  field in the dialog until C3 or C4 brings one; it is read from and written to the file.
  **Exit:** DynamoDB's guard works as before through the new names, with its tests unchanged
  in substance; the judging function's table of cases covers both kinds of estimate, each
  approval and the limit's edges; a `cost_guard = "off"` written before C0 still reads.
- **C0f — Queries as jobs:** a `job` module in `dbrover-core` with a `JobService` interface
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
- **C0g — Searching a catalog without loading it:** `Connection::search_catalog` finds objects
  by name anywhere in the connected database, on the server, a hundred at most; drivers that
  can't return `None`, and the explorer filters what it has loaded, as before. PostgreSQL
  searches every schema of its database (tables, views, materialized views, sequences and
  functions), and SQLite every attached schema, so the explorer is tested without a server.
  While the filter box has text, a data source that searches shows **Found on the server**
  under its row: the matches not already shown, each with its path (`sales › public ›
  orders`), acting as the node itself (Open data, Show definition, its menu). Levels still load
  only when expanded.
  **Exit:** on SQLite in a kittest, a table in a schema never expanded is found from the filter
  box and opened; on the PostgreSQL test server, a search among 2,500 tables in two schemas
  returns the matches, capped, with the server's case-insensitive match.
- **C0h — Manual and demo note:** a chapter, **Cloud sign-in and costs**, on signing in with a
  cloud account, the cost guard, the progress line and searching the explorer, and the C0 demo
  note in the roadmap.

**Exit:** an RDS PostgreSQL database connects with an AWS profile and no password, and keeps
working past the token's 15-minute lifetime. C0 can only show this with stand-ins (C0d's exit);
trying it against RDS, Cloud SQL and Azure Database with real accounts is left to the user
before release, as N5's IAM Identity Center check was.

### C1 — Managed databases

- Try each managed service in [Which services](#which-services-and-in-what-order), item 1, in
  the providers' order, and fix what its catalog or defaults do differently (Redshift's system
  tables, Azure SQL's gateway redirect, Neon's and Supabase's poolers, ClickHouse Cloud's TLS).
- Synapse and Fabric through the SQL Server driver: their warehouses support only part of
  T-SQL's catalog views and DDL, so the explorer and the object helpers check what each
  supports, and Fabric connects only with an Entra ID token.
- The data source dialog offers each as a preset: the right driver, port, TLS mode and sign-in.
- **Exit:** each service on the list connects, browses and runs the conformance suite where an
  account is available; a short page in the manual per provider.

### C2 — Object storage and table formats (DuckDB)

- An **object storage** data source (or a DuckDB option): S3, then Google Cloud Storage, then
  Azure Blob Storage, with C0's credentials passed to DuckDB as secrets, and the bucket
  browsable in the explorer.
- Iceberg and Delta tables, through DuckDB's extensions.
- MotherDuck, through DuckDB's `md:` databases.
- **Exit:** query `s3://bucket/path/*.parquet` and an Iceberg table from a MinIO container in CI
  with no AWS account.

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
