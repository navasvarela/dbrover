# Cloud sign-in and costs

Databases in the cloud bring three things a database on your own machine doesn't: you sign in
with a cloud account rather than a password, some engines bill for what a query reads, and a
catalog can be too large to walk. This chapter covers what DbRover does about each; each
managed service's own settings are in [Managed databases in the cloud](cloud-services.md).

## Signing in with a cloud account

PostgreSQL, MySQL and MariaDB, and SQL Server data sources can sign in with a short-lived token
from your cloud account instead of a password. Choose it in the data source dialog's **Sign in
with**:

| Sign in with | Engines | For |
| --- | --- | --- |
| Password | all | The password saved in the keychain, or [a command](data-sources.md#password-from-a-command) that prints one. |
| AWS IAM | PostgreSQL, MySQL | Amazon RDS and Aurora users signed in with IAM. |
| Google Cloud | PostgreSQL, MySQL | Cloud SQL's IAM users and service accounts. |
| Microsoft Entra ID | PostgreSQL, MySQL, SQL Server | Azure Database for PostgreSQL and MySQL, Azure SQL. |

DbRover uses the sign-in already on your machine and keeps no cloud secret: only the names you
type (a profile, a file, a tenant) are saved. It fetches a token on every connection (the
explorer, each console, reconnecting), so a token that expired is never reused. A token is
never written to the keychain, a file, the log or the history. A connection stays open after
its token expires, as the databases allow.

### AWS IAM

| Field | Default | Notes |
| --- | --- | --- |
| Profile | AWS's default | A profile of your `~/.aws` files, as for [DynamoDB](dynamodb.md#signing-in). IAM Identity Center (`aws sso login`), `aws login`, roles and `credential_process` all work. |
| Region | the profile's | The database's region, such as `eu-west-1`. |

DbRover signs an RDS authentication token on your machine with the profile's credentials, for
the data source's host, port and user, as `aws rds generate-db-auth-token` does; nothing is
sent to AWS to make it. The database user needs IAM sign-in turned on: `GRANT rds_iam TO
reader` on PostgreSQL, `CREATE USER reader IDENTIFIED WITH AWSAuthenticationPlugin AS 'RDS'`
on MySQL. Give the instance's or cluster's own host name, even through an
[SSH tunnel](data-sources.md#ssh-tunnels): the token is signed for it.

### Google Cloud

| Field | Default | Notes |
| --- | --- | --- |
| Credentials file | Application Default Credentials | A service account's key file of the data source's own. |

Empty, DbRover uses Application Default Credentials, as Google's own tools do: the file
`GOOGLE_APPLICATION_CREDENTIALS` names, else the one `gcloud auth application-default login`
writes. It reads a user's sign-in and a service account's key; workload identity federation
and impersonation aren't supported yet, and say so. The user is the IAM user's name as Cloud SQL
shows it: the email address for a user, and the service account's email without
`.gserviceaccount.com`.

If the sign-in has expired, connecting says to run `gcloud auth application-default login`
again.

### Microsoft Entra ID

| Field | Default | Notes |
| --- | --- | --- |
| Signed in with | Azure CLI | **Azure CLI** (`az login`) or **Azure Developer CLI** (`azd auth login`). |
| Tenant | the CLI's default | A tenant id or domain, when your account is in several. |

DbRover runs `az account get-access-token` (or `azd auth token`) for a token, without a shell,
for Azure SQL on SQL Server and for Azure Database on PostgreSQL and MySQL. Install the CLI and
sign in with it first; an app started from the Finder on a Mac also finds it where Homebrew
puts it. On SQL Server the token signs in by itself, so the **User** field is left as it is; on
PostgreSQL and MySQL the user is the Entra user's or group's name as the database knows it.

### MySQL needs TLS

IAM users on MySQL sign in with MySQL's clear-text password plugin, which sends the token as it
is. DbRover sends it only over TLS that can't fall back to plain text: set **TLS** to `require`
or `verify-full`, or connecting says so. RDS, Cloud SQL and Azure all offer TLS.

### In `data_sources.toml`

The choice is saved as `cloud_sign_in`, beside the other settings, with only what you set:

```toml
cloud_sign_in = { provider = "aws", profile = "analytics", region = "eu-west-1" }
cloud_sign_in = { provider = "google", credentials_file = "~/keys/reader.json" }
cloud_sign_in = { provider = "entra", tool = "azure-developer-cli", tenant = "contoso.onmicrosoft.com" }
```

A data source has a password command or a cloud sign-in, not both; connecting refuses one
that has both. [Exported](sharing.md) data sources take their cloud sign-in with them, and each
machine signs in with its own account.

## Cost guards

Some engines bill for what a statement reads. A data source on such an engine has a cost guard,
which estimates what a statement will cost before running it, and asks first when it is more
than you allowed:

- **Ask before reading a whole table**, on by default, holds back a statement that would read
  every item of a table. [DynamoDB](dynamodb.md#the-scan-guard) is the first engine with it.
- **A byte limit.** Athena says what a query scanned only once it runs, so there the limit
  stops a query that passes it, where the workgroup has no limit of its own: see
  [Athena and Trino](athena-trino.md#what-a-query-costs). BigQuery says before it runs, with a
  dry run, so there the console asks first, and the limit goes with every query for BigQuery to
  enforce: see [BigQuery](bigquery.md#what-a-query-costs). It is kept in `data_sources.toml` as
  `ask_above_bytes = 10000000000`.

A console asks before running a held statement, with what it would read and **Scan anyway**,
which runs it and the statements after it. **Open data** reads a page at a time, which a
whole-table read only bills page by page, so it isn't asked about; a statement over the byte
limit is, since it scans the same bytes however its rows are read. **Run to file**, exporting
**All rows**, [reports](reports.md) and AI assistants can't be asked, so they refuse a held
statement and say why. A report's [profile](reports.md#a-runs-profile) shows the bytes each
source's engine scanned, and its `scanned-more` hint says when a source scanned far more than it
returned.

## Seeing a statement run

While a statement runs on an engine that reports its progress, the console's status line says
what it has done so far, about once a second: `Running… 12 s · 1234 rows · 2.1 GB read`.
ClickHouse reports the rows and bytes it has read, and Athena, Trino and BigQuery their query's
progress. **Cancel** (⌘.) stops the statement as ever. Once it has finished, the line above the
rows says what the statement scanned where the engine tells, as in `412 rows · 900 ms · 2.5 MB
read`.

## Searching a large catalog

The explorer loads a level only when you expand it, a large one [a page at a
time](explorer.md#large-schemas). To find an object in a part you never expanded, type its name
in the filter box: once you stop typing, each connected data source that can search asks its
server, and shows **Found on the server** under its name with what the tree doesn't show
already, up to a hundred, each with its schema beside it.

A match is the object itself: **Open data**, **Show definition**, its other actions and
expanding it all work as in the tree. More than a hundred matches says so; type more of the
name. PostgreSQL searches the tables, views, materialized views and sequences of every schema
of its database; Redshift, the tables and views of its databases through `svv_all_tables`;
SQLite, the tables and views of every attached database. Typing never connects
a data source: connect it first to search it.
