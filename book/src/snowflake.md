# Snowflake

DbRover connects to Snowflake over its SQL API, signed in with a key pair, a programmatic access
token or an OAuth token. Snowflake bills the time a warehouse runs, not the bytes a statement
reads, so DbRover browses without waking your warehouse, and stops a statement that runs past
the limit you set.

## Signing in

Add a data source with **New data source › Other cloud services › Snowflake**. Snowflake's SQL
API takes no password, so a data source signs in one of three ways.

| Field | Default | Notes |
| --- | --- | --- |
| Account | required | The account identifier in its address, `<account>.snowflakecomputing.com`: `myorg-myaccount`, or a locator with its region, such as `xy12345.us-east-2.aws`. |
| User | required | The Snowflake user. |
| Sign in with | Key pair | **Key pair**: DbRover signs a token with your private key, whose public half is set on your user. **Access token**: a programmatic access token generated in Snowflake. **OAuth**: an access token from Snowflake OAuth or your identity provider. |
| Private key file | required for a key pair | The key in PEM, as `openssl genrsa 2048 \| openssl pkcs8 -topk8 -v2 aes-256-cbc` writes it. |
| Passphrase / Token / OAuth token | none | An encrypted key's passphrase, or the token itself, kept in the keychain. DbRover asks the keychain for a passphrase only when the key is encrypted. For OAuth, tick **Token from a command** to run your identity provider's CLI on every connection instead. |
| Warehouse, Role, Database, Schema | the user's defaults | What each statement runs with, until a console's `USE` changes it (see [The console](#the-console)). Snowflake reads these as written, so type them as Snowflake shows them, usually in capitals. |
| Storage integration | none | For reports that hand a large result over through an S3 bucket; see [Reports](#reports). |
| Stop statements after | 10 | In minutes; see [What a statement costs](#what-a-statement-costs). Empty: Snowflake's own limit. |
| MCP role | none | The role [AI assistants](mcp.md) are served through; see [Safety and AI assistants](#safety-and-ai-assistants). |

Each field takes `${NAME}` placeholders.

To make a key pair and set it on your user, as Snowflake's guide describes:

```text
openssl genrsa 2048 | openssl pkcs8 -topk8 -v2 aes-256-cbc -inform PEM -out rsa_key.p8
openssl rsa -in rsa_key.p8 -pubout -out rsa_key.pub
```

then, in Snowflake, `ALTER USER jane SET RSA_PUBLIC_KEY = '…'` with the public key's lines
between its `BEGIN` and `END` lines. A key encrypted with DES3 is refused, with the command
that re-encrypts it with AES. DbRover never reads `~/.snowflake` or a key file you didn't name.

**Test connection** names the warehouse, its size, whether it is suspended, and any statement
timeout set on it: *warehouse ANALYTICS_WH (X-Small, suspended; stops statements after 1 h)*. It
reads them with `SHOW WAREHOUSES`, which doesn't resume a suspended warehouse.

## The explorer

The explorer lists the databases your role can see; under each, its schemas
(`INFORMATION_SCHEMA` only when **System** is ticked); under a schema, three folders,
**tables**, **views** and **stages**; and under a table or view, its columns with their types.
Tables are marked *transient*, *temporary*, *dynamic*, *Iceberg* or *external*, views
*secure*, and a stage shows its bucket's address, or *internal*. Each level loads a thousand at
a time, and the filter box asks Snowflake too.

All of it comes from Snowflake's `SHOW` commands, which need no warehouse and which Snowflake
doesn't bill: browsing never wakes a suspended warehouse. So do **Show definition** and
**Found on the server**:

- **Show definition** shows a view's text, and writes a table's `CREATE TABLE` from its
  columns (types, `NOT NULL`, defaults) and its clustering key. A table's comments and options
  are left out; Snowflake's `GET_DDL` gives them in a console.
- **Found on the server** searches tables and views across the account.
- **Open data** opens a console with `SELECT * FROM SHOP.PUBLIC.ORDERS LIMIT 1000` and runs it.
  This one needs the warehouse: if it is suspended, Snowflake resumes it and bills it for a
  minute at least, as Snowflake's own preview does.

## The console

A console runs Snowflake's SQL. Each statement is sent with the warehouse, role, database and
schema, tagged `DbRover` so you can find DbRover's statements in Snowflake's query history.
Rows come a partition at a time; **Cancel** (⌘.) cancels the statement in Snowflake.

Snowflake's SQL API keeps no session between statements, so DbRover keeps each console's
context itself:

- `USE WAREHOUSE`, `USE ROLE`, `USE DATABASE`, `USE SCHEMA` and `USE db.schema` change what that
  console's later statements run with, and only that console's. DbRover answers them without
  asking Snowflake, so a name that doesn't exist fails on the next statement, with Snowflake's
  error.
- `ALTER SESSION`, session variables, temporary tables and transactions (`BEGIN … COMMIT`) don't
  last past their statement: Snowflake runs them only inside one request of several
  statements. Each statement commits on its own.

Values come as Snowflake's console writes them: `NUMBER` with every digit, dates and times as
`2026-01-02 03:04:05.123` with the column's scale, a `TIMESTAMP_TZ` with its offset (`+01:00`),
a `TIMESTAMP_LTZ` in UTC (marked `Z`, since there is no session time zone to show it in),
`BINARY` in hexadecimal, and `VARIANT`, `OBJECT` and `ARRAY` as JSON. Rows aren't edited in the
grid: Snowflake's keys aren't enforced, so nothing tells a table's rows apart.

Statements aren't checked with Snowflake as you type, only by DbRover's own syntax check: an
`EXPLAIN` needs no warehouse, but Snowflake bills it as cloud services past a daily allowance.

## What a statement costs

A warehouse is billed by the second while it runs, in credits by its size (an X-Small 1 credit
an hour, a Small 2, doubling with each size), with a minute at least each time it resumes; it
suspends itself after its idle time. A credit's price depends on your edition and region.
Nothing says beforehand how long a statement will run, so DbRover asks nothing before it runs.
What it does instead:

- **Stop statements after** goes with every statement as its timeout, and Snowflake stops a
  statement that runs longer, billing the warehouse until then. The statement says which limit
  stopped it. A new data source starts at 10 minutes, a sixth of an hour of its warehouse; a
  warehouse's own lower limit still wins. Clear it, and Snowflake's own limit applies (two days,
  unless your administrator set less).
- Everything DbRover runs by itself is a `SHOW`, which Snowflake doesn't bill, except **Open
  data**, which runs on the warehouse.

The limit is kept in `data_sources.toml` as `stop_after_seconds`.

## Reports

A report reads a Snowflake source's rows as text, a partition at a time. For a large result,
a `[[source]]` can hand it over instead ([Reports at scale](reports.md)): Snowflake unloads it
as Parquet into a folder of an S3 bucket the report names, with `COPY INTO`, and the report
reads the files. Snowflake writes to a bucket only through a *storage integration*, which your
administrator creates for that bucket; name it in **Storage integration**. Google Cloud Storage
and Azure buckets aren't offered yet.

## Safety and AI assistants

Snowflake has no read-only session. A **read-only** data source refuses statements that change
data or objects before they are sent. For Snowflake itself to refuse them, use a role granted
only `SELECT` on the tables and views and `USAGE` on their databases, schemas and the
warehouse. An AI assistant is served a Snowflake data source through its **MCP role**: the data
source's own user, signing in with its own key or token, takes that role, and Snowflake's
grants to it are what limit the assistant.

## Not tried yet

DbRover's Snowflake support was built and tested against Snowflake's documented SQL API,
without a Snowflake account (there is no emulator of the SQL API). These are still to be tried
against Snowflake itself:

- signing in with a key pair (encrypted and not), a programmatic access token and an OAuth
  token;
- a query over several partitions, and the text of `VARIANT`, `GEOGRAPHY` and `VECTOR` values;
- a statement stopped by **Stop statements after**, and its message; whether a limit longer
  than a warehouse's own is cut to it;
- `USE WAREHOUSE` and `USE ROLE` in a console, with names in lower case;
- error messages and the place they point at, and cancel;
- the explorer on a large account, `SHOW … FROM` paging, and **Show definition**;
- a report's handoff through a storage integration.
