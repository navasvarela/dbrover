# MinIO's fixtures

The `minio` service in `docker-compose.yml` starts with the bucket `dbrover` holding
everything in `dbrover/`, copied in with `mc mirror` on each start. The tests that read
buckets (C2, `docs/cloud-data.md`) count on what is here, so a change to a file is a change
to those tests.

| Path | What it is | How it was made |
| --- | --- | --- |
| `sales/year=2025/part-0.parquet`, `sales/year=2026/part-0.parquet` | 10 and 5 rows (`id`, `item`, `amount DECIMAL(10,2)`, `sold_on DATE`), partitioned as Hive does | DuckDB 1.5.6, `COPY (SELECT …) TO '…' ` |
| `customers.csv` | 3 rows with a header | by hand |
