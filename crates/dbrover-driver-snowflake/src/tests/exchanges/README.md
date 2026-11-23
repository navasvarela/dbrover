# Snowflake's exchanges, for the replay server

Each file is a scenario the driver's tests run against `tests/replay.rs`: the requests DbRover
should send to Snowflake's SQL API, in order, and the answers to give. They were written on
2026-11-23 from Snowflake's documentation (the SQL API reference, *Handling responses*,
*Handling errors*, *Cancelling the execution of a statement*), since there is no account to
record from and no emulator of the SQL API. They show DbRover's requests are well formed and
that it reads Snowflake's documented answers, not that Snowflake answers exactly so.

What the documentation doesn't show, for the live check (`live_snowflake`, and C5's demo note):

- the code and message of a statement stopped by its `timeout` (HTTP 408), and whether a
  request's `timeout` can go past a warehouse's own `STATEMENT_TIMEOUT_IN_SECONDS`;
- the text of `VARIANT`, `OBJECT`, `ARRAY`, `GEOGRAPHY`, `GEOMETRY` and `VECTOR` values (written
  here as Snowflake's other clients show them: JSON);
- whether a `NUMBER` with a scale keeps its trailing zeros (`1.50`), and the case of `rowType`'s
  `type` (the reference writes `fixed`, the guide `FIXED`; the driver reads either);
- the exact text of a refused JWT or token (`390144` here is from Snowflake's key-pair
  troubleshooting page) and of a 429's body;
- whether names in a request's `warehouse`, `role`, `database` and `schema` fold to upper case
  (the reference calls them case-sensitive; DbRover sends them as typed).

Any answer that differs when tried live goes into these files.
