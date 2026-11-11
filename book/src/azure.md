# Microsoft Azure

## Azure Database for PostgreSQL and MySQL

Choose **Azure Database for PostgreSQL** or **… MySQL** in **New data source › Microsoft Azure**
([adding one](cloud-services.md#adding-one)), for a flexible server. The host ends in
`.postgres.database.azure.com` or `.mysql.database.azure.com`, and the user is the plain name:
the `user@server` form was single server's, which Azure has retired.

- **TLS** is `verify-full`: Azure's certificates come from public authorities.
- **Sign in with** is a password, or **Microsoft Entra ID**, as described in
  [Cloud sign-in](cloud.md#microsoft-entra-id), once the server has an Entra administrator.

## Azure SQL Database

Choose **Azure SQL Database**. The host ends in `.database.windows.net`; **TLS** is
`verify-full`; **Sign in with** is a SQL login's password or **Microsoft Entra ID**.

From outside Azure, Azure SQL's gateway passes the connection on to the database. From inside
Azure (a virtual machine, say) it answers the sign-in with the database's own node instead, on
a port from 11000 to 11999, and DbRover connects there: those ports must be open. Through an
[SSH tunnel](data-sources.md#ssh-tunnels) DbRover can't follow it, and says so; set the server's
connection policy to **Proxy** to use a tunnel.

## Synapse and Fabric

Azure's warehouses speak SQL Server's protocol, so they are SQL Server data sources:

- **Azure Synapse dedicated SQL pool**: the host is `<workspace>.sql.azuresynapse.net`, the
  database the pool's name.
- **Azure Synapse serverless SQL pool**: the host is `<workspace>-ondemand.sql.azuresynapse.net`.
- **Microsoft Fabric**, a warehouse or a lakehouse's SQL analytics endpoint: the host is the
  **SQL connection string** from the item's settings, ending in
  `.datawarehouse.fabric.microsoft.com`, and the database the item's name. Fabric signs in only
  with **Microsoft Entra ID**; the dialog won't save it with a password.

DbRover sees from the server which warehouse it is and shows it as the server. A Fabric item
says the same about itself as a serverless pool, so DbRover tells it by its
`.datawarehouse.fabric.microsoft.com` host: give that host, not a name of your own. Each keeps only
part of SQL Server's catalog, and the explorer shows only what it has:

| | Synapse dedicated | Synapse serverless | Fabric |
| --- | --- | --- | --- |
| Other databases in the explorer | no, only the pool's own | yes | yes, the workspace's |
| Sequences, synonyms, triggers | no | no | no |
| A table's indexes | yes | no | no |
| A table's constraints | primary keys, unique, defaults | none | primary keys, unique, foreign keys |

The [object helpers](objects.md) are offered as far as each warehouse takes their DDL: on a
dedicated pool and on Fabric, tables and views without keys (a warehouse's keys are written
`NOT ENFORCED`, which you write in a console), and no indexes on Fabric, whose **New table**
also suggests Fabric's own column types. A serverless pool's tables are external tables over
files, which the helpers don't write. Fabric's SQL analytics endpoint refuses any change to its
tables, with its own message.

## Blob Storage

Files in an Azure Blob Storage container are read by a DuckDB data source, signed in with Entra
ID too: see [Object storage and MotherDuck](object-storage.md).

## Not tried against Azure yet

Built and tested without an Azure account. What DbRover shows for each warehouse comes from
Microsoft's documentation, tried on SQL Server standing in for each. To try by hand: Azure
Database for PostgreSQL and MySQL with a password and with Entra ID; Azure SQL from outside
Azure and from an Azure virtual machine (the redirect); a Synapse dedicated and a serverless
pool; and a Fabric warehouse and SQL analytics endpoint, each browsed and given a **New table**
where it takes one.
