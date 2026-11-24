# Databricks's exchanges, for the replay server

Each file is a scenario the driver's tests run against `tests/replay.rs`: the requests DbRover
should send to Databricks's REST APIs, in order, and the answers to give, the external links'
downloads among them (under `/links/`, where `{endpoint}` stands for the server's address).
They were written on 2026-11-24 from Databricks's documentation (the Statement Execution API's
reference and tutorial, the SQL warehouses' reference, and the official SDKs' sources, whose
docstrings are the reference's), since there is no workspace to record from and no emulator of
Databricks's own. They show DbRover's requests are well formed and that it reads Databricks's
documented answers, not that Databricks answers exactly so.

What the documentation doesn't show, for the live check (`live_databricks`, and C6's demo note):

- how `JSON_ARRAY` writes each type: timestamps (written here as `2026-01-02T03:04:05.123Z`),
  whether a `DECIMAL` keeps its trailing zeros (`1.50`), `BINARY`, `ARRAY`, `MAP`, `STRUCT`,
  `VARIANT` and intervals (written here as their JSON), and `BOOLEAN` (`true`);
- the status and words of a refused token (401 and `Invalid access token.` here), of an
  unknown warehouse, and of a 429's body;
- the error a cancelled statement carries (none here: DbRover says it was cancelled);
- what an expired link answers on each cloud (403 here, as S3 answers an expired presigned
  URL), and the names of the headers a link carries (S3's customer key here);
- whether every warehouse takes `query_tags`, and whether reading a warehouse ever starts it.

Any answer that differs when tried live goes into these files.
