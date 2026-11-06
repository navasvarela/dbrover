# Athena's exchanges

What the replay server (`../replay.rs`) answers the driver's tests with: each file is one
scenario, its requests in the order the driver sends them. They are written from Athena's API
reference (the request and response shapes of `GetWorkGroup`, `StartQueryExecution`,
`GetQueryExecution`, `GetQueryResults` and `StopQueryExecution`), not recorded: there is no AWS
account to record them from (`docs/cloud-data.md`, C3). What the reference doesn't show, and
the live test (`live_athena` in `../mod.rs`) is there to find out:

- whether a `SELECT`'s first row repeats the column names on every engine version (it does in
  the examples clients such as PyAthena handle);
- the text of a failed query's `StateChangeReason`, here Trino's `line L:C: …`;
- whether `DataScannedInBytes` grows while a query runs, or appears only at the end;
- whether an `INSERT`'s result has no columns and carries `UpdateCount`.

Correct a file from what real Athena answers, and say so in its `doc`.
