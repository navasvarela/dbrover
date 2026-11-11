# Amazon Web Services

## Amazon RDS and Aurora

Choose **Amazon RDS or Aurora PostgreSQL** or **… MySQL** in **New data source › Amazon Web
Services** ([adding one](cloud-services.md#adding-one)). The host is the instance's endpoint, or
for Aurora the cluster's writer or reader endpoint, all ending in `.rds.amazonaws.com`.

- **TLS** is `require`. RDS signs its certificates with its own authority: to check them, download
  AWS's `global-bundle.pem` (from the RDS documentation's "Using SSL/TLS" page), set **TLS** to
  `verify-full` and choose it as the **CA file**.
- **Sign in with** is a password, or **AWS IAM** with a profile and region, as described in
  [Cloud sign-in](cloud.md#aws-iam). IAM sign-in must be turned on for the instance, and the
  user granted it.

RDS and Aurora behave as their engines do; DbRover changes nothing else for them.

## Amazon Redshift

Choose **Amazon Redshift**, for a provisioned cluster or a Serverless workgroup. The host ends
in `.redshift.amazonaws.com` or `.redshift-serverless.amazonaws.com`, the port is 5439, and
every cluster starts with the database `dev`.

Redshift speaks PostgreSQL's protocol, so it is a PostgreSQL data source, but its catalog is
its own. DbRover sees from the server's version that it is Redshift, shows **Amazon Redshift**
as the server, and browses it through Redshift's `svv_all_*` views:

- the databases, each one's schemas, then their **tables** (external tables from Redshift
  Spectrum marked *external*) and **views**, with their columns;
- **Show definition** on a table or view of the connected database shows what `SHOW TABLE` or
  `SHOW VIEW` answers (an external table has none);
- typing in the explorer's filter [searches the server](cloud.md#searching-a-large-catalog)
  through `svv_all_tables`.

Not yet on Redshift:

- **Signing in with AWS IAM.** Redshift hands out temporary credentials through its own API,
  not the RDS token DbRover makes, so sign in with a database user's password, or a
  [password command](data-sources.md#password-from-a-command) that prints one.
- **Editing rows**: a table's data opens read-only.
- **The object helpers** (New table, Modify table and the rest), which write PostgreSQL's DDL,
  aren't offered; write Redshift's DDL in a console.
- Materialized views, functions and procedures aren't listed in the explorer.

## DynamoDB, Athena and S3

The same **Amazon Web Services** menu has [DynamoDB](dynamodb.md) and
[Amazon Athena](athena-trino.md), which sign in with the same AWS profiles. Files in an S3
bucket are read by a DuckDB data source: see [Object storage and MotherDuck](object-storage.md).

## Not tried against AWS yet

Built and tested without an AWS account. To try by hand: an RDS PostgreSQL and an RDS MySQL
instance with a password and with AWS IAM (TLS `require`, then `verify-full` with the bundle);
an Aurora cluster's reader endpoint; and a Redshift cluster and a Serverless workgroup, browsed
down to a table's columns, an external table, **Show definition** and a search.
