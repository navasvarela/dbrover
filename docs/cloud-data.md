# Cloud and big data

Status: **planned**, not scheduled. Proposed on 2026-10-27; the order and the decisions below are
confirmed when the track starts. The providers' priority was set on 2026-10-28: AWS, then Google
Cloud, then Azure. On 2026-11-01 DynamoDB was moved ahead of the track, as the NoSQL track's
[N5](nosql.md#n5--dynamodb), with the two parts of C0 it needs: the AWS sign-in and a cost
guard against reading a whole table.

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

The groundwork every later milestone needs.

- **Credential providers** in a new `dbrover-cloud` crate, chosen per data source, built in the
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

**Exit:** an RDS PostgreSQL database connects with an AWS profile and no password, and keeps
working past the token's 15-minute lifetime.

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
