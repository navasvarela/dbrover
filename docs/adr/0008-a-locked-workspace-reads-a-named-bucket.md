# 0008. A locked workspace reads a named bucket prefix

- Status: Accepted; qualifies the lock-down RP2 gave the combine step
- Date: 2026-11-15

## Context

A report's combine step runs in a DuckDB of the run's own. Once its sources are loaded, the
workspace is locked down (`Workspace::lock`): `enable_external_access = false` and
`lock_configuration = true`, with `allowed_directories` naming the few folders DuckDB may
still write Parquet in (RS3d, RS5b). RP2 wrote the promise as *the combine query reads only
what was loaded*, which is what makes a report's query safe to run unattended: whatever a
query from a file asks for, the database it runs in cannot reach a network or a file outside
the run's own folders.

A lake breaks that shape. The whole point of reading a partitioned Parquet lake, an Iceberg
table or a Delta table through DuckDB is that the bytes stay in the bucket until the combine
query asks for them: DuckDB's filter and projection pushdown, its row-group statistics and
Hive partitions read only the parts that matter, in parallel range requests
([Reports at scale](../reports-at-scale.md#when-to-delegate), path 2). Loading the lake into
the workspace first would read all of it, which is what RS7 exists to avoid. So the combine
query must reach the bucket while it runs, after the lock.

Tried against the DuckDB in the build (1.5.6) and MinIO on 2026-11-15:

- With `enable_external_access = false`, a read of `s3://bucket/prefix/…` fails with
  *Permission Error: … file system operations are disabled by configuration* unless that
  prefix is in `allowed_directories`, and succeeds when it is — globs, Hive partitions and
  range requests included.
- With the prefix allowed, a read of another prefix of the same bucket, of a local file, and
  of an `https://` address each still fail with that same error, and `INSTALL` and `LOAD`
  of an extension fail too. `allowed_directories` can't be changed afterwards.
- `CREATE OR REPLACE TEMPORARY SECRET` still works after the lock, and a read with the
  replaced secret works, so credentials that expire mid-combine can be renewed.

## Decision

**A locked workspace may read the bucket prefixes the report named, and nothing else.** The
promise becomes: *the combine query reads what was loaded, and the lake prefixes the report's
own file names*.

- A lake is named by a `[[source]]`, whose `data_source` is a DuckDB data source with a bucket
  (C2) and whose `lake` is a path under that bucket. Nothing else puts a bucket in the
  workspace: a path in the combine query's own text reaches no bucket, because it is not in
  `allowed_directories`.
- Before the lock, the run loads the extensions the lake's paths need (`httpfs`, and
  `iceberg` or `delta` for a table format), makes the bucket's temporary secret from C2's
  sign-in, and makes a view over the lake's path. Extensions can't be loaded after the lock,
  so a lake the report doesn't name can't be reached even by an extension that was never
  loaded.
- The lock then allows each lake's prefix, beside the folders it already allowed. The prefix
  is the longest part of the path before its first wildcard, so a source that names
  `sales/year=*/*.parquet` allows `sales/` and not the bucket.
- The secret stays temporary, is scoped to the bucket, and is made again during the combine
  when its credentials are about to expire, which the lock allows.
- The cost guard judges the lake before the run: the bytes its files hold, from the bucket's
  listing and the files' Parquet footers, against the data source's `ask_above_bytes`.

## Consequences

- A report that names a lake can read the bucket's objects under that prefix for as long as
  the combine runs, with the credentials the data source signs in with. The report file is
  the only place that grants it, and a run's profile records the bytes and requests it made,
  so what the lake cost is visible afterwards.
- The failure mode of a wrong path is a DuckDB *Permission Error* rather than a read of
  something unintended, and that error names the path, which the run explains.
- The run's read-only promise is unchanged: the combine query is still one statement that only
  reads, and DuckDB has no write path to the bucket (`COPY … TO` an `s3://` path is refused:
  it isn't a folder in `allowed_directories`, and writing would need external access).
  Handing a warehouse result over through a bucket (RS7f) writes to a prefix the user names,
  by the warehouse's own statement, never from the workspace.
- Reading a lake makes the combine step's time depend on a network. The run's `timeout` and
  `stall_timeout` bound it, as they bound a source.
- Each lake's prefix in `allowed_directories` is one more thing to get right. The prefix is
  computed in one function, from the path the file names, and a workspace test pins it against
  the behaviours above.

## Alternatives considered

- **Load the lake into the workspace like any source.** No change to the lock, but it reads
  every byte of the lake and keeps it on the run's disk: it is what RS7 is for, and a
  terabyte doesn't fit.
- **Leave external access on for a run with a lake.** Tried: with external access on,
  `allowed_directories` bounds nothing, so the combine query could read any URL, any local
  file, and install extensions. The lock is what makes a path in a file safe to run.
- **A bucket read through DbRover rather than DuckDB,** fetching the objects and appending
  them: pushdown, row-group statistics and parallel range requests are DuckDB's, and
  rebuilding them is the engine work this plan keeps out.
- **A path written in the combine query, with the whole bucket allowed.** Shorter to write,
  but the prefix then comes from SQL text rather than from a key the report declares, so
  nothing could estimate the lake's cost before the run, name the source in the profile, or
  bound what the query reaches.
