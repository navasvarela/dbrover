# MinIO's fixtures

The `minio` service in `docker-compose.yml` starts with the bucket `dbrover` holding
everything in `dbrover/`, copied in with `mc mirror` on each start. The tests that read
buckets (C2, `docs/cloud-data.md`) count on what is here, so a change to a file is a change
to those tests.

| Path | What it is | How it was made |
| --- | --- | --- |
| `sales/year=2025/part-0.parquet`, `sales/year=2026/part-0.parquet` | 10 and 5 rows (`id`, `item`, `amount DECIMAL(10,2)`, `sold_on DATE`), partitioned as Hive does | DuckDB 1.5.6, `COPY (SELECT …) TO '…' ` |
| `customers.csv` | 3 rows with a header | by hand |
| `warehouse/shop/orders/` | An Iceberg table (`shop.orders`, 4 rows in two snapshots: `id`, `customer`, `total DECIMAL(10,2)`, `ordered_on DATE`). Its metadata names `s3://dbrover/warehouse/shop/orders/…`, so the bucket keeps its name. | pyiceberg 0.10.0's SQL catalog (SQLite) with its warehouse at `s3://dbrover/warehouse` on this MinIO, two appends, then copied out with `mc` |
| `warehouse/products/` | A Delta table (4 rows in two commits: `sku`, `name`, `stock INTEGER`), whose log names its files relative to the folder | delta-rs (`deltalake` 1.1.4 for Python), `write_deltalake` then an append |
