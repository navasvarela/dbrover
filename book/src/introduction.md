# Introduction

<img src="images/logo.svg" alt="" width="72">

DbRover is a desktop tool to browse and query databases. It connects to eleven databases:
**PostgreSQL**, **MySQL and MariaDB**, **SQLite**, **SQL Server**, **DuckDB**, **ClickHouse**,
**Redis and Valkey**, **MongoDB**, **Elasticsearch and OpenSearch**, **Cassandra and
ScyllaDB**, and **Amazon DynamoDB** (Oracle is planned),
shows what is inside them as a tree, and gives you query consoles to write and run SQL, page
through, sort and export the results, and edit rows.

![DbRover with the explorer, a query console and the log console](images/workspace.png)

The window has three areas, which you can drag around and resize:

- **Explorer**, a column down the left: your saved connections ("data sources") and, under
  each, its databases, schemas, tables and so on.
- **Consoles**, the main area on the right: the Welcome tab and one tab per query console.
- **Log console**, under the consoles: what DbRover is doing — connections opening and closing,
  queries running and how long they took, errors.

The thin bar at the very bottom shows whether DbRover's background engine is running and how
many data sources the explorer is connected to.

This manual describes DbRover as it is today. It is young software: things that are planned but
not built yet are not described here, apart from a few warnings in
[Troubleshooting](troubleshooting.md).
