# Trino's test server

The `trino` service of `docker-compose.yml` mounts `catalog/` as Trino's catalogs, in place of
the image's own four (`jmx`, `memory`, `tpcds`, `tpch`):

- `memory`, where the tests create their tables and views, gone when the container stops;
- `tpch`, which makes up TPC-H's tables as they are read, at any scale (`tpch.tiny` to
  `tpch.sf100000`), with nothing stored: the cancel test sums `tpch.sf1000.lineitem`.

Trino's own `system` catalog is always there. Leaving out `jmx` and `tpcds` keeps the explorer's
tests from walking hundreds of tables nobody asked for.
