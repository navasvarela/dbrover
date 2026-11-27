# DbRover and other database tools

What the tools people already use do well, and where DbRover fits among them. First read on
2026-10-28 and 2026-10-29, against DbRover 0.13.0, and refreshed on 2026-11-25 against DbRover
0.39.0. This page has four jobs:

- name the tools closest to DbRover and say why;
- compare features area by area;
- rank the gaps worth closing soon;
- say what DbRover brings.

The guide to other tools on dbrover.org is built from it, and so were the roadmap's W9 to W13
and ER1, all six now built (see [Closed since October](#closed-since-october)). Its start-up and
size numbers come from [Measurements](measurements.md).

The site's page credits each tool for what it does well, says plainly what DbRover doesn't do
yet, and leads with what DbRover brings rather than with how it beats anyone. This page is
research, so it ranks and weighs, but it keeps to that tone too.

How to read it:

- **DbRover's facts** come from its own manual and roadmap, linked as `[R…]`. They describe
  0.39.0.
- **Other tools' facts** come from their official sites, docs, pricing pages, release notes and
  GitHub repositories, and each one is linked to its source: `[D…]` DBeaver, `[J…]` DataGrip,
  `[T…]` TablePlus, `[B…]` Beekeeper Studio, `[X…]` DBX, `[L…]` everything else. The sources
  are listed with their dates at the end. A cell changed on 2026-11-25 cites a source read that
  day.
- **Unconfirmed** means no official page said so either way. Where vendors' own pages disagree,
  the cell says so. A gap here is a gap in the docs, not proof the feature is missing.
- Vendors change these things often. Check a fact against its source before quoting it outside
  this repository, and date the quote.

## The field

No survey ranks database GUIs: the Stack Overflow 2025 survey and JetBrains' 2025 developer
ecosystem report list database engines and IDEs only [L1][L2]. Adoption below rests on GitHub
stars and downloads and on marketplace installs, read on 2026-11-25.

Tools were ranked on five questions, the first weighing most:

1. **Audience overlap:** aimed at developers, several databases, desktop, all three systems.
2. **Native and fast:** is it small and quick to start (no JVM, no Electron)?
3. **Safe AI access:** does it serve databases to AI agents over MCP, with permissions?
4. **Adoption:** stars, downloads, installs.
5. **Momentum:** release cadence and growth.

| Rank | Tool | What it does well, and how it relates to DbRover |
| --- | --- | --- |
| 1 | **DBeaver Community** | The default free multi-database client: 52.5k stars [D2], Apache-2.0, every system and most databases DbRover has. Java on Eclipse RCP [D2]. Its MCP server is documented for 26.3, not yet released, in the paid editions only [D9]; 26.2.5 brought Ollama's local models to Community's AI chat, though the docs still call it PRO [D33][D40]. The tool most people know already. |
| 2 | **DBX** | The closest to DbRover in spirit, and the fastest-moving: 31.4k stars since its repository was created on 2026-04-29 [X1], fifteen releases from v0.6.60 to v0.6.74 between 2026-10-28 and 2026-11-24 [X3]. Free (Apache-2.0), all three systems, "100+" databases, a Rust core under a Vue interface in the system web view (Tauri) [X1], a 45 MiB download for Apple silicon [X3], an AI assistant with an agent mode and **Fix with AI** [X16], an MCP server with read-only, read-write and full-access modes [X5], and a store of signed, sandboxed plugins [X1][X20]. It shares DbRover's aims: small, free and open, with safe AI access. |
| 3 | **Beekeeper Studio** | The "friendly open-source SQL client" for the same developers: 24k stars [B2], all systems, Electron [B2]. Plugins since 6.2 (2026-10-28), for every edition [B18][B19]. DuckDB, ClickHouse, ER diagrams and its AI shell are paid [B1]; no official MCP server [B16]. |
| 4 | **DataGrip** | The benchmark for completion, refactoring and inspections, free for non-commercial use since October 2025 [J23]. JVM [J24]. 2026.3 (2026-11-05) puts JetBrains Air in it: agents that run SQL and read results in a chat [J25][J27]. Its built-in MCP server asks for consent before agents read or change schema and data [J9][J38]. |
| 5 | **Tabularis** | Rust and Tauri, Apache-2.0, 5.5k stars since January 2026 [L3]. Its MCP reads run in a read-only database session on its built-in drivers, with approval gates [L4]. v0.33 (2026-11-16) added Fix with AI, a vim mode and Git commits of SQL files [L13]; it already had notebooks and a visual query builder [L16], and its drivers are plugins in any language [L15]. Aimed at the same Rust audience as DbRover's soft launch. |
| 6 | **TablePlus** | The "native and fast" reference among paid tools [T1], with an MCP server whose tokens are read-only, read-write, full access or set per connection [T14]. 27.2 to 27.4 (2026-10-28 to 2026-11-21) added Schedule Tasks, Sync Database in place of Backup and Restore, and a warning before an `UPDATE` or `DELETE` without `WHERE` [T3]; JavaScript plugins are in development [T19]. Proprietary, perpetual licence [T2]. |
| 7 | **DbGate** | Open source (GPL-3.0, 7.4k stars), all systems, DuckDB and ClickHouse; a read-only MCP server in its free Docker build since July 2026 [L5]. Built-in backup and restore since 7.2.4, and AI that explains an SQL error in Premium [L18][L19]. Electron and web-first. |

**Watch list:**

- **TablePro**: native Swift, AGPL, MCP and a Safe Mode in its free tier, 6.5k stars; macOS and
  iOS, with Linux now in public beta [L6].
- **Chat2DB**: 28k stars, built around AI; its licence is now Apache-2.0 with added conditions
  [L7].
- **DBCode**: a VS Code extension with MCP tools and per-connection AI permissions in its free
  tier, 238k installs [L8].
- **Sequel Ace**: macOS and MySQL only, but its MCP server is off by default and read-only by
  default, as DbRover's is [L9].
- **Small Rust clients** on GPUI (DBFlux, OnetCli, ZQLZ) and on egui, DbRover's own stack
  (Ferox, Tabular) [L10]. DBFlux and Tabular now serve MCP too. They will come up on r/rust.

**Narrower or retired:**

- **Azure Data Studio** was retired on 2026-02-28; Microsoft sends its users to VS Code with
  the MSSQL extension [L11].
- **Postico 2** is a fine native Mac app, but for PostgreSQL and its relatives only, and macOS
  only [L12].

The matrix below compares DbRover with the four tools the launch plan named (DBeaver,
DataGrip, TablePlus and Beekeeper Studio) and with DBX.

## Feature matrix

✅ yes · ⚠️ partly, or only in a paid edition (the cell says which) · ❌ no · ❔ unconfirmed.
DBeaver's column is Community unless a cell names a PRO edition (Lite, Enterprise, Ultimate).
Beekeeper's is Community unless it names a paid tier (Indie, Professional, Business).

### Databases

| | DbRover 0.39 | DBeaver | DataGrip | TablePlus | Beekeeper | DBX |
| --- | --- | --- | --- | --- | --- | --- |
| Engines | 16 kinds of data source, with MariaDB, Valkey, OpenSearch, ScyllaDB and Presto under them [R3] | "100+" through JDBC and ODBC [D2] | "over 50" [J1] | 25 on its home page [T1] | about 25 across tiers [B1] | "100+" [X1] |
| PostgreSQL, MySQL, MariaDB, SQLite, SQL Server | ✅ [R1] | ✅ [D4] | ✅ [J1] | ✅ [T1] | ✅ [B1] | ✅ [X4] |
| DuckDB | ✅ built in, also over files in a bucket and MotherDuck [R19] | ❔ listed [D4], edition not stated | ✅ [J10]; a dialect of its own since 2026.3 [J25] | ✅ [T3] | ⚠️ Indie and up [B1] | ✅ [X4]; MCP needs a separate driver [X5] |
| ClickHouse | ✅ [R2] | ❔ listed [D4], edition not stated | ✅ [J1] | ✅ [T1] | ⚠️ Professional and up [B1] | ✅ [X4] |
| Oracle | ❌ planned [R2] | ✅ [D4] | ✅ [J1] | ✅ [T1] | ⚠️ Indie and up [B1] | ✅ [X4] |
| MongoDB, Redis | ✅ with Valkey and Dragonfly [R3][R16] | ⚠️ PRO [D1] | ✅ [J1] | ✅ [T1] | ⚠️ Redis free, MongoDB Professional [B1] | ✅ [X1] |
| Elasticsearch, Cassandra, DynamoDB | ✅ with OpenSearch and ScyllaDB [R3][R16] | ⚠️ Elasticsearch and Cassandra PRO; DynamoDB ❔ [D1] | ⚠️ Cassandra and DynamoDB; Elasticsearch basic [J35][J36] | ✅ [T1] | ⚠️ Cassandra Indie, DynamoDB Professional; Elasticsearch ❔ [B1] | ✅ Cassandra through a JDBC agent [X4] |
| Snowflake, BigQuery, Redshift, Databricks | ✅ each signed in its own way; Redshift as PostgreSQL [R18][R21][R22][R23] | ❔ Snowflake and BigQuery listed, edition not stated; a Databricks driver in 26.2.5; Redshift PRO [D4][D33][D37] | ⚠️ Databricks basic [J35][J36] | ⚠️ Snowflake, BigQuery, Redshift; Databricks ❔ [T1] | ⚠️ BigQuery and Redshift free, Snowflake Business, Databricks ❔ [B1] | ✅ Redshift native, the rest through JDBC agents [X4] |
| Athena, Trino | ✅ with Starburst and Presto [R20] | ❔ | ⚠️ basic support [J36] | ⚠️ Athena since 27.3; Trino ❔ [T3] | ⚠️ Trino Professional; Athena ❔ [B1] | ⚠️ Trino through JDBC; Athena ❔ [X4] |
| Files in S3, Google Cloud Storage, Azure Blob | ✅ Parquet, CSV, JSON, Iceberg and Delta, through DuckDB [R19] | ❔ | ❔ | ❔ | ❔ | ❔ |

### Connections

| | DbRover 0.39 | DBeaver | DataGrip | TablePlus | Beekeeper | DBX |
| --- | --- | --- | --- | --- | --- | --- |
| SSH tunnel | ✅ key, agent or password; checks `known_hosts` [R3] | ✅ with jump servers [D13] | ✅ [J7] | ✅ [T4] | ✅ with jump hosts [B3] | ✅ chained with SOCKS or HTTP proxies [X7] |
| TLS with CA file, verify-full | ✅ [R3] | ⚠️ listed as a PRO feature [D1]; Community ❔ | ✅ [J7] | ✅ [T4] | ✅ [B3] | ✅ [X3][X14] |
| AWS IAM / GCP IAM / Entra ID sign-in | ✅ all three for PostgreSQL and MySQL, Entra ID for SQL Server too; AWS and Google accounts for DynamoDB, Athena and BigQuery [R17] | ⚠️ Ultimate [D1][D14] | ⚠️ cloud account explorers; RDS IAM ❔ [J5] | ⚠️ Entra ID for SQL Server; an AWS profile for Athena [T4][T3] | ⚠️ AWS IAM and Entra ID in Business [B1] | ❔ none found |
| Password from a command (cloud tokens, 1Password, Vault) | ✅ asks before its first run [R3] | ✅ asks before it runs since 26.2.3 [D2] | ❔ | ✅ PostgreSQL, MySQL, MariaDB, Redshift [T4] | ❔ | ❔ |
| Passwords in the OS keychain | ✅ [R3] | ❔ | ❔ | ✅ [T4] | ❔ | ✅ [X1] |
| `${VARIABLE}` placeholders in connections | ✅ [R3] | ❔ | ❔ | ❔ | ❔ | ❔ |

### SQL editor

| | DbRover 0.39 | DBeaver | DataGrip | TablePlus | Beekeeper | DBX |
| --- | --- | --- | --- | --- | --- | --- |
| Completion from the schema | ✅ follows aliases and CTEs, fuzzy [R5] | ✅ [D15] | ✅ including whole JOINs from keys [J6] | ✅ [T5] | ✅ [B9] | ✅ [X8] |
| Formatter | ✅ [R5] | ✅ [D16] | ✅ [J14] | ✅ [T5] | ✅ [B10] | ✅ [X8] |
| Errors and warnings as you type | ✅ lint rules and the database's own check [R5] | ❔ | ✅ inspections [J6] | ❔ | ❔ | ❔ |
| Rename refactoring | ❌ | ❔ | ✅ [J6] | ❔ | ❔ | ❔ |
| Explain plan | ✅ a tree with each step's cost and rows, and Explain analyse, on PostgreSQL, MySQL, MariaDB and SQL Server; text elsewhere [R24] | ✅ graph view; some engines PRO [D17] | ✅ diagram and flame graph [J15] | ❔ | ⚠️ a plugin [B19] | ✅ plan tree with costs [X8] |
| `:name` parameters | ✅ [R6] | ❔ | ❔ | ❔ | ✅ [B9] | ❔ |
| `.sql` files, find and replace | ✅ [R6] | ✅ scripts [D2] | ✅ [J1] | ❔ | ❔ | ❔ |
| Vim keys | ❌ | ❔ | ✅ IdeaVim [J32] | ❔ | ✅ [B18] | ✅ [X1] |
| Quick open, split editors | ❌ | ❔ | ✅ Search Everywhere; split ❔ [J33] | ✅ Open Anything, split panes [T21] | ❔ | ✅ both [X1][X3] |

### Data grid, import and export

| | DbRover 0.39 | DBeaver | DataGrip | TablePlus | Beekeeper | DBX |
| --- | --- | --- | --- | --- | --- | --- |
| Edit, add and delete rows | ✅ submitted together, all or none [R7] | ✅ [D3] | ✅ [J8] | ✅ reviewed before ⌘S [T6][T7] | ✅ tables; query results Indie and up [B1] | ✅ shows the SQL before saving [X9] |
| Follow foreign keys | ✅ both ways [R7] | ❔ | ❔ | ❔ | ❔ | ✅ the referenced row beside the cell [X3] |
| Sort and filter in the database | ✅ [R7] | ❔ | ❔ | ❔ | ❔ | ❔ |
| Value viewers | ⚠️ long text, XML, and JSON as a tree; no images, hex or maps [R5] | ✅ binary, JSON, XML, HTML; a GIS map in Lite and up [D38][D29] | ✅ binary, and a geo viewer [J31] | ❔ JSON and tree views [T3] | ⚠️ JSON Indie and up; a map plugin [B1][B19] | ✅ hex, images, maps of geometry [X9] |
| Search for a value across a database's tables | ❌ being built (W16) | ❔ a full-text search page, edition not stated | ✅ Full-Text Search [J28] | ✅ Search in Database [T21] | ❔ | ❔ object search only [X1] |
| Export | ✅ CSV, Excel, TSV, Markdown, SQL, JSON, JSON Lines, Parquet, HTML, and formats of your own; every row streamed [R26][R27] | ✅ CSV, JSON, HTML, Markdown, SQL, TXT; XLSX, XML, Parquet PRO [D18] | ✅ CSV, JSON, HTML, Markdown, Excel, custom [J8] | ✅ CSV, JSON, SQL, Excel [T8] | ⚠️ Indie and up: CSV, Excel, JSON, Markdown, SQL [B1][B12] | ✅ CSV, TSV, JSON, Markdown, XLSX, SQL [X9] |
| Import | ⚠️ CSV with a mapping and preview [R7] | ✅ CSV; XLSX, XML PRO [D18][D20] | ✅ CSV, TSV, dumps [J16] | ✅ CSV, JSON, SQL dump [T8] | ⚠️ Indie and up: CSV, JSON, XLSX [B1][B11] | ✅ CSV, JSON, Excel, SQL; Parquet into DuckDB [X10] |
| Compare rows between two tables | ❌ | ⚠️ Enterprise and Ultimate [D36] | ✅ [J29] | ❌ Sync Database replaces tables, without a diff [T17] | ❔ | ✅ with a script [X11] |
| Charts from results | ✅ bars, lines, scatter, area; written in a query's text, drawn by reports and for MCP clients [R25] | ⚠️ listed for Lite and up [D29] | ✅ [J31] | ✅ Chart view [T6] | ❔ | ✅ [X14] |
| Reports: queries over several databases into files, on the command line | ✅ combined in DuckDB or PRQL, written to CSV, Excel, Parquet and charts, with limits [R32] | ❔ | ⚠️ one query across data sources in DuckDB, since 2026.3 [J25] | ⚠️ Schedule Tasks back up and sync, not query [T18] | ❔ | ❔ |

### Schema tools

| | DbRover 0.39 | DBeaver | DataGrip | TablePlus | Beekeeper | DBX |
| --- | --- | --- | --- | --- | --- | --- |
| Create and change tables without SQL | ✅ shows the DDL before it runs [R8] | ✅ [D2] | ✅ [J8] | ✅ [T9] | ✅ [B1] | ✅ reviewable DDL [X12] |
| Views, indexes | ✅ [R8] | ✅ [D2] | ✅ [J1] | ⚠️ indexes [T9] | ✅ [B1] | ❔ |
| Users and roles | ✅ PostgreSQL, MySQL, MariaDB, SQL Server [R28] | ❔ | ✅ on most engines [J30] | ✅ PostgreSQL, MySQL, MariaDB [T20] | ❔ | ✅ MySQL, PostgreSQL, Oracle and more [X3][X14] |
| Copy DDL | ✅ [R8] | ✅ "Generate SQL" [D23] | ❔ | ❔ | ❔ | ❔ |
| Schema compare and migration script | ⚠️ PostgreSQL only: two schemas, or one and a snapshot file [R31] | ⚠️ Enterprise and Ultimate; a simple compare in Community [D21][D22] | ✅ [J17] | ❌ Sync Database copies, it doesn't compare [T17] | ⚠️ a community plugin, not reviewed by Beekeeper [B19] | ✅ schema diff across several schemas, with a script [X11][X3] |
| Dump and restore | ❌ | ✅ PostgreSQL, MySQL, MariaDB [D35] | ✅ through the databases' own dump tools: PostgreSQL, MySQL, MariaDB [J37][J25] | ✅ Sync Database, to and from dump files, on a schedule [T17][T18] | ⚠️ Indie and up [B1] | ✅ scheduled backups for MySQL and PostgreSQL [X21] |
| Server activity: sessions, locks, cancel and kill | ✅ PostgreSQL, MySQL, MariaDB, SQL Server, ClickHouse; recorded in the history [R29] | ✅ Session Manager and Lock Manager [D37] | ❔ cancels its own statements only [J40] | ✅ Process List [T21] | ❔ | ✅ Process List [X14] |
| Queries, diagrams and schemas in Git | ⚠️ no Git inside; files that live well in it, query folders and schema snapshots [R33][R34][R31] | ✅ Git projects [D2] | ✅ the IDE's Git [J34] | ❔ | ❔ | ⚠️ sync through a Git snippet [X18] |

### ER diagrams

| | DbRover 0.39 | DBeaver | DataGrip | TablePlus | Beekeeper | DBX |
| --- | --- | --- | --- | --- | --- | --- |
| Has them | ✅ read-only, on the six engines with foreign keys [R30] | ✅ [D2] | ✅ [J18] | ✅ since June 2026 [T3][T11] | ⚠️ paid tiers only [B13] | ✅ [X1] |
| Selected tables, or whole schemas | ✅ both: a schema's tables, or a table with the ones its keys join [R30] | ✅ both: one table, a schema, or a custom diagram of tables from several connections [D23][D28] | ✅ both: a data source, schema or table, and add more by dragging [J18] | ⚠️ a schema, with tables added and removed [T11] | ⚠️ the active database [B13] | ✅ both: a database, chosen schemas, or chosen tables with their related ones [X3][X14] |
| Edit the schema from the diagram | ❌ out of scope [R30] | ⚠️ Edit mode in Enterprise, Ultimate and Team; it writes a script to review [D1][D23] | ⚠️ through the Modify dialog [J18] | ✅ columns, indexes, foreign keys, committed with ⌘S [T11] | ❔ not described [B13] | ✅ create, alter and drop, synced after showing the SQL; existing keys can't be edited [X14] |
| Export | ✅ SVG, PNG at 1× to 3× [R30] | ✅ PNG, GIF, BMP, GraphML; SVG in PRO [D23] | ✅ PNG, GraphML, draw.io, DOT, Mermaid, PlantUML [J18] | ❔ [T11] | ⚠️ an image, or the clipboard [B13] | ✅ SVG, PNG, JSON, DBML, Mermaid [X14] |
| Layout | automatic, kept as a TOML file beside your settings [R30] | automatic, kept only if asked [D23] | automatic layouts, saved as `.uml` [J18] | rearrange and sort [T11] | ❔ | automatic, saved per view [X3][X14] |

### Query history

| | DbRover 0.39 | DBeaver | DataGrip | TablePlus | Beekeeper | DBX |
| --- | --- | --- | --- | --- | --- | --- |
| Kept across restarts | ✅ [R10] | ⚠️ Lite and up; Community clears it at restart [D25] | ✅ [J19] | ✅ [T12] | ❔ [B9] | ✅ [X8] |
| Search | ✅ every data source at once, by text, kind and time [R10] | ⚠️ filters; keyword search ❔ [D25] | ✅ per data source [J19] | ✅ with regular expressions [T3] | ❔ | ✅ by date [X8] |
| Where each statement came from (console, grid, AI agent) | ✅ with the MCP client's name [R10] | ❔ | ⚠️ AI agents' queries are logged [J3] | ❔ | ❔ | ✅ AI or not; a log of MCP calls with the client [X8][X5] |
| A grid edit's old values, and the statement that undoes it | ✅ [R10] | ❔ | ❔ | ❔ | ⚠️ "Query Edit History" [B8] | ❔ |
| Retention and export | ✅ [R10] | ❔ | ❔ | ✅ 1–3 years [T3] | ❔ | ⚠️ retention by count [X8] |

### AI and MCP

| | DbRover 0.39 | DBeaver | DataGrip | TablePlus | Beekeeper | DBX |
| --- | --- | --- | --- | --- | --- | --- |
| AI chat inside the app | ❌ bring your own agent over MCP | ✅ OpenAI, Copilot and local models through Ollama since 26.2.5; other providers PRO [D33][D6] | ✅ JetBrains Air: Junie, Claude, Codex, Copilot, Gemini CLI or any ACP agent, run SQL in the chat [J25][J27] | ✅ own key, many providers, Ollama, ACP agents; also in diagrams since 27.4 [T13][T3] | ⚠️ Professional and up, own key, OpenAI-compatible [B1][B14] | ✅ Ask and Agent modes, many providers and Ollama, or a local Claude Code or Codex [X16] |
| Fix an error with AI | ❌ | ❔ | ❔ | ❔ | ❔ | ✅ Fix with AI [X16] |
| MCP server | ✅ free, stdio, the app needn't be running; reports, charts and server activity too [R11] | ⚠️ Lite to Ultimate, documented "since 26.3", unreleased on 2026-11-25 [D9][D2] | ✅ inside the running IDE [J9] | ✅ inside the running app [T14] | ❌ only a community plugin [B16] | ✅ free, a separate install; the app needn't be running [X5] |
| Read-only by default | ✅ [R11] | ❔ | ⚠️ asks consent; advises a read-only database user [J9][J38] | ⚠️ per token; "approval for unsafe query" on by default [T14] | — | ❔ |
| Read-only enforced by the database, not only by parsing | ✅ the engine's read-only mode where it has one; elsewhere served only through a read-only login [R11] | ❔ | ❌ relies on the database user [J9] | ❔ | — | ❌ "depends on classifying SQL" [X6] |
| Scope | per project, optionally a list of schemas [R11] | per connection [D9] | ❔ | per token: all, or chosen connections or groups [T14] | — | per database, connection or group, with allow-lists of connections, databases and tools [X5] |
| Separate permissions for writes, schema changes and admin | ✅ and for running reports and seeing server activity [R11] | ❔ | ✅ four consents [J38] | ⚠️ read only, read & write, full access, or custom per connection [T14] | — | ⚠️ read only, data read and write, or full access [X5] |
| A dedicated database login for agents | ✅ a login, AWS profile, service account or role of its own [R11] | ❔ | ⚠️ advised, not built in [J9] | ❔ | — | ⚠️ advised [X6] |
| Row limit and statement timeout | ✅ [R11] | ❔ | ❔ | ❔ | — | ❔ |

### Safety

| | DbRover 0.39 | DBeaver | DataGrip | TablePlus | Beekeeper | DBX |
| --- | --- | --- | --- | --- | --- | --- |
| Colour per connection | ✅ [R3] | ✅ the red "Production" type [D11] | ✅ [J3] | ✅ tags and a status-bar colour [T4] | ❔ | ✅ [X1] |
| Ask before changes | ✅ Careful: every change, naming the data source; consoles start in manual commit [R3] | ✅ Production type: confirm SQL and data changes, auto-commit off [D11] | ❔ | ✅ five Safe Mode levels, up to a password per query [T15] | ❔ | ✅ production protection: every write asks again [X6] |
| Read-only connection | ✅ enforced by the database on every engine that has a read-only mode [R3] | ✅ with finer restrictions [D12] | ✅ [J7] | ❔ beyond Safe Mode [T15] | ✅ free since 6.1 [B17] | ✅ by parsing; can be lifted for a few minutes [X6] |
| `UPDATE`/`DELETE` without `WHERE` | ✅ asks [R5] | ❔ | ✅ inspection [J6] | ✅ warns, since 27.4 [T3] | ❔ | ❔ |
| Typed name before dropping on production | ✅ [R8] | ❔ | ❔ | ❔ | ❔ | ❔ |
| Cost guards on cloud warehouses | ✅ a byte limit, checked by a dry run on BigQuery; a time limit on Snowflake and Databricks; a question before a whole-table read on DynamoDB [R17] | ❔ | ❔ | ❔ | ❔ | ❔ |

### Team, platforms, licence and price

| | DbRover 0.39 | DBeaver | DataGrip | TablePlus | Beekeeper | DBX |
| --- | --- | --- | --- | --- | --- | --- |
| Account needed | ❌ none [R12] | ❌ none described for Community [D27] | ✅ to activate, even the free licence [J23] | ❌ a licence key [T2] | ❌ for Community [B1] | ❌ [X17] |
| Share connections | ⚠️ export and import files without secrets; plain TOML [R13] | ⚠️ Git sync in Enterprise and Ultimate; shared workspaces in Team [D1] | ⚠️ templates in your JetBrains Account [J22] | ⚠️ exported files, or a synced folder [T4] | ⚠️ cloud workspaces, Professional and up [B1] | ⚠️ sync over WebDAV or a private Git snippet, secrets left out by default [X18] |
| Settings as plain files you can version | ✅ keymap, actions, snippets, themes, export formats, reports, diagrams, in a folder that lives in Git [R14][R33] | ❔ | ❔ | ❔ | ❔ | ❔ |
| Plugins | ⚠️ actions that run your programs, and export formats as programs; WebAssembly plugins later (H3) [R14][R27][R35] | ✅ Eclipse extensions [D2] | ✅ Plugin Marketplace [J39] | ⚠️ JavaScript, "still under development" [T19] | ✅ since 6.2, every edition; Community up to two community plugins [B18][B19] | ✅ signed and sandboxed, from a store [X1][X20] |
| macOS, Windows, Linux | ✅ [R12] | ✅ [D27] | ✅ [J24] | ✅ Linux in step with macOS since 27.4 [T16][T22][T23] | ✅ [B2] | ✅ and a web version [X19] |
| Built with | Rust and egui, drawn natively [R15] | Java, Eclipse RCP [D2] | IntelliJ platform, JVM [J24] | native [T1] | Electron [B2] | Rust, with a Vue interface in the system web view [X1] |
| Licence | MIT or Apache-2.0 [R12] | Apache-2.0; PRO commercial [D2] | proprietary [J23] | proprietary [T2] | GPL-3.0; paid parts source-available [B2] | Apache-2.0 [X1] |
| Price | free | free; PRO $113–510 a year [D29][D31][D32] | free for non-commercial use; else a subscription [J2][J23] | $99 perpetual, one year of updates [T2] | free; $9–18 a user a month, billed yearly [B1] | free [X1] |

### Start-up and size

Measured on one Mac, as [Measurements](measurements.md) describes: an Apple M4 Pro with 24 GB,
macOS 27.2, warm starts, median of five, on 2026-10-29. They are for DbRover 0.13.0, DBX
0.6.60, DataGrip 2026.2.6 and DBeaver Community 26.2.3, and weren't taken again for this
refresh. TablePlus and Beekeeper Studio weren't measured. DBX 0.6.74's Apple-silicon download
is 45 MiB [X3]. The site quotes DbRover's own numbers as facts, not as a comparison.

| | DbRover 0.13 | DBeaver | DataGrip | DBX |
| --- | --- | --- | --- | --- |
| Main window up | 0.35 s | 3.6 s | 1.8 s | 0.38 s |
| Download for Apple silicon | 75 MiB, universal | 117 MiB | 980 MiB | 43 MiB |
| Installed | 174 MiB | 199 MiB | 2695 MiB | 77 MiB |

## Gaps worth closing soon

Ranked by how often DbRover's audience (developers who query PostgreSQL, MySQL and SQLite every
day, many of them on AWS) would miss each one, against the effort. Sizes are rough, in the
milestones' terms: **S** is a pull request or two, **M** a stack of three to five, **L** a
track of its own. Ranked again on 2026-11-25, after all six gaps of October were built.

| # | Gap | Who has it | Hit how often | Size | Why this rank |
| --- | --- | --- | --- | --- | --- |
| 1 | **Search across a database's data:** a value typed once, looked for in every table's columns, with the rows that hold it. | DataGrip [J28], TablePlus [T21]; DBeaver has a page for it, edition not stated | Weekly, for anyone chasing an id or an email through a schema they don't know | **M** | Hard to do by hand and quick to show. **Being built as W16.** The explorer already searches a catalog's object names on the server (v0.23.0) [R17]; this searches the rows. |
| 2 | **Dump and restore:** a database or chosen tables written to a dump file and read back, PostgreSQL and MySQL first. | DBeaver Community [D35], DataGrip [J37], TablePlus [T17], DBX [X21], DbGate [L18], Tabularis [L14]; Beekeeper Indie and up [B1] | Weekly: a copy of production for local work, a backup before a migration | **M** | Every other tool has it, most of them free, so its absence is noticed at once. Most wrap `pg_dump` and `mysqldump`, which DbRover would have to find on the machine or do without. TablePlus and DBX also run it on a schedule. |
| 3 | **Users and roles beyond PostgreSQL:** MySQL's and MariaDB's accounts, SQL Server's logins and users. | DataGrip [J30], TablePlus [T20], DBX [X14] | Monthly, but on every engine people run | **M** | PostgreSQL's dialogs show the way [R28]; MySQL's `'name'@'host'` accounts and SQL Server's logins are already planned on their own. **Built as W18** ([Users beyond PostgreSQL](users-beyond-postgres.md)). |
| 4 | **Image, hex and geometry viewers** in the value panel. | DataGrip [J31], DBeaver [D38], DBX [X9] | Weekly, for tables with blobs or PostGIS columns | **M** | The value panel beside the grid is there to hold them [R5]. Maps need tiles or a basemap, which makes geometry the largest of the three. |
| 5 | **Vim mode, quick open, split editors.** | DataGrip [J32][J33], TablePlus [T21], Beekeeper [B18], DBX [X1], Tabularis [L13] | Daily for the people who use them, never for the rest | **M** for the three | Small each, and a vim mode is often the first thing a terminal user asks for. Quick open fits the command list the menus already use. Ranked below the data gaps because nobody is blocked without them. |
| 6 | **Row data compare:** two tables' rows side by side by key, and the script that makes one match the other. | DataGrip [J29], DBX [X11]; DBeaver Enterprise and Ultimate [D36]; DbGate Premium [L19] | Monthly | **M–L** | Schema compare (W13) gives the tab and the script to build on [R31]. Large tables need the comparison done in the database, by key ranges or hashes. |
| 7 | **Query builder, mock data, notebooks, debugger.** | Query builder: DBeaver Lite and up [D39], Tabularis [L16], DbGate Premium [L19]. Mock data: DBeaver Enterprise and up [D1], DBX [X14]. Notebooks: Tabularis [L16]. A debugger: DBeaver Enterprise for PostgreSQL [D1], DBX for Oracle [X22] | Rarely, each | **L** each | Each serves fewer people than the gaps above and costs a track of its own. Reports [R32] and charts in a query's text [R25] cover part of what notebooks do. |
| — | **AI chat inside the app** | DBeaver, DataGrip, TablePlus, DBX; Beekeeper in paid tiers | — | — | Not proposed. DbRover serves the agents people already use over MCP rather than brokering a model. The trend below is worth watching. |
| — | **Rename refactoring across SQL** | DataGrip | Rarely | **L** | Not proposed: it needs a full SQL language server, a non-goal. |

**Two trends since October:**

- **AI inside the editor, and agents that run queries there.** DataGrip 2026.3 puts JetBrains
  Air in the IDE, so an agent runs SQL against a chosen data source and shows the result in the
  chat [J25][J27]. DBeaver Community gained local models through Ollama [D33]; TablePlus's chat
  takes Ollama and any ACP agent, and now reaches its diagrams [T13][T3]. A one-click **Fix with
  AI** under a failed query is in DBX [X16] and Tabularis [L13]; DbGate explains an error in
  Premium [L19]. DbRover's answer is still MCP: the agent stays in the client people already
  use, with DbRover's permissions, logins and history around it. That holds as long as the MCP
  path stays as easy as a button in the editor; a narrow, opt-in Fix with AI that hands the error
  to the user's own agent is the one piece worth weighing.
- **Plugin systems.** Beekeeper Studio opened its plugin system to everyone in 6.2 [B18][B19];
  DBX has a store of signed, sandboxed plugins [X20]; Tabularis's drivers are plugins in any
  language over JSON-RPC [L15]; TablePlus has JavaScript plugins in development [T19]. DBeaver
  and DataGrip have always had them. DbRover's actions, commands and export programs [R14][R27]
  cover running your own programs on your data; sandboxed WebAssembly plugins are H3, started
  once three real needs outgrow those [R35].

### Closed since October

All six gaps ranked in October were picked on 2026-10-29 and are built:

| Gap in October | Built as | Released in | Pull requests |
| --- | --- | --- | --- |
| ER diagrams, read-only first | ER1, [the diagrams track](roadmap.md#diagrams-track) [R30] | v0.21.0 | #315, #317, #320, #321, #327, #328 |
| A password command | W9 [R3] | v0.16.0 | #252, #253 |
| A visual explain plan | W10 [R24] | v0.20.0 | #291, #294, #296, #302, #303 |
| Read-only on every engine | W11 [R11]; SQL Server and ClickHouse served to MCP through an MCP login since v0.16.0 | v0.21.0 | #310, #311, #313, #314 |
| Schema compare | W13, with VC3's schema snapshots [R31] | v0.28.0 | #483 to #498 |
| Charts from results | W12, then CH1 to CH5 for charts in a query's text, in reports and for MCP [R25] | v0.21.0; CH1 to CH5 in v0.29.0 to v0.35.0 | #319, #324, #325, #329 |

The export and report formats the October table left to the roadmap are built too: W7 in
v0.15.0 and W8 in v0.22.0 [R26][R27].

## What DbRover brings

The ideas DbRover is built around, and the ones it shares with other tools. Each holds among
DBeaver, DataGrip, TablePlus, Beekeeper Studio and DBX as their own docs describe them.

1. **A native app, free and open source on all three systems, with no account.** DbRover draws
   its window itself, in Rust with egui, so it opened in about a third of a second when
   measured (0.13.0) and stays light. TablePlus is native too, and paid; DBX is free and quick
   to start, with its interface in the system's web view.
2. **AI agents whose reads the database itself keeps read-only.** DbRover's MCP server runs
   every read in the engine's own read-only mode wherever it has one (a read-only transaction,
   a file opened read-only, ClickHouse's `readonly`), so even a write hidden in a function
   fails; an engine without one is served only through a login, profile or service account
   that can only read. The server is part of the app, off until a project is allowed,
   read-only by default, with separate permissions for writes, schema changes, admin, reports
   and server activity, a row limit and a timeout. DataGrip, TablePlus and DBX serve MCP too,
   with consent prompts and permission levels of their own [J9][T14][X5], and Tabularis also
   reads in a read-only session on its built-in drivers [L4].
3. **Reports: one query over several databases, run again and again into files.** A report
   reads one data source or several, combines them in DuckDB SQL or PRQL, and writes CSV,
   Excel, Parquet and charts, in a process of its own with limits you can see, larger than
   memory if it must, from the app, the command line or an MCP client [R32]. DataGrip 2026.3
   now joins data sources in DuckDB in a query [J25]; none of the five documents reports of
   this kind.
4. **Actions that run your own programs on the selected rows,** declared in a TOML file next to
   your keymap, snippets and theme. The plugin systems elsewhere add drivers, panels and menu
   commands [B19][X20][T19]; none of them documents running a program you already have on the
   rows you picked.

Shared with one or two of the others, and done well:

- **Safety levels.** A careful data source starts its consoles in manual commit, names itself in
  every confirmation, and waits for a table's name before dropping it. DBeaver's Production
  type, DBX's production protection and TablePlus's Safe Mode follow the same idea.
- **A history you can audit.** Every statement from every data source in one search, tagged by
  where it came from down to the AI client, a grid edit's old values with the statement that
  puts them back, and passwords redacted before they are stored. DBX keeps a log of MCP calls
  with the client; Beekeeper has a history of grid edits.
- **Server activity.** A server's sessions, who waits on whose lock, and Cancel and Kill, each
  stop recorded in the history and offered to an agent only with its own permission [R29].
  DBeaver's Session Manager, TablePlus's and DBX's Process List do the same job.
- **Warehouses with their costs in view.** Snowflake, BigQuery, Databricks, Athena and Trino
  beside the transactional engines, each signed in its own way, with a byte or time limit you set,
  and BigQuery's dry run, before a costly statement [R17][R20][R21][R22][R23]. DataGrip and
  TablePlus reach most of these engines too.
- **Settings as plain files, in Git.** Keymap, actions, snippets, themes, export formats,
  reports, diagrams and data sources (without their secrets) are files you can read, edit and
  keep in Git, with schema snapshots to diff and folders of queries listed in the explorer
  [R33][R34][R31]. DataGrip and Tabularis commit from inside the app [J34][L13]; DbRover leaves
  Git to Git.

What DbRover doesn't do yet, said as plainly on the site: search across a database's data
(being built as W16), dump and restore, users and roles beyond PostgreSQL, image, hex and map
viewers, a vim mode, quick open and split editors, row data compare, a query builder, mock data,
notebooks, a debugger, schema compare beyond PostgreSQL, editing from an ER diagram, Oracle,
import beyond CSV, AI chat inside the app, and plugins beyond actions and commands.

## Sources

DbRover's links are to this repository. Other tools' pages were read on 2026-10-28 unless a
line says otherwise; a line marked 2026-11-25 was read or re-read for this refresh, and the
cells that cite it carry that day's facts. Each tag in the tables links here and to its page.

**DbRover** (0.39.0, as on main on 2026-11-25)

- R1: [Manual: Introduction](../book/src/introduction.md)
- R2: [More databases (and the roadmap's database track)](databases.md)
- R3: [Manual: Data sources (the kinds, TLS, SSH tunnels, safety levels, placeholders, the keychain, a password from a command)](../book/src/data-sources.md)
- R5: [Manual: Query consoles (completion, formatting, problems, the value panel)](../book/src/consoles.md)
- R6: [Manual: Files, parameters and find](../book/src/files-parameters.md)
- R7: [Manual: Working with table data](../book/src/table-data.md)
- R8: [Manual: Creating and changing tables](../book/src/objects.md)
- R10: [Manual: The query history](../book/src/history.md)
- R11: [Manual: Connecting an AI assistant](../book/src/mcp.md)
- R12: [Manual: Install and first launch](../book/src/install.md)
- R13: [Manual: Moving data sources to another machine](../book/src/sharing.md)
- R14: [Manual: Actions, snippets and themes](../book/src/customising.md)
- R15: [ADR 0001: the UI toolkit is egui](adr/0001-ui-toolkit-egui.md)
- R16: [NoSQL databases (the plan; N0 to N5 built)](nosql.md)
- R17: [Manual: Cloud sign-in and costs](../book/src/cloud.md)
- R18: [Manual: Managed databases in the cloud](../book/src/cloud-services.md)
- R19: [Manual: Object storage and MotherDuck](../book/src/object-storage.md)
- R20: [Manual: Athena and Trino](../book/src/athena-trino.md)
- R21: [Manual: BigQuery](../book/src/bigquery.md)
- R22: [Manual: Snowflake](../book/src/snowflake.md)
- R23: [Manual: Databricks](../book/src/databricks.md)
- R24: [Manual: Reading query plans](../book/src/explain-plans.md)
- R25: [Manual: Charts](../book/src/charts.md)
- R26: [Manual: Exporting and copying rows](../book/src/exporting.md)
- R27: [Manual: Your own export formats](../book/src/export-formats.md)
- R28: [Manual: Users and roles](../book/src/users-roles.md)
- R29: [Manual: Server activity](../book/src/server-activity.md)
- R30: [Manual: ER diagrams](../book/src/diagrams.md)
- R31: [Manual: Comparing schemas](../book/src/schema-compare.md)
- R32: [Manual: Reports](../book/src/reports.md)
- R33: [Manual: Where DbRover keeps its files (keeping your settings in Git)](../book/src/files.md)
- R34: [Manual: The explorer (query folders)](../book/src/explorer.md)
- R35: [Roadmap (H3, W15, the milestones above)](roadmap.md)

**DBeaver**

- D1: <https://dbeaver.com/edition/> (read 2026-10-28; prices and editions re-read 2026-11-25)
- D2: <https://github.com/dbeaver/dbeaver> (2026-10-28; stars, releases 26.2.3 to 26.2.5.2 and the open 26.3.0 milestone re-read via the GitHub API 2026-11-25)
- D3: <https://dbeaver.io/> (2026-10-28)
- D4: <https://dbeaver.com/databases/> (2026-10-28)
- D6: <https://dbeaver.com/docs/dbeaver/AI-Smart-Assistance/> (2026-10-28, re-read 2026-11-25)
- D9: <https://dbeaver.com/docs/dbeaver/Model-Context-Protocol-Server/> (2026-10-28, re-read 2026-11-25)
- D11: <https://dbeaver.com/docs/dbeaver/Connection-Types/> (2026-10-28)
- D12: <https://dbeaver.com/docs/dbeaver/Managing-security-restrictions-for-database-connection/> (2026-10-28)
- D13: <https://dbeaver.com/docs/dbeaver/SSH-Configuration/> (2026-10-28)
- D14: <https://dbeaver.com/docs/dbeaver/AWS-Credentials/> (2026-10-28)
- D15: <https://dbeaver.com/docs/dbeaver/SQL-Assist-and-Auto-Complete/> (2026-10-28)
- D16: <https://dbeaver.com/docs/dbeaver/SQL-Formatting/> (2026-10-28)
- D17: <https://dbeaver.com/docs/dbeaver/Query-Execution-Plan/> (2026-10-28)
- D18: <https://dbeaver.com/docs/dbeaver/Data-transfer/> (2026-10-28)
- D20: <https://dbeaver.com/docs/dbeaver/Data-import/> (2026-10-28)
- D21: <https://dbeaver.com/docs/dbeaver/Schema-compare/> (2026-10-28)
- D22: <https://dbeaver.com/docs/dbeaver/Simple-Structure-Compare/> (2026-10-28)
- D23: <https://dbeaver.com/docs/dbeaver/ER-Diagrams/> (2026-10-28, re-read 2026-10-29)
- D25: <https://dbeaver.com/docs/dbeaver/Query-Manager/> (2026-10-28)
- D27: <https://dbeaver.io/download/> (2026-10-28)
- D28: <https://dbeaver.com/docs/dbeaver/Custom-Diagrams/> (2026-10-29)
- D29: <https://dbeaver.com/dbeaver-lite/> (2026-10-28, re-read 2026-11-25)
- D31: <https://dbeaver.com/dbeaver-enterprise/> (2026-10-28, re-read 2026-11-25)
- D32: <https://dbeaver.com/dbeaver-ultimate/> (2026-10-28, re-read 2026-11-25)
- D33: <https://github.com/dbeaver/dbeaver/releases/tag/26.2.5> (26.2.5 of 2026-11-15, "Added Ollama support for running local AI models"; also <https://dbeaver.io/2026/11/15/dbeaver-26-2-5/>, 2026-11-25)
- D35: <https://dbeaver.com/docs/dbeaver/Backup-Restore/> (2026-11-25)
- D36: <https://dbeaver.com/docs/dbeaver/Data-compare/> (2026-11-25)
- D37: <https://dbeaver.com/docs/dbeaver/Session-Manager-Guide/> (2026-11-25)
- D38: <https://dbeaver.com/docs/dbeaver/Value-Panel/> (2026-11-25)
- D39: <https://dbeaver.com/docs/dbeaver/Visual-Query-Builder/> (2026-11-25)
- D40: <https://dbeaver.com/docs/dbeaver/AI-integration-with-Ollama/> (2026-11-25; still says Lite, Enterprise and Ultimate only, against 26.2.5's release notes)

D24 (DBeaver Lite 26.2's page, seen only in a search listing) is no longer cited.

**DataGrip**

- J1: <https://www.jetbrains.com/datagrip/> (rendered page incl. FAQ, 2026-10-28; re-read 2026-11-25)
- J2: <https://www.jetbrains.com/datagrip/buy/> (rendered; ?section=personal and ?section=commercial, billing=yearly/monthly, 2026-10-28; still a subscription, priced by country, 2026-11-25)
- J3: <https://www.jetbrains.com/datagrip/whatsnew/> (2026.2, rendered, 2026-10-28; rendered empty on 2026-11-25)
- J5: <https://www.jetbrains.com/datagrip/whatsnew/2025-3/> (2026-10-28)
- J6: <https://lp.jetbrains.com/datagrip/sql-support/> (2026-10-28)
- J7: <https://www.jetbrains.com/help/datagrip/data-sources-and-drivers-dialog.html> (2026-10-28)
- J8: <https://lp.jetbrains.com/datagrip/working-with-data/> (2026-10-28)
- J9: <https://www.jetbrains.com/help/datagrip/mcp-server.html> (2026-10-28, re-read 2026-11-25)
- J10: <https://www.jetbrains.com/help/datagrip/duckdb.html> (2026-10-28)
- J14: <https://www.jetbrains.com/help/datagrip/reformat-file-dialog.html> (2026-10-28)
- J15: <https://www.jetbrains.com/help/datagrip/query-execution-plan.html> (2026-10-28, re-read 2026-11-25)
- J16: <https://www.jetbrains.com/help/datagrip/import-data.html> (2026-10-29)
- J17: <https://www.jetbrains.com/help/datagrip/schema-comparison-and-migration.html> (via official search results, 2026-10-28; re-read 2026-11-25)
- J18: <https://www.jetbrains.com/help/datagrip/creating-diagrams.html> (2026-10-28, re-read 2026-10-29 and 2026-11-25)
- J19: <https://www.jetbrains.com/help/datagrip/run-a-query.html> and https://www.jetbrains.com/help/datagrip/find-recent-queries-and-files.html (via official search results, 2026-10-28)
- J22: <https://www.jetbrains.com/datagrip/whatsnew/2026-1/> (rendered, 2026-10-28)
- J23: <https://blog.jetbrains.com/datagrip/2025/10/01/datagrip-is-now-free-for-non-commercial-use/> (via search; WebFetch returned 403, 2026-10-28)
- J24: <https://www.jetbrains.com/help/datagrip/installation-guide.html> (2026-10-28)
- J25: <https://blog.jetbrains.com/datagrip/2026/11/05/datagrip-2026-3-agentic-development-with-jetbrains-air-cross-database-queries-duckdb-support-and-major-ux-improvements/> (2026.3, released 2026-11-05 per https://data.services.jetbrains.com/products/releases?code=DG; release notes https://youtrack.jetbrains.com/api/articles/DBE-A-83165563; 2026-11-25)
- J27: <https://www.jetbrains.com/help/datagrip/air-in-jetbrains-ides.html> (2026-11-25; calls Air an EAP plugin where the 2026.3 post says it is on by default)
- J28: <https://www.jetbrains.com/help/datagrip/full-text-search-for-databases.html> (2026-11-25)
- J29: <https://www.jetbrains.com/help/datagrip/compare-data.html> (2026-11-25)
- J30: <https://www.jetbrains.com/help/datagrip/database-users-and-roles.html> (2026-11-25)
- J31: <https://www.jetbrains.com/help/datagrip/data-editor.html> (2026-11-25)
- J32: <https://www.jetbrains.com/help/datagrip/using-product-as-the-vim-editor.html> (2026-11-25)
- J33: <https://www.jetbrains.com/help/datagrip/search-in-ide.html> (2026-11-25)
- J34: <https://www.jetbrains.com/help/datagrip/using-git-integration.html> (2026-11-25)
- J35: <https://www.jetbrains.com/help/datagrip/managing-data-sources.html> (the engines with complete support, 2026-11-25)
- J36: <https://www.jetbrains.com/help/datagrip/other-databases.html> (the engines with basic support, 2026-11-25)
- J37: <https://www.jetbrains.com/help/datagrip/import-sql-dump-files.html> (2026-11-25)
- J38: <https://www.jetbrains.com/help/datagrip/settings-database-ai-tools.html> (the four AI tools settings, 2026-11-25; replaces J22 for the consents)
- J39: <https://www.jetbrains.com/help/datagrip/managing-plugins.html> (2026-11-25)
- J40: <https://www.jetbrains.com/help/datagrip/managing-connection-sessions.html> (the sessions DataGrip opens, and Cancel Running Statements, 2026-11-25)

**TablePlus**

- T1: <https://tableplus.com/> (2026-10-28, re-read 2026-11-25)
- T2: <https://tableplus.com/pricing> (2026-10-28, re-read 2026-11-25)
- T3: <https://tableplus.com/osx/changelog> (2026-10-28; re-read 2026-11-25 up to 27.4.0, build 854, of 2026-11-21)
- T4: <https://tableplus.com/docs/gui-tools/manage-connections> (2026-10-28, re-read 2026-11-25)
- T5: <https://tableplus.com/docs/query-editor/autocomplete> (via official search results, 2026-10-28)
- T6: <https://tableplus.com/docs/query-editor/working-with-query-results> (2026-10-28, re-read 2026-11-25)
- T7: <https://tableplus.com/docs/gui-tools/code-review-and-safemode> (2026-10-29)
- T8: <https://tableplus.com/docs/gui-tools/import-and-export> (2026-10-29)
- T9: <https://docs.tableplus.com/gui-tools/working-with-table/column> (via official search results, 2026-10-28)
- T11: <https://tableplus.com/docs/gui-tools/diagram> (2026-10-29, re-read 2026-11-25)
- T12: <https://tableplus.com/docs/query-editor/query-history> (2026-10-28)
- T13: <https://tableplus.com/docs/llm-plugin> (2026-10-28, re-read 2026-11-25)
- T14: <https://tableplus.com/docs/preferences/mcp> (2026-10-28, re-read 2026-11-25)
- T15: <https://tableplus.com/docs/preferences/crash-and-security> (2026-10-28)
- T16: <https://tableplus.com/linux> (2026-10-28, re-read 2026-11-25)
- T17: <https://tableplus.com/docs/gui-tools/sync-database> (2026-11-25)
- T18: <https://tableplus.com/docs/gui-tools/schedule-tasks> (2026-11-25)
- T19: <https://tableplus.com/docs/utilities/plugin> (2026-11-25)
- T20: <https://tableplus.com/docs/gui-tools/user-management> (2026-11-25)
- T21: <https://tableplus.com/docs/gui-tools/the-interface/menu> (2026-11-25)
- T22: <https://tableplus.com/linux/changelog> (27.4.0, build 400, of 2026-11-21; 2026-11-25)
- T23: <https://tableplus.com/blog/2026/11/tableplus-2026-ten-years.html> (2026-11-25)

**Beekeeper Studio**

- B1: <https://www.beekeeperstudio.io/pricing/> (rendered comparison table and price cards, monthly and yearly, 2026-10-28; re-read 2026-11-25)
- B2: <https://github.com/beekeeper-studio/beekeeper-studio> (2026-10-28; stars and latest release, v6.3.3 of 2026-11-19, re-read 2026-11-25)
- B3: <https://docs.beekeeperstudio.io/user_guide/connecting/connecting/> (2026-10-28)
- B8: <https://www.beekeeperstudio.io/category/release/> (2026-10-28)
- B9: <https://docs.beekeeperstudio.io/user_guide/sql_editor/editor/> (2026-10-28)
- B10: <https://docs.beekeeperstudio.io/user_guide/sql-query-formatter/> (listed in docs index, 2026-10-28; not opened)
- B11: <https://docs.beekeeperstudio.io/user_guide/importing-data-csv-json-etc/> (2026-10-28)
- B12: <https://docs.beekeeperstudio.io/user_guide/data-export/> (2026-10-28)
- B13: <https://docs.beekeeperstudio.io/user_guide/entity-relationship-diagrams-erd/> (2026-10-28, re-read 2026-10-29)
- B14: <https://www.beekeeperstudio.io/features/sql-ai> (2026-10-28, re-read 2026-11-25)
- B16: <https://www.beekeeperstudio.io/plugins/gatekeeper/> (2026-10-28, re-read 2026-11-25)
- B17: <https://github.com/beekeeper-studio/beekeeper-studio/releases/tag/v6.1.0> (2026-10-28)
- B18: <https://github.com/beekeeper-studio/beekeeper-studio/releases/tag/v6.2.0> (6.2.0 of 2026-10-28: plugins, relative line numbers in vim mode; 2026-11-25)
- B19: <https://www.beekeeperstudio.io/blog/release-6-2-plugins> (plugins and their limits per tier, the official map, explain and pivot-table plugins, community plugins; 2026-11-25)

**DBX**

- X1: <https://github.com/t8y2/dbx> (README, read via GitHub API, 2026-10-29; re-read with its stars 2026-11-25)
- X3: <https://github.com/t8y2/dbx/releases> (latest v0.6.60, 2026-10-28T20:12Z, read 2026-10-29; re-read 2026-11-25: v0.6.61 to v0.6.74, the last on 2026-11-24, whose `DBX_0.6.74_arm64.dmg` is 46,736,211 bytes)
- X4: <https://dbxio.com/en/docs/databases> (source: docs/content/docs/databases.mdx, 2026-10-29; re-read 2026-11-25)
- X5: <https://dbxio.com/en/docs/mcp> (source: docs/content/docs/mcp.mdx) and https://github.com/t8y2/dbx/blob/main/packages/mcp-server/README.md (2026-10-29)
- X6: <https://dbxio.com/en/docs/production-safety> (source .mdx read, and live page spot-checked, 2026-10-29; re-read 2026-11-25)
- X7: <https://dbxio.com/en/docs/ssh-tunnel> (source .mdx, 2026-10-29)
- X8: <https://dbxio.com/en/docs/query-editor> (source .mdx, 2026-10-29)
- X9: <https://dbxio.com/en/docs/data-grid> (source .mdx, 2026-10-29; re-read 2026-11-25)
- X10: <https://dbxio.com/en/docs/table-import> (source .mdx, 2026-10-29)
- X11: <https://dbxio.com/en/docs/schema-diff> (source .mdx, 2026-10-29; re-read 2026-11-25)
- X12: <https://dbxio.com/en/docs/what-is-dbx> and https://dbxio.com/en/docs/getting-started (source .mdx, 2026-10-29)
- X14: <https://github.com/t8y2/dbx/blob/main/apps/desktop/src/i18n/locales/en.ts> (official app UI strings, `diagram` and connection-form sections, 2026-10-29; chart, data generator, process list and user sections, 2026-11-25)
- X16: <https://dbxio.com/en/docs/ai-assistant> (source .mdx, 2026-10-29; re-read 2026-11-25)
- X17: <https://dbxio.com/en/privacy> (effective 2026-09-27; read 2026-10-29)
- X18: <https://dbxio.com/en/docs/cloud-sync> (source .mdx, 2026-10-29)
- X19: <https://dbxio.com/> (2026-10-29)
- X20: <https://dbxio.com/en/docs/plugin-development> (source .mdx, 2026-11-25)
- X21: <https://dbxio.com/en/docs/database-backup> (source .mdx, 2026-11-25)
- X22: <https://dbxio.com/en/docs/plsql-debug> (source .mdx, 2026-11-25)

**Other tools and surveys**

- L1: [Stack Overflow Developer Survey 2025, Technology](https://survey.stackoverflow.co/2025/technology/)
- L2: [JetBrains State of Developer Ecosystem 2025, Tools and trends](https://devecosystem-2025.jetbrains.com/tools-and-trends)
- L3: [Tabularis repository (stars and creation date via the GitHub API, re-read 2026-11-25)](https://github.com/TabularisDB/tabularis)
- L4: [Tabularis MCP server docs (with its read-only mode and approval gates, re-read 2026-11-25)](https://tabularis.dev/wiki/mcp-server)
- L5: [DbGate 7.2.3 release notes (read-only MCP server in the Community Docker build; 2026-11-25)](https://www.dbgate.io/news/2026-07-21-7-2-3-mcp-server/)
- L6: [TablePro repository (stars, platforms; 2026-11-25)](https://github.com/TableProApp/TablePro)
- L7: [Chat2DB repository (stars, licence; 2026-11-25)](https://github.com/OtterMind/Chat2DB)
- L8: [DBCode on the VS Code Marketplace (2026-11-25)](https://marketplace.visualstudio.com/items?itemName=DBCode.dbcode)
- L9: [Sequel Ace repository (MCP server in the README; 2026-11-25)](https://github.com/Sequel-Ace/Sequel-Ace)
- L10: [DBFlux; also https://github.com/feigeCode/gpui-kit (OnetCli), https://github.com/samurmaykrr/zqlz (ZQLZ), https://github.com/frkdrgt/ferox (Ferox), https://github.com/cmjcode/tabular (Tabular); 2026-11-25](https://github.com/0xErwin1/dbflux)
- L11: [What's happening to Azure Data Studio (page updated 2026-10-19; re-read 2026-11-25)](https://learn.microsoft.com/en-us/sql/tools/whats-happening-azure-data-studio?view=sql-server-ver17)
- L12: [Postico 2: pre-connect script (and https://www.eggerapps.at/postico2/, re-read 2026-11-25)](https://eggerapps.at/postico2/documentation/connection-preconnect-script.html)
- L13: [Tabularis v0.33.0 release notes (2026-11-16: Fix with AI, vim mode, Git commits of SQL files; 2026-11-25)](https://github.com/TabularisDB/tabularis/releases/tag/v0.33.0)
- L14: [Tabularis wiki: dump and import (2026-11-25)](https://tabularis.dev/wiki/dump-import)
- L15: [Tabularis wiki: plugins (2026-11-25)](https://tabularis.dev/wiki/plugins)
- L16: [Tabularis v0.4.0 release notes (visual query builder, 2026-01-27; notebooks came in https://github.com/TabularisDB/tabularis/releases/tag/v0.9.15, 2026-04-08; 2026-11-25)](https://github.com/TabularisDB/tabularis/releases/tag/v0.4.0)
- L18: [DbGate changelog (backup and restore from 7.2.4, Explain SQL error; 2026-11-25)](https://github.com/dbgate/dbgate/blob/master/CHANGELOG.md)
- L19: [DbGate pricing (Premium's AI tools, data compare, query designer; 2026-11-25)](https://www.dbgate.io/pricing/)

[R1]: ../book/src/introduction.md
[R2]: databases.md
[R3]: ../book/src/data-sources.md
[R5]: ../book/src/consoles.md
[R6]: ../book/src/files-parameters.md
[R7]: ../book/src/table-data.md
[R8]: ../book/src/objects.md
[R10]: ../book/src/history.md
[R11]: ../book/src/mcp.md
[R12]: ../book/src/install.md
[R13]: ../book/src/sharing.md
[R14]: ../book/src/customising.md
[R15]: adr/0001-ui-toolkit-egui.md
[R16]: nosql.md
[R17]: ../book/src/cloud.md
[R18]: ../book/src/cloud-services.md
[R19]: ../book/src/object-storage.md
[R20]: ../book/src/athena-trino.md
[R21]: ../book/src/bigquery.md
[R22]: ../book/src/snowflake.md
[R23]: ../book/src/databricks.md
[R24]: ../book/src/explain-plans.md
[R25]: ../book/src/charts.md
[R26]: ../book/src/exporting.md
[R27]: ../book/src/export-formats.md
[R28]: ../book/src/users-roles.md
[R29]: ../book/src/server-activity.md
[R30]: ../book/src/diagrams.md
[R31]: ../book/src/schema-compare.md
[R32]: ../book/src/reports.md
[R33]: ../book/src/files.md
[R34]: ../book/src/explorer.md
[R35]: roadmap.md
[D1]: https://dbeaver.com/edition/
[D2]: https://github.com/dbeaver/dbeaver
[D3]: https://dbeaver.io/
[D4]: https://dbeaver.com/databases/
[D6]: https://dbeaver.com/docs/dbeaver/AI-Smart-Assistance/
[D9]: https://dbeaver.com/docs/dbeaver/Model-Context-Protocol-Server/
[D11]: https://dbeaver.com/docs/dbeaver/Connection-Types/
[D12]: https://dbeaver.com/docs/dbeaver/Managing-security-restrictions-for-database-connection/
[D13]: https://dbeaver.com/docs/dbeaver/SSH-Configuration/
[D14]: https://dbeaver.com/docs/dbeaver/AWS-Credentials/
[D15]: https://dbeaver.com/docs/dbeaver/SQL-Assist-and-Auto-Complete/
[D16]: https://dbeaver.com/docs/dbeaver/SQL-Formatting/
[D17]: https://dbeaver.com/docs/dbeaver/Query-Execution-Plan/
[D18]: https://dbeaver.com/docs/dbeaver/Data-transfer/
[D20]: https://dbeaver.com/docs/dbeaver/Data-import/
[D21]: https://dbeaver.com/docs/dbeaver/Schema-compare/
[D22]: https://dbeaver.com/docs/dbeaver/Simple-Structure-Compare/
[D23]: https://dbeaver.com/docs/dbeaver/ER-Diagrams/
[D25]: https://dbeaver.com/docs/dbeaver/Query-Manager/
[D27]: https://dbeaver.io/download/
[D28]: https://dbeaver.com/docs/dbeaver/Custom-Diagrams/
[D29]: https://dbeaver.com/dbeaver-lite/
[D31]: https://dbeaver.com/dbeaver-enterprise/
[D32]: https://dbeaver.com/dbeaver-ultimate/
[D33]: https://github.com/dbeaver/dbeaver/releases/tag/26.2.5
[D35]: https://dbeaver.com/docs/dbeaver/Backup-Restore/
[D36]: https://dbeaver.com/docs/dbeaver/Data-compare/
[D37]: https://dbeaver.com/docs/dbeaver/Session-Manager-Guide/
[D38]: https://dbeaver.com/docs/dbeaver/Value-Panel/
[D39]: https://dbeaver.com/docs/dbeaver/Visual-Query-Builder/
[D40]: https://dbeaver.com/docs/dbeaver/AI-integration-with-Ollama/
[J1]: https://www.jetbrains.com/datagrip/
[J2]: https://www.jetbrains.com/datagrip/buy/
[J3]: https://www.jetbrains.com/datagrip/whatsnew/
[J5]: https://www.jetbrains.com/datagrip/whatsnew/2025-3/
[J6]: https://lp.jetbrains.com/datagrip/sql-support/
[J7]: https://www.jetbrains.com/help/datagrip/data-sources-and-drivers-dialog.html
[J8]: https://lp.jetbrains.com/datagrip/working-with-data/
[J9]: https://www.jetbrains.com/help/datagrip/mcp-server.html
[J10]: https://www.jetbrains.com/help/datagrip/duckdb.html
[J14]: https://www.jetbrains.com/help/datagrip/reformat-file-dialog.html
[J15]: https://www.jetbrains.com/help/datagrip/query-execution-plan.html
[J16]: https://www.jetbrains.com/help/datagrip/import-data.html
[J17]: https://www.jetbrains.com/help/datagrip/schema-comparison-and-migration.html
[J18]: https://www.jetbrains.com/help/datagrip/creating-diagrams.html
[J19]: https://www.jetbrains.com/help/datagrip/run-a-query.html
[J22]: https://www.jetbrains.com/datagrip/whatsnew/2026-1/
[J23]: https://blog.jetbrains.com/datagrip/2025/10/01/datagrip-is-now-free-for-non-commercial-use/
[J24]: https://www.jetbrains.com/help/datagrip/installation-guide.html
[J25]: https://blog.jetbrains.com/datagrip/2026/11/05/datagrip-2026-3-agentic-development-with-jetbrains-air-cross-database-queries-duckdb-support-and-major-ux-improvements/
[J27]: https://www.jetbrains.com/help/datagrip/air-in-jetbrains-ides.html
[J28]: https://www.jetbrains.com/help/datagrip/full-text-search-for-databases.html
[J29]: https://www.jetbrains.com/help/datagrip/compare-data.html
[J30]: https://www.jetbrains.com/help/datagrip/database-users-and-roles.html
[J31]: https://www.jetbrains.com/help/datagrip/data-editor.html
[J32]: https://www.jetbrains.com/help/datagrip/using-product-as-the-vim-editor.html
[J33]: https://www.jetbrains.com/help/datagrip/search-in-ide.html
[J34]: https://www.jetbrains.com/help/datagrip/using-git-integration.html
[J35]: https://www.jetbrains.com/help/datagrip/managing-data-sources.html
[J36]: https://www.jetbrains.com/help/datagrip/other-databases.html
[J37]: https://www.jetbrains.com/help/datagrip/import-sql-dump-files.html
[J38]: https://www.jetbrains.com/help/datagrip/settings-database-ai-tools.html
[J39]: https://www.jetbrains.com/help/datagrip/managing-plugins.html
[J40]: https://www.jetbrains.com/help/datagrip/managing-connection-sessions.html
[T1]: https://tableplus.com/
[T2]: https://tableplus.com/pricing
[T3]: https://tableplus.com/osx/changelog
[T4]: https://tableplus.com/docs/gui-tools/manage-connections
[T5]: https://tableplus.com/docs/query-editor/autocomplete
[T6]: https://tableplus.com/docs/query-editor/working-with-query-results
[T7]: https://tableplus.com/docs/gui-tools/code-review-and-safemode
[T8]: https://tableplus.com/docs/gui-tools/import-and-export
[T9]: https://docs.tableplus.com/gui-tools/working-with-table/column
[T11]: https://tableplus.com/docs/gui-tools/diagram
[T12]: https://tableplus.com/docs/query-editor/query-history
[T13]: https://tableplus.com/docs/llm-plugin
[T14]: https://tableplus.com/docs/preferences/mcp
[T15]: https://tableplus.com/docs/preferences/crash-and-security
[T16]: https://tableplus.com/linux
[T17]: https://tableplus.com/docs/gui-tools/sync-database
[T18]: https://tableplus.com/docs/gui-tools/schedule-tasks
[T19]: https://tableplus.com/docs/utilities/plugin
[T20]: https://tableplus.com/docs/gui-tools/user-management
[T21]: https://tableplus.com/docs/gui-tools/the-interface/menu
[T22]: https://tableplus.com/linux/changelog
[T23]: https://tableplus.com/blog/2026/11/tableplus-2026-ten-years.html
[B1]: https://www.beekeeperstudio.io/pricing/
[B2]: https://github.com/beekeeper-studio/beekeeper-studio
[B3]: https://docs.beekeeperstudio.io/user_guide/connecting/connecting/
[B8]: https://www.beekeeperstudio.io/category/release/
[B9]: https://docs.beekeeperstudio.io/user_guide/sql_editor/editor/
[B10]: https://docs.beekeeperstudio.io/user_guide/sql-query-formatter/
[B11]: https://docs.beekeeperstudio.io/user_guide/importing-data-csv-json-etc/
[B12]: https://docs.beekeeperstudio.io/user_guide/data-export/
[B13]: https://docs.beekeeperstudio.io/user_guide/entity-relationship-diagrams-erd/
[B14]: https://www.beekeeperstudio.io/features/sql-ai
[B16]: https://www.beekeeperstudio.io/plugins/gatekeeper/
[B17]: https://github.com/beekeeper-studio/beekeeper-studio/releases/tag/v6.1.0
[B18]: https://github.com/beekeeper-studio/beekeeper-studio/releases/tag/v6.2.0
[B19]: https://www.beekeeperstudio.io/blog/release-6-2-plugins
[X1]: https://github.com/t8y2/dbx
[X3]: https://github.com/t8y2/dbx/releases
[X4]: https://dbxio.com/en/docs/databases
[X5]: https://dbxio.com/en/docs/mcp
[X6]: https://dbxio.com/en/docs/production-safety
[X7]: https://dbxio.com/en/docs/ssh-tunnel
[X8]: https://dbxio.com/en/docs/query-editor
[X9]: https://dbxio.com/en/docs/data-grid
[X10]: https://dbxio.com/en/docs/table-import
[X11]: https://dbxio.com/en/docs/schema-diff
[X12]: https://dbxio.com/en/docs/what-is-dbx
[X14]: https://github.com/t8y2/dbx/blob/main/apps/desktop/src/i18n/locales/en.ts
[X16]: https://dbxio.com/en/docs/ai-assistant
[X17]: https://dbxio.com/en/privacy
[X18]: https://dbxio.com/en/docs/cloud-sync
[X19]: https://dbxio.com/
[X20]: https://dbxio.com/en/docs/plugin-development
[X21]: https://dbxio.com/en/docs/database-backup
[X22]: https://dbxio.com/en/docs/plsql-debug
[L1]: https://survey.stackoverflow.co/2025/technology/
[L2]: https://devecosystem-2025.jetbrains.com/tools-and-trends
[L3]: https://github.com/TabularisDB/tabularis
[L4]: https://tabularis.dev/wiki/mcp-server
[L5]: https://www.dbgate.io/news/2026-07-21-7-2-3-mcp-server/
[L6]: https://github.com/TableProApp/TablePro
[L7]: https://github.com/OtterMind/Chat2DB
[L8]: https://marketplace.visualstudio.com/items?itemName=DBCode.dbcode
[L9]: https://github.com/Sequel-Ace/Sequel-Ace
[L10]: https://github.com/0xErwin1/dbflux
[L11]: https://learn.microsoft.com/en-us/sql/tools/whats-happening-azure-data-studio?view=sql-server-ver17
[L12]: https://eggerapps.at/postico2/documentation/connection-preconnect-script.html
[L13]: https://github.com/TabularisDB/tabularis/releases/tag/v0.33.0
[L14]: https://tabularis.dev/wiki/dump-import
[L15]: https://tabularis.dev/wiki/plugins
[L16]: https://github.com/TabularisDB/tabularis/releases/tag/v0.4.0
[L18]: https://github.com/dbgate/dbgate/blob/master/CHANGELOG.md
[L19]: https://www.dbgate.io/pricing/
