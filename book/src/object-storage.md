# Object storage and MotherDuck

A DuckDB data source can read a bucket in object storage: Parquet, CSV and JSON files, and
Iceberg and Delta tables, in Amazon S3 (or a store that speaks its API, such as MinIO or
Cloudflare R2), Google Cloud Storage or Azure Blob Storage. The bucket shows in the explorer
beside the database, and a query can join its files with the database's own tables. A DuckDB
data source can also be a database on MotherDuck, hosted DuckDB.

DuckDB does the reading, with extensions it downloads the first time it needs them; DbRover
hands it short-lived credentials from the sign-in already on your machine, and lists the
bucket for the explorer.

## Reading a bucket

In the DuckDB data source dialog (see [Data sources](data-sources.md)), tick **Read from a
bucket**. The database can be a file, in memory or on MotherDuck; a bucket and an in-memory
database make a console for a data lake.

| Field | Provider | Notes |
| --- | --- | --- |
| Provider | all | **Amazon S3**, **Google Cloud Storage** or **Azure Blob Storage**. |
| Bucket name | S3, Google Cloud Storage | Such as `sales`, without `s3://`. |
| Region | S3 | Empty: the profile's region, or `us-east-1`. Set it for a bucket in another region: the explorer finds such a bucket anyway, but DuckDB's reads need it. |
| Endpoint | all | Empty for the provider's own. An S3-compatible store's address (`http://localhost:9000` for MinIO, `https://<account>.r2.cloudflarestorage.com` for R2), whose buckets are reached by path; an Azure private endpoint. |
| Account, Container | Azure | The storage account (the first part of `<account>.blob.core.windows.net`) and the container. |
| Sign in with | all | See [Signing in](#signing-in). |

Every field may hold `${NAME}` placeholders, as other settings do.

## Signing in

DbRover never stores a cloud secret. It signs in as [Cloud sign-in and costs](cloud.md)
describes, makes DuckDB a *temporary secret* from what it gets, scoped to the bucket, and makes
it again before it expires. DuckDB keeps the secret in memory only: DbRover turns off DuckDB's
persistent secrets, so none is ever written to `~/.duckdb`, and `duckdb_secrets()` shows its
values as `redacted`.

| Provider | Sign in with | What DuckDB gets |
| --- | --- | --- |
| Amazon S3 | **AWS (default chain)** | The keys AWS's SDK finds: the environment, the `default` profile, IAM Identity Center, a role. |
| | **AWS profile** | One profile of `~/.aws/config` and `~/.aws/credentials`, alone, as `aws --profile` uses it. |
| | **Access key** | An S3-compatible store's key: **Access key** is its id, and **Secret key** is kept in the keychain as the data source's password. |
| | **None (public)** | Nothing: for public buckets, such as open datasets. |
| Google Cloud Storage | **Application Default Credentials** | A token from `gcloud auth application-default login`'s file, or the one `GOOGLE_APPLICATION_CREDENTIALS` names. |
| | **Credentials file** | A token from a service account's key file. |
| | **None (public)** | Nothing. |
| Azure Blob Storage | **Microsoft Entra ID** | A token for Azure Storage from `az login`'s or `azd auth login`'s sign-in, with an optional tenant. Your account needs a data role on the container, such as *Storage Blob Data Reader*. |
| | **None (public)** | Nothing, for a container that allows public reads. |

A token is asked for once and shared by every console on the same sign-in until five minutes
before it expires.

## The explorer

The bucket is a node beside the database, labelled with its name. Its folders and files load
one level at a time, a thousand at a time, so a bucket of millions of objects opens as fast as
a small one; **Load more…** reads the next thousand. The filter box keeps the names that
contain what you type, a thousand objects at a time, and **Load more…** looks further.

- A **file** shows its size. A file DuckDB reads as a table (`.parquet`, `.csv`, `.tsv`,
  `.json`, `.jsonl`, `.ndjson`, gzip- or zstd-compressed) expands to its columns, read from its
  footer or first rows, and has **Open data**, which opens a console with
  `SELECT * FROM 's3://sales/2026/orders.parquet';` and runs it, a page at a time. Files in
  folders named like `year=2026` get a `year` column, as DuckDB reads Hive partitions.
- A **folder** holding a Delta table's `_delta_log/` or an Iceberg table's `metadata/` starts
  with the **table**, named after the folder, with *Delta table* or *Iceberg table* beside it.
  **Open data** reads it with `delta_scan('…')`, or with `iceberg_scan('…')` on the table's
  newest metadata file; it expands to its columns. The folder's own files stay listed below it.

## Querying

In a console, a bucket's files are tables named by their address:

```sql
SELECT count(*) FROM 's3://sales/2026/*/*.parquet';
SELECT * FROM read_csv('gs://logs/exports/customers.csv');
SELECT o.*, c.name
FROM 's3://sales/orders/*.parquet' o
JOIN customers c USING (customer_id);
SELECT * FROM iceberg_scan('s3://lake/warehouse/orders/metadata/00002-….metadata.json');
SELECT * FROM delta_scan('az://raw/products');
```

The data source's credentials apply to its own bucket. To read another, make a secret of your
own in the console (`CREATE SECRET (TYPE s3, KEY_ID …, SECRET …, SCOPE 's3://other')`);
it lasts as long as the console's connection. Writing works too (`COPY … TO 's3://…'`), on a
data source that isn't read-only.

DbRover checks a statement as you type by asking DuckDB to prepare it, except a statement that
names a remote address (`s3://`, `https://`, …): preparing it would read the file from the
bucket at every pause. Its errors show when it runs.

## Extensions, and working offline

DuckDB reads buckets and table formats with extensions it downloads from
`extensions.duckdb.org` the first time a statement needs them, checks against DuckDB's
signature, and keeps in `~/.duckdb/extensions/<version>/<platform>/`, shared with DuckDB's
command-line client:

| Extension | For | Size |
| --- | --- | --- |
| `httpfs` | S3, Google Cloud Storage, `https://` | 16 MB |
| `azure` | Azure Blob Storage | 13 MB |
| `iceberg` (and `avro`) | Iceberg tables | 34 MB and 8 MB |
| `delta` | Delta tables | 65 MB |
| `motherduck` | MotherDuck | 10 MB |

Without a network, a statement that needs an extension DuckDB doesn't have yet fails with a
message saying which. The database's own tables still work, and the bucket's credentials are
handed over once the extension arrives. To install one by hand, download it from
`http://extensions.duckdb.org/v1.5.6/<platform>/<name>.duckdb_extension.gz` on another machine,
unpack it, and run `INSTALL '/path/to/<name>.duckdb_extension';` in a console.

Downloading an extension changes no data and costs nothing, so it happens on read-only data
sources too; `INSTALL`, `LOAD`, `CREATE SECRET`, `ATTACH` and `COPY` are refused there, as
administration. Reading a bucket costs what your provider bills for requests and data
transfer: DbRover's own reads are listings, a file's footer, and a page of rows, and nothing
asks before a query of yours reads `**/*.parquet`.

## MotherDuck

Choose **MotherDuck** under **Database**:

| Field | Notes |
| --- | --- |
| Name | The database, such as `analytics`. Empty: your default database, `my_db`. |
| Token | A token from MotherDuck's settings. Kept in the keychain, never in DbRover's files. |

DbRover opens `md:<name>` with the token as DuckDB's `motherduck_token` setting; DuckDB
downloads its `motherduck` extension the first time. The explorer, consoles and the grid work
as on any DuckDB database. A MotherDuck data source can read a bucket too, signed in any way
but an access key, since its one password is the token.

A **read-only** MotherDuck data source has statements checked, but isn't opened read-only, so
the MCP server doesn't serve it; a report can't attach one.

## What wasn't tried

DbRover was built and tested against stand-ins: MinIO for S3, fake-gcs-server for Google Cloud
Storage, and Azure's documented answers for listing a container. Before relying on it, try
with your own account:

- **Amazon S3:** a bucket with an AWS profile (IAM Identity Center or a role, to see the
  credentials renewed after an hour), a public bucket, and Cloudflare R2 with an access key.
- **Google Cloud Storage:** with Application Default Credentials and with a service account's
  key.
- **Azure Blob Storage:** a container with `az login`, through a private endpoint, and a
  public one.
- **Iceberg and Delta** tables written by Spark, Glue, Snowflake or Databricks.
- **MotherDuck:** connecting with a token, browsing, and a read-only data source.
