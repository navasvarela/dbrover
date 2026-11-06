# Athena and Trino

DbRover connects to Amazon Athena, AWS's query service over data in S3, and to Trino, the engine
Athena runs on, which Starburst sells and Presto was forked from. Both speak Trino's SQL, so a
console on either highlights, checks and formats the same way; Athena's DDL is Hive's, with
backquoted names, and DbRover reads that too.

## Amazon Athena

### Signing in

An Athena data source keeps no password and no AWS secret. It signs in as a
[DynamoDB](dynamodb.md#signing-in) data source does, through AWS's own SDK, with what is already
on your machine (`~/.aws`, IAM Identity Center, `aws login`, a role). Add one with **New data
source › Amazon Athena**, or under **Amazon Web Services**.

| Field | Default | Notes |
| --- | --- | --- |
| Sign in with | AWS profile | **AWS profile**, or **Local emulator** for LocalStack's Athena. |
| Profile | AWS's default | A profile of your AWS files. Empty: AWS's default chain. |
| Region | the profile's | Such as `eu-west-1`. |
| Workgroup | `primary` | The workgroup queries run in. It may say where results go, and limit what a query scans. |
| Catalog | `AwsDataCatalog` | The catalog unqualified names resolve in: the Glue Data Catalog, or a federated one. |
| Database | `default` | The database unqualified names resolve in. |
| Output location | the workgroup's | Where results go in S3 (`s3://bucket/prefix/`), for a workgroup that names no place of its own. A workgroup that enforces its settings ignores it. |
| Stop queries above | no limit | In GB scanned; see [What a query costs](#what-a-query-costs). |

**Test connection** reads the workgroup: it says which engine runs your queries and, when the
workgroup limits what a query scans, that limit ("Athena engine version 3, eu-west-1, the
workgroup stops queries past 10.0 GB"). A workgroup with nowhere to put results, and a data
source that names none either, is an error there, saying to set an **Output location**.

### The explorer

The explorer lists your catalogs (the Glue Data Catalog, and any federated catalogs), their
databases, then each database's tables and views, then a table's columns, with its partition
keys last, marked *partition key*. A table shows what it is kept as: *Iceberg*, *Delta*, or its
files' format, such as *parquet*. All of it comes from Athena's catalog API, which scans no data
and costs nothing; the filter box is sent to Athena for a database's tables.

- **Open data** opens a console with `SELECT * FROM "AwsDataCatalog".sales.orders LIMIT 1000`
  and runs it. Athena runs a query to its end, and bills what it scanned, however few rows you
  look at, so DbRover never reads a table whole unasked.
- **Show definition** runs `SHOW CREATE TABLE` or `SHOW CREATE VIEW`, which scan nothing. It is
  offered in the data source's own catalog, where its queries run.

### The console

Each statement runs as an Athena query in the data source's workgroup, catalog and database. The
status line shows what it has scanned while it runs, and each finished statement says so beside
its rows and time: `500 rows · 2.1 s · 1.4 GB read`. Rows come back 500 at a time; **Cancel**
(⌘.) stops the query in Athena.

Athena can check a statement only by running a query, so DbRover doesn't ask it as you type: the
syntax check still underlines what it can read, and Athena's own errors show, with their place
in the statement, when it runs. There are no transactions, and rows aren't edited in the grid:
Athena's tables have no key that tells their rows apart.

### What a query costs

Athena bills each query by the bytes it scanned (at least 10 MB), whatever it returns; failed
queries and DDL cost nothing, and a cancelled one what it scanned until then. A workgroup can
limit what a query scans, and Athena then cancels any query that scans more. That limit is
exact; set it in the workgroup where it matters.

Where the workgroup sets none, **Stop queries above** is DbRover's: a query that has scanned
more is stopped, and the statement says what it had scanned. DbRover asks Athena how far a query
has got every two seconds at most, so a query may scan up to two seconds' more than the limit,
and Athena bills that. **Run to file** and AI assistants are held to the same limit. The limit
is kept in `data_sources.toml` as `ask_above_bytes`.

### Safety and AI assistants

Athena has no read-only mode. A **read-only** data source refuses statements that change data
or tables before they are sent; for AWS itself to refuse them, sign in with a profile whose IAM
policy allows Athena's queries and reading Glue and the tables' S3 data, and writes nothing but
query results. An AI assistant is served an Athena data source through its **MCP profile**, as
for DynamoDB.

### Not tried yet

DbRover's Athena support was built and tested against Athena's documented API, without an AWS
account. These are still to be tried against Athena itself:

- a query's first row repeating the column names, which DbRover leaves out;
- the bytes scanned growing while a query runs, rather than appearing at its end;
- an `INSERT`'s count of rows, and the place errors point at;
- federated catalogs, Iceberg tables, and `SHOW CREATE VIEW` on views with odd names.

## Trino, Starburst and Presto

### Connecting

Add a **Trino / Presto** data source, or **Starburst Galaxy** under **Other cloud services**.

| Field | Default | Notes |
| --- | --- | --- |
| Host, Port | `localhost`, `8080` | The coordinator. HTTPS is usually on 443 (Starburst Galaxy) or 8443. |
| Catalog | none | The catalog a console starts in. Empty: names must say their catalog until `USE` picks one. |
| User | `trino` | Trino asks for a user on every request. On Starburst Galaxy it is your email and a role: `ada@example.com/accountadmin`. |
| Password | none | Sent only over HTTPS: a password with TLS `disable` is refused, as Trino would refuse it. |
| TLS | `disable` | `require` or `verify-full` for HTTPS. |
| Schema | none | The schema a console starts in. |
| Protocol | Trino | **Presto** for a Presto server, which spells the protocol's headers `X-Presto-…`. Starburst takes Trino's. |

An SSH tunnel works as for any server.

### The explorer and the console

The explorer lists the coordinator's catalogs (Trino's own `system` catalog is hidden unless
**System** is ticked), then each catalog's schemas, tables and views, and columns, from the
catalog's `information_schema`. **Open data** runs `SELECT * FROM catalog.schema.table LIMIT
1000`; **Show definition** runs `SHOW CREATE TABLE` or `SHOW CREATE VIEW`.

A console keeps what Trino keeps for a client: `USE`, `SET SESSION`, `PREPARE` and
`START TRANSACTION` last until you change them or disconnect. A statement is checked as you type
with `EXPLAIN (TYPE VALIDATE)`, which plans it without running it, so a misspelt table or column
is underlined before you run it. Rows are read as you page through them: a result you stop
reading is cancelled, and **Cancel** (⌘.) stops a running query. The status line shows the rows
and bytes read so far, and each finished statement what it read. Rows aren't edited in the grid.

### Safety and AI assistants

Trino has no read-only session. A **read-only** data source refuses changes before they are
sent; for Trino to refuse them, sign in as a user whose access control allows only reading.
An AI assistant is served a Trino data source through its **MCP login**.

### Not tried yet

The Trino driver is tested against Trino 484 every night. Starburst Enterprise, Starburst Galaxy
and Presto haven't been tried yet: connecting to each, a password over HTTPS, and Presto's
headers on a Presto server.
