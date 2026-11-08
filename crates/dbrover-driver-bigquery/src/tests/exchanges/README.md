# BigQuery's exchanges

What the replay server (`../replay.rs`) answers the driver's tests with: each file is one
scenario, its requests in the order the driver sends them. They are written from BigQuery's
REST reference (`bigquery/v2`: `datasets.list`, `jobs.insert`, `jobs.get`,
`jobs.getQueryResults` and `jobs.cancel`), not recorded: there is no Google Cloud project to
record them from (`docs/cloud-data.md`, C4). `{job}` stands for the job id the driver chose.
What the reference doesn't show, and the live test (`live_bigquery` in `../mod.rs`) is there to
find out:

- the exact text of an error's message, here ending `at [line:column]`, and whether a
  statement that can't be read is refused by `jobs.insert` (HTTP 400) or fails at its first
  `jobs.get`; both are replayed;
- whether `statistics.query.totalBytesProcessed` is there while a job runs, or only once it is
  done;
- whether a `TIMESTAMP` comes as microseconds when `formatOptions.useInt64Timestamp` is set
  (the reference says so; the emulator ignores it);
- the message of a cancelled job, here `Job execution was cancelled: User requested
  cancellation`.

Correct a file from what BigQuery really answers, and say so in its `doc`.
