# A cookbook of worked examples

Status: **planned on 2026-11-28** (D10a); the twelve open questions at the end wait for answers
before any recipe is written. The [roadmap](roadmap.md#design-and-documentation-track) tracks it
as D10 in the design and documentation track.

The manual is a reference: one chapter per feature, each saying what the feature does, every
setting it has and what happens at its edges. That suits someone who already knows which
feature they need. It suits less well someone who starts from a goal: "send last month's sales
to finance as an Excel file, every month", "find out why the orders page is slow", "let the new
analyst use our databases without giving them my passwords". Each of those needs three to five
chapters, read in an order the manual doesn't give, and the reader has to know that a report,
`dbrover report run` and cron fit together before they can look any of them up.

Only [Reports](../book/src/reports.md) has worked examples today: six report files in
`examples/reports/`, the made-up bike shop they read, and a first report walked through from an
empty SQLite file to an Excel file with a known total. The tests keep it true, because
`crates/dbrover-engine/tests/report_examples.rs` runs every example and compares what it writes
with the files checked in beside it.

D10 adds a **Cookbook** part to the manual: task-led recipes that start from a real goal and
walk the reader to a result they can check, on one sample dataset that loads onto the local
test servers with one command, linking to the reference chapters for every detail instead of
repeating them.

## The question

Who opens a cookbook, and what do they want from a recipe?

1. **Someone new to DbRover with a job to do.** They know their databases and SQL. They want
   the shortest path from the goal to the result, with what to click and type, and a number at
   the end that tells them it worked.
2. **Someone who knows DbRover's features one by one, but not how they combine.** A saved query
   with parameters, a report, the command line and a scheduler each have a chapter; the recipe
   is the glue.
3. **Someone deciding whether DbRover can do a thing at all.** "Can it reach Aurora with IAM?"
   They read the recipe's first lines and its *Not tried* section, and need both to be honest.

From that, a recipe must: name the goal in its title; say up front what the reader needs
(servers, programs, accounts); give steps that work in order on the sample data; state what the
reader should see, with exact numbers where there are numbers; offer the common variations in a
line each; and link to the chapters that explain each step in full. It must not restate a
chapter's reference material, and it must not claim a step works where nobody has tried it.

## What the reader sees

### The part in the manual

A new part in `book/src/SUMMARY.md`, right after **Getting started** (question 1), with its pages
under `book/src/cookbook/`:

```text
# Cookbook

- [The cookbook](cookbook/README.md)
- [The sample shop](cookbook/shop.md)
- [Configuration](cookbook/configuration.md)
  - [Reach a database through an SSH tunnel](cookbook/ssh-tunnel.md)
  - [Keep a password in your password manager](cookbook/password-command.md)
  - [Make production hard to change by accident](cookbook/careful-production.md)
  - [Give a teammate your data sources, without your passwords](cookbook/share-data-sources.md)
- [Querying](cookbook/querying.md)
  - [From a question to a chart](cookbook/question-to-chart.md)
  - [A query you run with new values](cookbook/parameterised-query.md)
  - [Make a slow query fast from its plan](cookbook/slow-query.md)
  - [Find where a value lives](cookbook/find-a-value.md)
- [Reports](cookbook/reports.md)
  - [Last month's sales in Excel, every month](cookbook/monthly-sales.md)
  - [This year's sales, half of them archived in S3](cookbook/archive-in-s3.md)
  - [Test a report before it ships](cookbook/test-a-report.md)
- [Cloud](cookbook/cloud.md)
  - [Amazon RDS and Aurora with IAM sign-in](cookbook/rds-iam.md)
  - [Query Parquet files in S3](cookbook/parquet-in-s3.md)
  - [BigQuery without a surprise bill](cookbook/bigquery-cost.md)
  - [Snowflake and Databricks without a surprise bill](cookbook/warehouses.md)
- [Operations](cookbook/operations.md)
  - [Last night's dump, restored into a scratch database](cookbook/nightly-dump.md)
  - [Find the session that blocks everyone, and stop it](cookbook/blocking-session.md)
```

**The cookbook** says what a recipe is, how to load the sample data (`just shop`, or by hand
without the repository), and lists every recipe with its goal in one line. **The sample shop**
describes the data: which tables are where, how many rows, and the few numbers recipes come back
to (October's net sales, the top customer). Each group's page is a short list of its recipes, so
the sidebar can fold them.

### A recipe's shape

Every recipe has the same sections, in this order, so a reader who has done one knows where to
look in the next:

```markdown
# Find the session that blocks everyone, and stop it

The orders page hangs. Somebody's transaction holds a lock on `shop.order_lines`, and every
update behind it waits. Find that session, see what it ran, and end it.

**You need:** the sample shop on PostgreSQL (`just shop`), and two consoles on **shop-postgres**.

## Steps

1. In the first console, start a transaction that holds the lock:
   {{#include ../../../examples/cookbook/blocking-session.sql:hold}}
2. ...

## What you should see

Server activity lists two sessions; the second is *waiting*, under the first in the tree...

## Variations

- **On MySQL or MariaDB:** ...
- **When you may not see other users' sessions:** ...

## See also

- [Server activity](../server-activity.md): every column, Cancel query and Kill.
- [Transactions](../consoles.md#transactions)
```

Cloud recipes add a line under the title saying what they were tried against, and end with a
**Not tried against … yet** section, as [Amazon Web Services](../book/src/aws.md#not-tried-against-aws-yet)
does. Recipes walked through by hand end with *Walked through with DbRover 0.41 on 2026-11-…*,
so a reader can tell how fresh a recipe is.

Recipe titles say the goal in the reader's words, not the feature's name (question 12): *Make a
slow query fast from its plan*, not *Explain plans*.

## The recipes

The candidate list from the request, refined: two reports recipes that the Reports chapter
already works through (*a report built on another*, and *a report run in CI*) become variations
or a different recipe, and Snowflake and Databricks share one recipe, since its steps are the
same and neither can be tried (question 2). Seventeen recipes in five groups.

**How checked** says what keeps each recipe true: **test** is a check in a test binary CI
already runs (see [Keeping recipes correct](#keeping-recipes-correct)); **walked** is a walk
through by hand on the sample data when the recipe is written; **stand-in** is a local service
standing in for the cloud one; **not tried** is said in the recipe.

### Configuration

| Recipe | Goal | On | How checked |
| --- | --- | --- | --- |
| Reach a database through an SSH tunnel | A PostgreSQL server only reachable from a jump host | The compose `ssh` service, in front of `postgres` | Its data source parses (test); walked |
| Keep a password in your password manager | No database password in DbRover's keychain or files | **Password from a command** with macOS's `security` and Linux's `secret-tool`; 1Password's `op` and Vault as variations | macOS and Linux walked; `op` and Vault not tried |
| Make production hard to change by accident | A red, careful data source; a read-only login for reports | shop-postgres | Walked |
| Give a teammate your data sources, without your passwords | Export, import, `${VARIABLE}` placeholders, the settings folder in Git | The cookbook's `data_sources.toml` | Its file parses (test); walked |

### Querying

| Recipe | Goal | On | How checked |
| --- | --- | --- | --- |
| From a question to a chart | "Who are our best customers, month by month?" as a query, a stacked bar chart, a PNG | shop-postgres | Its query's rows (test); walked |
| A query you run with new values | A saved `.sql` file with `:since` and `:until` | shop-postgres | Its query with the recipe's values (test); walked |
| Make a slow query fast from its plan | A sequential scan found in the plan, an index added, the plan read again | shop-postgres's `visits` table | The plan's node before and after (test); walked |
| Find where a value lives | "Where does `cust-07` appear?" with **Search data** | shop-postgres | The tables it names (test); walked |

### Reports

| Recipe | Goal | On | How checked |
| --- | --- | --- | --- |
| Last month's sales in Excel, every month | `sales-by-category` run by `dbrover report run` from cron or launchd; a quarterly report built on it as a variation | The SQLite shop | The report (existing test); the cron line not run |
| This year's sales, half of them archived in S3 | September to November in PostgreSQL, January to August as Parquet in a bucket, in one report | shop-postgres and MinIO | The report against its checked-in CSV (test) |
| Test a report before it ships | A repository of reports whose CI runs each one on a small fixture database and compares its CSV with a checked-in one | The SQLite shop | The job's commands run by hand on Linux; the workflow not run on GitHub |

### Cloud

| Recipe | Goal | Stand-in | How checked |
| --- | --- | --- | --- |
| Amazon RDS and Aurora with IAM sign-in | No password at all: a token from your AWS sign-in | None | Not tried, said at the top |
| Query Parquet files in S3 | A bucket's Hive-partitioned Parquet, Delta and Iceberg tables, queried in DuckDB | MinIO | MinIO (existing tests and walked); S3 itself not tried |
| BigQuery without a surprise bill | The byte limit, the question above it, a read-only service account | The BigQuery emulator, for browsing only | The cost guard not tried: the emulator ignores it |
| Snowflake and Databricks without a surprise bill | Their cost guards, a small warehouse, a read-only role | None | Not tried, said at the top |

### Operations

| Recipe | Goal | On | How checked |
| --- | --- | --- | --- |
| Last night's dump, restored into a scratch database | A saved dump on a schedule, then **Restore…** into a new database to look at yesterday's rows | shop-postgres, with `pg_dump` 17 installed | Its `dumps.toml` parses (test); walked |
| Find the session that blocks everyone, and stop it | Two consoles, one holding a lock; **Server activity** shows the tree; **Kill** ends it | shop-postgres | Walked |

## The sample shop

One dataset, the made-up bike shop that `examples/reports/shop.sql` already makes: eight
products, twelve customers and 720 order lines from 2026-09-01 to 2026-11-30, every value from a
formula, none of it anyone's real data. The cookbook puts the **same rows** wherever a recipe
needs them, so October's net sales are 68,435.76 in every one, and a reader who moves from the
SQLite recipe to the PostgreSQL one sees the same numbers.

| Where | Data source | What | From |
| --- | --- | --- | --- |
| A SQLite file | **shop** | `products`, `order_lines` | `examples/reports/shop.sql`, as it is |
| The compose `postgres`, schema `shop` | **shop-postgres** | `products`, `customers`, `order_lines` (the same rows), and `visits`, 200,000 page views for the slow-query recipe (question 5) | New: `examples/cookbook/shop-postgres.sql` |
| The compose `ssh`, in front of `postgres` | **shop-via-ssh** | The same schema, reached through the tunnel | The same database |
| The compose `mongo` | **crm** | `customers`, with addresses | `examples/reports/shop-customers.js`, as it is |
| The compose `minio`, bucket `dbrover` | **lake** | `shop/archive/month=2026-01/` to `month=2026-08/`: order lines for January to August as Parquet, partitioned as Hive does (question 4); the existing `sales/`, `warehouse/products` (Delta) and `warehouse/shop/orders` (Iceberg) | New Parquet, made by a checked-in DuckDB script |

PostgreSQL gets a `customers` table of its own, the twelve names of `shop-customers.js`, so the
querying recipes need one server only; MongoDB stays for the report that joins across engines.
The BigQuery emulator keeps its own small dataset, since its recipe is about the cost guard and
can't show it anyway; the emulator also starts only with the compose file's `nightly` profile.

`shop-postgres.sql` computes the order lines with `generate_series` and the same arithmetic as
`shop.sql`'s recursive query (integer division, the `% 10` returns, the cents that vary by
line), so the two agree row for row; a test holds them to it (below).

### Loading it: `just shop`

```bash
just shop
```

does, in order:

1. `docker compose up -d --wait postgres mongo minio ssh`: only the four services the cookbook
   uses, not all of the test servers.
2. Loads `shop-postgres.sql` with `docker compose exec -T postgres psql -v ON_ERROR_STOP=1`,
   replacing the `shop` schema, and `shop-customers.js` with `mongosh` in the `mongo`
   container. Nothing has to be installed on the machine for these two.
3. Makes `target/cookbook/shop.db` from `shop.sql` with the `sqlite3` program (question 7).
4. Writes a settings folder, `target/cookbook/settings/`: `data_sources.toml` from the template
   `examples/cookbook/data_sources.toml`, with a fixed ID for each data source, and the example
   reports copied into its `reports/` folder.
5. Prints how to start DbRover on it: `just run --config-dir "$PWD/target/cookbook/settings"`.

Running it again starts over: the schema, the collection and the file are replaced. The MinIO
bucket is mirrored from `docker/minio/dbrover/` each time the service starts, as it is today.
`cargo clean` removes the SQLite file and the settings folder with the rest of `target/`.

The data sources' passwords (all `dbrover`, the test servers' own) are asked for once and saved
to the keychain under the fixed IDs, so they keep working after the next `just shop`
(question 6). The SQLite file's path in the template is `${DBROVER_COOKBOOK}/shop.db`, a
[placeholder](../book/src/data-sources.md#placeholders-variable) that `just run` sets; the
*Give a teammate your data sources* recipe uses the same file to show placeholders.

A reader without the repository can still follow most recipes: **The sample shop** links the
scripts, and says how to run each in its data source's console, as Reports' first report does
today.

`just` runs its recipes with `sh`. The recipe is written for macOS and Linux; on Windows it runs
from Git Bash, which the manual says, and isn't tried in CI.

## Keeping recipes correct

A recipe that goes stale is worse than none: the reader follows it, it fails, and they stop
trusting the rest. CI time is a budget, so the checks add **no job and no service**, and run in
a test binary that CI already builds and runs against the compose servers.

### What the reader copies comes from a checked file

Every SQL statement, report, `data_sources.toml` entry, `dumps.toml`, cron line and workflow a
recipe asks the reader to type or copy lives in a file under `examples/cookbook/`, and the page
pulls it in with mdBook's `{{#include file:anchor}}`, marking the part it shows with
`ANCHOR: name` and `ANCHOR_END: name` comments. The text in the book is then the text the tests
run; there is no second copy to forget.

mdBook doesn't fail on a broken include: tried with mdBook 0.5.4, a file it can't read is logged
as an error and the build still ends well, and an anchor that isn't in the file includes
nothing, silently. So the check below also reads every `{{#include …}}` in `book/src/cookbook/`
and fails if a file or an anchor is missing, or if an anchor's part is empty.

### The checks

All in `crates/dbrover-engine/tests/report_examples.rs`, which already runs the example reports
"so the manual's examples keep working", renamed `examples.rs` if it outgrows its name
(question 8):

- **The two shops agree.** `shop-postgres.sql` is loaded on the compose PostgreSQL into a schema
  of the test's own, and `sales-by-category.toml` is run with **shop** pointing at it; its CSV
  must equal the same `expected/sales-by-category-2026-10.csv` that the SQLite run is compared
  with.
- **Each recipe's numbers.** Each query file is run on the sample data and its rows compared
  with a CSV in `examples/cookbook/expected/`, the rows the recipe's *What you should see* shows.
  `DBROVER_UPDATE_EXPECTED=1` rewrites them, as it does the report examples'.
- **The slow-query recipe's plan.** The plan's top scan is a `Seq Scan` on `visits` before the
  recipe's index and an `Index Scan` (or `Bitmap Heap Scan`) after it, read through DbRover's
  own plan parser.
- **The archive report** runs on shop-postgres and MinIO and matches its checked-in CSV.
- **Every file parses:** the cookbook's `data_sources.toml` through `dbrover-store`'s reader,
  `dumps.toml` through the saved dumps' reader, and each report through `read_report`.
- **Every include resolves**, as above. This one needs no server.

Each check skips when its server's `DBROVER_TEST_*_URL` is unset, as every server test does, so
`cargo test` still passes on a laptop with nothing running. On CI's Linux runner, where
`postgres`, `mongo` and `minio` already start, they add a few seconds to one binary.

The `examples/` folder isn't in CI's documentation-only list, so a layer that changes an example
file runs the full build and tests, which is what it should do.

### What stays a walk-through

The clicks are not tested end to end: no kittest scene drives a whole recipe. Each recipe is
walked through by hand on `just shop`'s data when it is written, and dated (see
[A recipe's shape](#a-recipes-shape)). The reference chapters' own pictures and tests keep each
dialog true, and a recipe links to them rather than describing every field.

A recipe that a later change breaks is caught by the tests when its files no longer run or give
other numbers, and by the reader otherwise. Each milestone's manual layer already updates the
chapters it touches; from D10 on, it also searches the cookbook for the feature's name.

## Cloud recipes

Cloud recipes can't be verified without real accounts, and D10 doesn't use or read any:
nothing in `~/.aws`, `~/.config/gcloud`, Azure's caches or a Databricks token. Where a local
stand-in exists, the recipe is walked through on it; everywhere else it says so, at the top
and in full at the foot:

| Recipe | Tried on | Not tried against the real service |
| --- | --- | --- |
| RDS and Aurora with IAM | Nothing | Everything: the token from **Sign in with › AWS**, TLS `verify-full` with the RDS bundle, an Aurora reader endpoint |
| Parquet in S3 | MinIO, with the access key `just shop` uses | An AWS profile with IAM Identity Center, a public bucket, Iceberg and Delta tables written by other engines |
| BigQuery without a surprise bill | The emulator, for signing in, browsing and querying | The dry run's estimate, the question above the limit, a query refused for its maximum bytes billed |
| Snowflake and Databricks | Nothing | Everything: sign-in, the cost guards, a read-only role |

The steps of the untried recipes come from the reference chapters, which were built against the
services' documented APIs and say the same. A recipe that has never run is still worth having
(question 10): it puts the chapters' parts in order, and its *Not tried* section tells a reader
exactly what to check first. When someone tries one against the real service, the section
shrinks and the date says when.

## How it's built

- **`examples/cookbook/`:** `shop-postgres.sql`; `archive.sql`, the DuckDB script that writes the
  archive's Parquet; `data_sources.toml`, the settings template; one file per recipe that has
  something to copy (`top-customers.sql`, `orders-since.sql`, `slow-query.sql`,
  `archive-report.toml`, `nightly-dump.toml`, `monthly-sales.cron`, `test-a-report.yml`, …);
  and `expected/`.
- **`docker/minio/dbrover/shop/archive/`:** the Parquet files, a few kilobytes each, and a line
  in `docker/minio/README.md` saying how they were made. `fake-gcs` serves the same folder, so
  it gains them too.
- **`justfile`:** the `shop` recipe, in `sh`, with a check for `sqlite3` that says how to install
  it.
- **`crates/dbrover-engine/tests/report_examples.rs`:** the checks above, beside the report
  examples' own.
- **`book/src/cookbook/`** and the part in `SUMMARY.md`.

No crate's code changes.

## Options weighed

### One dataset, or one per recipe

A dataset per recipe could fit each goal exactly (a real lock-heavy schema for the blocking
recipe, a wide table for the slow query). One shop means a reader loads data once, learns its
tables once, and sees the same numbers across recipes, and one script to keep correct. The shop
gains one table, `visits`, for the only recipe it can't serve; everything else fits it.

### The book's SQL tested in place, or in files the book includes

A test could parse the fenced SQL blocks out of the Markdown and run them. That needs a parser
for the book, a way to say which data source each block runs on, and conventions for blocks that
aren't meant to run. Files with anchors, included by mdBook, give the same guarantee with a
plain file per recipe that the tests already know how to run, and the file is also a link a
reader can download.

### A new CI job, or the existing test binary

A job that brings up the servers, runs `just shop` and checks every recipe would test more,
including the `just` recipe itself, at the cost of a runner's start, the compose services' start
and a build, on every pull request. The existing engine tests already have the servers and the
build; the checks above add seconds to them. What they leave out, `just shop`'s shell, is run by
hand in each cookbook layer.

### Pictures in every step, or few

A picture per step goes stale with every look-and-feel change, and the reference chapters
already show each dialog, rendered by the screenshot test so they never go stale. A recipe shows
at most one picture of its own, of the result where it is the point (the chart, the activity
tree), from a new scene in `screenshots.rs` on the shop; otherwise it links to the chapter's
(question 9).

## Layers

A stack of small pull requests, one per group, each passing `mdbook build` and the five required
checks:

1. **D10a — The plan:** this page and the roadmap's entry.
2. **D10b — The sample shop:** `shop-postgres.sql`, the archive's Parquet and its script, the
   settings template, `just shop`, the checks that the two shops agree, that the files parse and
   that includes resolve; the Cookbook part with **The cookbook** and **The sample shop**.
3. **D10c — Configuration:** four recipes.
4. **D10d — Querying:** four recipes, their query files and expected rows, the plan check.
5. **D10e — Reports:** three recipes, the archive report and its CSV.
6. **D10f — Cloud:** four recipes, each with what it was tried on.
7. **D10g — Operations, and the demo note:** two recipes; the roadmap's demo note.

**Exit:** on a clean checkout, `just shop` loads the shop into SQLite, PostgreSQL, MongoDB and
MinIO and writes a settings folder that DbRover opens with every data source connecting;
`mdbook build` passes with every recipe in the Cookbook part; the checks pass on CI's Linux
runner; and every recipe is either dated as walked through on that data, or says at its top and
foot what wasn't tried against the real service.

## What stays out

- **A recipe per engine.** Recipes use PostgreSQL where a server is needed and say in
  *Variations* what differs on MySQL, MariaDB or SQL Server; the reference chapters cover each
  engine.
- **Recipes for AI assistants** (*let an assistant read the shop but not change it*). A good
  candidate for a group of its own once these land.
- **A guided tour inside the app.** The cookbook is the manual's.
- **Real cloud accounts in CI** or in the walk-throughs.
- **Moving `examples/reports/`.** Its files are linked from the Reports chapter and named by
  tests; the cookbook adds `examples/cookbook/` beside it and reuses the shop scripts where they
  are.

## Open questions

Each has a proposed answer; nothing beyond this plan is written until they are answered.

1. **Where the part goes.** Right after **Getting started**, as the second thing a new reader
   meets; or before **Reference**, after every feature chapter? Proposed: after Getting started.
2. **The recipe list.** The seventeen above: *a report built on another* as a variation of the
   monthly report, since Reports works one through already; *a report in CI* reshaped as *Test a
   report before it ships*, since Reports' **In CI** already shows a job that runs one; Snowflake
   and Databricks in one recipe. Add or drop any? Proposed: as listed.
3. **Where the PostgreSQL shop lives.** A `shop` schema in the compose `dbrover` database, which
   every test URL already names; or a database `shop` of its own? Proposed: a schema, with the
   tests loading the same script into a schema of their own so they never touch the reader's.
4. **The archive in S3.** New Parquet files for January to August under
   `docker/minio/dbrover/shop/archive/`, made once by a checked-in DuckDB script and committed
   (a few kilobytes); or written by `just shop` at load time, which needs DuckDB's command-line
   program on the machine? Proposed: committed, as the bucket's other fixtures are.
5. **Data for the slow-query recipe.** A `visits` table of 200,000 rows, in PostgreSQL only,
   enough for the plan to change from a sequential scan to an index scan and for the time to
   differ visibly; or a larger `order_lines`, which would change every report's numbers?
   Proposed: `visits`.
6. **Passwords for the cookbook's data sources.** Asked once and kept in the keychain under the
   template's fixed IDs; or a password command that prints `dbrover`, which asks to be trusted
   once and differs between macOS, Linux and Windows? Proposed: the keychain, as any data source.
7. **The SQLite file.** Made by `just shop` with the `sqlite3` program (on macOS and most Linux
   systems already); or left to the reader in the app, as Reports' first report does?
   Proposed: `just shop` makes it, and **The sample shop** also says how to make it in the app.
8. **The checks.** In the existing `report_examples.rs` engine test binary, with includes
   checked there too, and no new CI job; or a job that runs `just shop` and every recipe?
   Proposed: the existing binary.
9. **Pictures.** At most one per recipe, of its result, from a new screenshot scene on the shop,
   and the reference chapters' pictures linked otherwise; or none at all? Proposed: at most one.
10. **Cloud recipes nobody has run.** RDS with IAM, Snowflake and Databricks included, with
    *Not tried against … yet* at the top and in full at the foot; or left out until someone tries
    them? Proposed: included, marked.
11. **Its place on the roadmap.** D10 in the design and documentation track, after D9? Proposed:
    D10.
12. **Recipe titles.** The goal, in the reader's words (*Make a slow query fast from its plan*);
    or the feature's name (*Explain plans*), matching the reference chapters? Proposed: the goal.
