# Users beyond PostgreSQL

Status: **planned on 2026-11-27** (W18a), its twelve open questions answered the same day, all
as proposed. The [roadmap](roadmap.md#well-rounded-track) tracks it as W18 in the well-rounded
track. Third in the competitor-gap review of 2026-11-25 ([comparison](comparison.md), gap 3),
after server activity (W15) and searching data (W16); dump and restore (W17) is planned beside
it.

A new service needs a database login that may read one schema. A colleague leaves, and their
account has to be locked today and dropped next week. An application's password has to be
changed without taking it down. On PostgreSQL, DbRover does all of this from the explorer's
**roles** folder (E3, [Users and roles](../book/src/users-roles.md)). On MySQL, MariaDB and SQL
Server it does none of it: the manual says their accounts and logins "are planned on their
own", and the only way is to write `CREATE USER` and `GRANT` by hand in a console, after looking
up which of the engine's many spellings this server takes.

W18 adds them: **MySQL's and MariaDB's `'name'@'host'` accounts and roles**, and **SQL Server's
logins, server roles, database users and database roles**, listed in the explorer and created,
changed, renamed and dropped through the same dialogs and review panel as PostgreSQL's roles,
with the password hashed on the client where the engine allows it.

## The question

Three tasks come up, in this order of frequency:

1. **"Give this service a login."** Create a user with a password, let it read (or write) one
   database or schema, and nothing else.
2. **"What can this account do?"** Its roles, its privileges at each level, whether it is
   locked, when its password expires, which host it may connect from.
3. **"Take it away."** Lock or disable it now; drop it later, knowing what that breaks (the
   views and routines it is the definer of, the databases where it has a user, the schemas it
   owns).

Other tools answer them in a similar shape. The [comparison](comparison.md) found user
management in DataGrip on most engines (J30), in TablePlus on PostgreSQL, MySQL and MariaDB
(T20) and in DBX on MySQL, PostgreSQL, Oracle and more (X14); their dialogs were not read for
this plan. The engines' own tools set the shape people know: SQL Server Management Studio's
login dialog has pages for the login, its server roles, its **user mapping** (a database user
per database, with its database roles) and its permissions, and MySQL Workbench's **Users and
Privileges** has tabs for the login, its limits, its administrative roles and its schema
privileges. DbRover's version follows that shape (a list, a dialog per account with sections,
the privileges by level) and keeps to its own rules: every statement is shown before it runs,
the safety levels apply, and the password is never kept and, where the engine allows, never
sent as typed.

## What the user sees

### MySQL and MariaDB

A MySQL or MariaDB data source starts with two folders, before its databases, because accounts
belong to the whole server, as PostgreSQL's **roles** folder does:

```text
users
  app@%                 locked, member of app_read
  report@10.0.%         expires in 12 days
  root@localhost        all privileges
roles
  app_read              granted to 2 accounts
  app_write
shop
  tables
  …
```

- **An account is `name@host`**, listed under its whole name: `app@%` and `app@localhost` are
  two accounts with their own passwords and privileges, which is the first surprise MySQL holds
  for a PostgreSQL user, and the list shows it as it is. The host is written as the server
  stores it (`%`, `localhost`, `10.0.%`, `10.0.0.0/255.255.0.0`).
- **Roles** are in their own folder. MariaDB marks a role (`is_role`, and a role has no host).
  MySQL 8 doesn't: a role is an account created locked and without a password, and any account
  can be granted to another like a role. DbRover lists an account that is locked and has no
  password as a role, which is how `CREATE ROLE` makes one (question 2).
- **The detail** says what matters at a glance: locked, password expired or expiring, the roles
  it is a member of, and *all privileges* for an account with every global privilege.
- **System accounts** (`mysql.sys`, `mysql.session`, `mysql.infoschema`, `mariadb.sys`, and the
  providers' own, such as RDS's `rdsadmin`) are [system objects](../book/src/explorer.md#system-objects),
  hidden until the eye button shows them.

| Right-click on | Menu items |
| --- | --- |
| The data source, or its **users** or **roles** folder | **New › User…**, **New › Role…** |
| An account | **Copy name**, **Copy DDL**, **Edit…**, **Rename…**, **Lock** or **Unlock**, **Expire password**, **Drop…** |
| A role | **Copy name**, **Copy DDL**, **Edit…**, **Rename…**, **Drop…** |

**Lock**, **Unlock** and **Expire password** open the review panel with their one statement
(`ALTER USER … ACCOUNT LOCK`), so taking access away is two clicks, as asked first in
[the question](#the-question).

### The account dialog (MySQL and MariaDB)

**New › User…** and **Edit…** open one dialog with sections, as PostgreSQL's does, longer
because an account holds more:

```text
+--------------------------------------------------------------------------------------+
| Edit user app@%                                                                       |
| General | Password | Limits | Roles | Privileges                                      |
+--------------------------------------------------------------------------------------+
| Name  [ app        ]   Host [ %            ]  (% for any host, or a name, IP or mask)  |
| Authentication  [ caching_sha2_password ▾ ]                                          |
| New password    [ •••••••• ]   Confirm [ •••••••• ]   [ ] Remove password             |
| [x] Locked                                                                            |
| Password expires  ( ) as the server's default  ( ) never  (•) every [ 90 ] days       |
|                   [ ] Expire it now, so the next login must change it                 |
+--------------------------------------------------------------------------------------+
| ALTER USER `app`@`%` ACCOUNT LOCK PASSWORD EXPIRE INTERVAL 90 DAY                      |
| REVOKE `app_write`@`%` FROM `app`@`%`                                                  |
| GRANT SELECT, INSERT ON `shop`.* TO `app`@`%`                                          |
|                                             [Copy] [Open in console] [Execute]         |
+--------------------------------------------------------------------------------------+
```

- **General:** name and host; the authentication plugin, from those the server has active
  (see [Passwords and plugins](#passwords-and-plugins)); the password, typed twice; locked.
- **Password:** expiry (the server's default, never, or every *n* days), **Expire it now**;
  on MySQL, **Lock after** *n* failed logins for *d* days (`FAILED_LOGIN_ATTEMPTS`,
  `PASSWORD_LOCK_TIME`), and password history and reuse where the server's defaults are
  overridden. Each field says when it is the server's default.
- **Limits:** connections at once (`MAX_USER_CONNECTIONS`), and connections, queries and
  updates per hour; on MariaDB, a statement time limit (`MAX_STATEMENT_TIME`).
- **Roles:** the roles it is granted, with **Admin option** each, and which of them are its
  **default roles**, active when it logs in (`SET DEFAULT ROLE`; MySQL's
  `activate_all_roles_on_login` and `mandatory_roles` are said when set).
- **Privileges:** see [Privileges](#privileges).

**New › Role…** opens the same dialog without the Password and Limits sections; a MySQL role
has a host too, `%` unless typed. On MariaDB, `CREATE ROLE` grants the new role to the account
that creates it, with admin option, which the dialog says and the statement shows.

### SQL Server

SQL Server has two levels, which the explorer keeps apart:

```text
logins
  app                    SQL login, maps to a user in shop, sales
  CORP\analysts          Windows group
  ops@contoso.com        Microsoft Entra
  sa                     SQL login, sysadmin, disabled
server roles
  sysadmin
  ##MS_ServerStateReader##
  monitoring             1 member
shop
  schemas
  users
    app                  for login app, db_datareader
    legacy_job           no login (orphaned)
    reports_svc          without login
  roles
    db_datareader
    reporting            3 members
```

- **Logins** and **server roles** belong to the server, so they sit at the top of the data
  source, before the databases. Each login says its kind (SQL, Windows, Windows group, Microsoft
  Entra), whether it is disabled or locked, its server roles, and in which databases it has a
  user.
- **Users** and **roles** belong to a database, so each database gains two folders beside its
  schemas. A user says the login it is for, its database roles, or that it is **without login**,
  **contained** (it has a password of its own), or **orphaned**: made for a login that no
  longer exists, as `DROP LOGIN` leaves it.
- **Fixed** server and database roles (`sysadmin`, `db_owner`, `##MS_…##`) are listed, since
  granting them is how most logins get their privileges, and can't be edited, renamed or
  dropped; `public`, the `##…##` certificate logins, `dbo`, `guest`, `sys` and
  `INFORMATION_SCHEMA` are system objects.

| Right-click on | Menu items |
| --- | --- |
| The data source, or its **logins** folder | **New › Login…** |
| The **server roles** folder | **New › Server role…** |
| A login | **Copy name**, **Copy DDL**, **Edit…**, **Rename…**, **Disable** or **Enable**, **Drop…** |
| A server role | **Edit…** (members), and for one of your own **Rename…**, **Drop…** |
| A database's **users** folder | **New › User…** |
| A database's **roles** folder | **New › Role…** |
| A database user | **Copy name**, **Copy DDL**, **Edit…**, **Rename…**, **Map to login…** (orphaned users), **Drop…** |
| A database role | **Edit…** (members), and for one of your own **Rename…**, **Drop…** |

### The login and user dialogs (SQL Server)

- **New › Login…:** the kind first. **SQL login**: name, password typed twice, **Enforce
  password policy** (`CHECK_POLICY`, on by default as the server's is), **Enforce expiration**
  (`CHECK_EXPIRATION`), **Must change at next login** (`MUST_CHANGE`), default database and
  language. **Windows login or group**: `DOMAIN\name`, no password (`FROM WINDOWS`). **Microsoft
  Entra**: a user or group name (`FROM EXTERNAL PROVIDER`), where the server takes it. Then the
  **server roles** it is a member of, and **Map to databases**: a user for it in each database
  ticked, with its database roles and default schema, as SSMS's user mapping does.
- **Edit login…:** the same fields; the password changed only if typed; **Enabled**;
  **Unlock**, which SQL Server takes only with a new password (`WITH PASSWORD = … UNLOCK`); the
  state the server reports (`LOGINPROPERTY`: locked, expired, must change, bad password count,
  days until the password expires).
- **New › User…** in a database: **for a login** (picked from the logins), **without login**
  (for signing modules and impersonation), **with a password** (only in a contained database),
  or **Windows** / **Microsoft Entra**; its default schema; its database roles.
- **Edit user…:** default schema, database roles, and for an orphaned user **Map to login…**
  (`ALTER USER … WITH LOGIN = …`).
- **Server role** and **database role** dialogs: name, owner, members.

## The shared model

Each engine has its own kind of user, and no one model fits all three without hiding what the
user needs to know. W18 keeps them apart in the model and shares what the dialogs and the
statements have in common.

| | PostgreSQL (E3) | MySQL | MariaDB | SQL Server |
| --- | --- | --- | --- | --- |
| What logs in | a role with `LOGIN` | an account, `'name'@'host'` | an account, `'name'@'host'` | a login (server); it reaches a database through a user there, or a contained user logs in to its database directly |
| Groups | a role without `LOGIN` | a role (8.0 and later): an account, locked, without password | a role (`is_role`), no host | server roles; database roles |
| Belongs to | the server | the server | the server | logins and server roles: the server; users and roles: a database |
| Privileges | `GRANT` on objects; attributes | global (static and dynamic), database, table, column, routine, proxy | global, database, table, column, routine, proxy | server permissions, database permissions, object permissions; `DENY` as well as `GRANT` |
| Kinds of login | password | a password plugin, or socket, LDAP, PAM, Kerberos, FIDO… | a password plugin, or `unix_socket`, PAM, GSSAPI, `ed25519`… | SQL (password), Windows, Windows group, Microsoft Entra, certificate, asymmetric key |

- **`Capabilities::users`** (`UserModel`) gains two variants beside `Roles`: **`Accounts`** for
  MySQL and MariaDB, whose one dialect serves both, and **`LoginsAndUsers`** for SQL Server and
  Azure SQL. The menus read it, as they read `Roles` today. Synapse, Fabric and the other
  dialects stay `Unsupported`.
- **What the server offers is read from the server,** not the dialect: MySQL or MariaDB and
  its version, the authentication plugins it has active, whether `validate_password` or
  MariaDB's password checks are loaded, whether failed-login locking exists, whether the
  database is contained, and what the login may do (below). One read when a folder first loads,
  `Connection::user_management()`, answering a `UserManagement`.
- **Descriptions** are per kind, in `dbrover-core`: `AccountDescription` (MySQL and MariaDB,
  roles included), `LoginDescription`, `ServerRoleDescription`, `DatabaseUserDescription` and
  `DatabaseRoleDescription` (SQL Server), beside PostgreSQL's `RoleDescription`, gathered in a
  `PrincipalDescription` enum that one `Connection::describe_principal(&CatalogPath)` answers.
  PostgreSQL's `describe_role` becomes its `Role` variant.
- **Specs** are per kind, in `dbrover-sql::ddl`: `AccountSpec`, `LoginSpec`, `DatabaseUserSpec`,
  `RoleMembers`, beside `RoleSpec`, each with operations (`CreateAccount`, `AlterAccount`,
  `RenameAccount`, `DropAccount`, `CreateLogin`, …) written by pure functions per dialect and
  golden tested, as E3's are. Where MySQL and MariaDB spell a statement differently (`SET DEFAULT
  ROLE … TO` on MySQL, `… FOR` on MariaDB; roles with or without a host), the builder takes the
  flavour from `UserManagement`.
- **Shared** with E3, unchanged or generalised: the review panel and its Execute, Open in
  console and Copy; `ObjectForm`; the password fields, with the hashing per engine; the
  membership picker; the Drop dialog's layout, which counts what the drop breaks before
  Execute; `Command::ApplyDdl`; the history's redaction.
- **Not shared:** PostgreSQL's dialog and statements don't change, and PostgreSQL gains no
  privileges editor in W18 (question 4).

## Privileges

Privileges are what makes an account useful, and where the engines differ most.

### MySQL and MariaDB

| Level | Shown | Edited in W18 | Read from |
| --- | --- | --- | --- |
| Global, static (`SELECT`, `CREATE USER`, `SUPER`, …) | ticks, grouped as data, structure and administration | yes | `mysql.user` columns, or `information_schema.USER_PRIVILEGES` |
| Global, dynamic (MySQL 8: `BACKUP_ADMIN`, `SYSTEM_USER`, …) | ticks under administration | yes | `mysql.global_grants`; the list of what exists from `SHOW PRIVILEGES` |
| Database (`shop.*`, wildcards such as `shop\_%`) | one row per database: the database and its ticks | yes: add a row, change its ticks, remove it | `mysql.db` |
| Table, column, routine | one row each, with **Revoke** | revoked only; granted from a console | `mysql.tables_priv`, `columns_priv`, `procs_priv` |
| Proxy (`GRANT PROXY ON …`) and MySQL's partial revokes | listed | no | `mysql.proxies_priv`, `User_attributes` |

- **`WITH GRANT OPTION`** is a tick on the global and each database row.
- **What the statements look like:** `GRANT SELECT, INSERT ON \`shop\`.* TO \`app\`@\`%\`` for what
  is added and `REVOKE … FROM` for what is taken, one statement per level changed, ordered so
  that revokes come before grants.
- **Privileges that give administration** (`ALL` or `SUPER` on `*.*`, `GRANT OPTION`, `CREATE
  USER`, `SYSTEM_USER`, `SET_ANY_DEFINER`, `FILE`) are said in a warning line above Execute,
  which turns into the danger button (question 8).

### SQL Server

| What | Shown | Edited in W18 |
| --- | --- | --- |
| A login's server roles | ticks, fixed roles first | yes (`ALTER SERVER ROLE … ADD MEMBER`) |
| Server permissions (`CONNECT SQL`, `VIEW SERVER STATE`, `ALTER ANY LOGIN`, …) | one row each: granted, granted with grant option or denied | revoked only |
| A user's database roles | ticks, fixed roles first | yes (`ALTER ROLE … ADD MEMBER`) |
| Database, schema and object permissions | one row each, with the securable and the state | revoked only |
| A role's members | a list | yes |

- **`DENY`** is shown as such, since it wins over every `GRANT`, including one through a role.
- **Fixed roles cover most needs**: `db_datareader`, `db_datawriter`, `db_ddladmin` in a
  database, `##MS_ServerStateReader##` and the other `##MS_…##` roles of SQL Server 2022 on the
  server. Granting finer permissions (`GRANT SELECT ON SCHEMA::sales TO app`) stays in a console
  in W18; a grants editor on tables and schemas, for every engine with PostgreSQL first, is
  left for a following milestone (question 4).
- **Roles and permissions that give administration** (`sysadmin`, `securityadmin`, `CONTROL
  SERVER`, `db_owner`, `db_securityadmin`, `CONTROL` on a database) get the warning line and the
  danger button, as on MySQL.

## Passwords and plugins

### How the password is sent

E3 promised that a PostgreSQL password never leaves the computer as typed: DbRover hashes it
into a SCRAM verifier, which the server stores as it is. MySQL, MariaDB and SQL Server accept a
hash too, each in its own form, and each was tried on 2026-11-27 against the compose servers:

| Engine and plugin | The statement | Hash, computed by DbRover | Checked |
| --- | --- | --- | --- |
| MySQL, `caching_sha2_password` | `IDENTIFIED WITH caching_sha2_password AS '$A$005$<salt><hash>'` | SHA-256 crypt, 5,000 rounds, a 20-character salt of letters and digits | MySQL 8.4.13: a hash written this way by a script let the account log in, and the same script, given the salt of a hash the server made, reproduced it exactly |
| MySQL, `mysql_native_password` | `IDENTIFIED WITH mysql_native_password AS '*<hex>'` | `SHA1(SHA1(password))` in upper-case hex | *to check* on a server with the plugin active (off by default in 8.4, removed in 9.0) |
| MariaDB, `mysql_native_password` | `IDENTIFIED BY PASSWORD '*<hex>'` | as above | MariaDB 11.4.13: accepted, and matches what the server makes of the same password |
| MariaDB, `ed25519` | `IDENTIFIED VIA ed25519 USING '<public key>'` | the public key the plugin derives from the password | *to check*: the plugin isn't installed on the compose server |
| SQL Server, SQL login | `PASSWORD = 0x0200<salt><hash> HASHED` | `0x0200`, a 4-byte salt, then SHA-512 of the password in UTF-16LE followed by the salt | SQL Server 2022 CU28: the login logged in |
| SQL Server, contained user | `CREATE USER … WITH PASSWORD = N'…'` | none: there is no hashed form | |

But a hash hides the password from the server's own **password policy** too, which was also
tried:

- **MySQL with `validate_password`** refused `IDENTIFIED BY 'a'` and accepted a hash of `a`.
- **SQL Server with `CHECK_POLICY = ON`** refused `PASSWORD = 'a'` (*too short*) and accepted
  a `HASHED` `a`; and it refuses `MUST_CHANGE` with `HASHED` outright.

So W18 proposes (question 5):

- **Hashed on the client by default,** as on PostgreSQL, in the plugin's own form above.
- **Sent as typed when the server would check it:** when `validate_password` (MySQL) or
  `simple_password_check` / `cracklib_password_check` (MariaDB) is loaded, when **Enforce
  password policy** or **Must change at next login** is ticked on SQL Server, and for SQL
  Server's contained users, which have no hashed form. Then only over an encrypted connection
  (TLS, or DbRover's SSH tunnel); on an unencrypted one, Execute is disabled with the reason.
  The dialog says, under the password, which of the two it will do and why. MySQL rewrites the
  password out of its general log, slow log and binary log, and SQL Server hides the text of a
  `CREATE LOGIN` from its traces, so the server keeps no copy either way.
- **Never kept by DbRover,** either way: the review panel, the history and the log show
  `'<redacted>'` (W4b's `redact_secrets`), and Copy and Open in console are off while the SQL
  holds a password or a hash, as on PostgreSQL.

**Redaction needs two additions, golden tested:** SQL Server's hash is a binary literal
(`0x0200…`), not a string, which `redact_secrets` passes over today; and MySQL's grammar puts
the password before the options that follow it (`DEFAULT ROLE`, `PASSWORD EXPIRE`, `COMMENT`),
so W18 writes account names with backquotes, never `'name'@'host'` (which would read as
literals in the password's clause), and puts a `COMMENT` or `ATTRIBUTE`, if ever written, in a
statement of its own. `SET PASSWORD FOR 'app'@'%'`, whose account the redaction hides with the
password, is never written: W18 uses `ALTER USER`.

### Plugins

- **Offered:** on MySQL, `caching_sha2_password` (the default since 8.0), and
  `mysql_native_password` only where the server has it active; on MariaDB,
  `mysql_native_password` (its default) and `ed25519` where it is installed.
  `sha256_password`, deprecated, is shown on accounts that have it and not offered.
- **Shown and kept, not offered:** every other plugin (`auth_socket`, `unix_socket`, PAM, LDAP,
  Kerberos, GSSAPI, FIDO and WebAuthn), MySQL's multi-factor authentication, and MariaDB's
  `IDENTIFIED VIA a OR b`. Edit leaves them as they are unless another plugin is picked, which
  needs a new password (question 6).
- **Changing the plugin** writes `ALTER USER … IDENTIFIED WITH <plugin> AS '…'` with the new
  password's hash in that plugin's form.

### Lock and expire

| | MySQL | MariaDB | SQL Server |
| --- | --- | --- | --- |
| Lock | `ACCOUNT LOCK` / `UNLOCK` | the same (10.4 and later) | `ALTER LOGIN … DISABLE` / `ENABLE`; for a database user, `DENY CONNECT` |
| Expire now | `PASSWORD EXPIRE` | the same | `MUST_CHANGE`, with a new password |
| Expire every *n* days | `PASSWORD EXPIRE INTERVAL n DAY`, `NEVER`, `DEFAULT` | the same | `CHECK_EXPIRATION = ON`, with Windows's policy for the interval |
| Lock after failed logins | `FAILED_LOGIN_ATTEMPTS n PASSWORD_LOCK_TIME d` (8.0.19 and later) | server-wide only (`max_password_errors`), said | `CHECK_POLICY = ON`, with Windows's policy |
| Unlock after failed logins | `ACCOUNT UNLOCK` | `FLUSH PRIVILEGES` resets it, said | `WITH PASSWORD = … UNLOCK` |

## Safety: the preview, careful and read-only data sources

Everything here is DDL through the helpers, so it follows E3's rules and the safety levels (W2)
without anything new:

- **Always shown before it runs,** in the review panel, with **Execute**, **Open in console**
  and **Copy** (the last two off while a password is in it).
- **A careful data source** asks before Execute, naming it, as every helper does there.
- **A read-only data source** lists users, logins and roles and opens every dialog, but refuses
  Execute, with the reason; Copy and Open in console stay on where no password is in the SQL.
- **A red data source** asks for the name to be typed before a drop, as for a role.
- **Administration granted** (above) gets a warning line and the danger Execute everywhere.
- **The MCP login** of a data source is never used: the dialogs act as the user, as the
  explorer does.

**MySQL and MariaDB can't roll back.** `CREATE USER`, `GRANT` and the rest commit at once, so
an Edit that writes three statements and fails on the second leaves the first done (the
dialect is already `transactional_ddl: false`, so `ApplyDdl` runs them one at a time and says
which failed). Each account statement is atomic on MySQL 8, so none is left half-applied (*to
check* on MariaDB).
The builder orders them so the one most likely to be refused comes first (the account's own
change before its grants), and after a failure the dialog loads the account again and shows
what is now true. SQL Server runs them in one transaction, which rolls back logins and server
roles too (tried: a login, its user and a server role membership in one rolled-back
transaction left nothing).

## Dropping, and what it breaks

| Engine | What blocks or breaks the drop | What Drop… offers |
| --- | --- | --- |
| MySQL | Nothing blocks it for a login with `SET_ANY_DEFINER` (tried on 8.4: `DROP USER` of the definer of a view and of a procedure warned and went ahead), but those views, routines, triggers and events then fail when used | lists them by count and name (`information_schema.VIEWS.DEFINER`, `ROUTINES`, `TRIGGERS`, `EVENTS`), with the statements that give them another definer left to a console |
| MariaDB | as MySQL | as MySQL |
| SQL Server login | owning a database, a server role or an Agent job blocks it; its database users are left **orphaned** (tried: the user stayed, with its permissions, mapped to no login) | names the databases where it has a user and offers **Drop its users too**, one `DROP USER` per database before `DROP LOGIN` |
| SQL Server user | owning a schema or a role blocks it (tried: *The database principal owns a schema in the database, and cannot be dropped*) | names the schemas and roles it owns and offers **Give them to** another user (`ALTER AUTHORIZATION ON SCHEMA::… TO …`), as PostgreSQL's Drop offers `REASSIGN OWNED` |

## What a login without the privileges sees

DbRover often connects as an application's own account, which may read nothing about others.
What was seen on 2026-11-27:

- **MySQL and MariaDB:** without `SELECT` on `mysql.*` (or `CREATE USER`), `SELECT … FROM
  mysql.user` is refused, and `information_schema.USER_PRIVILEGES` shows the login's own
  privileges only.
- **SQL Server:** a login with no permissions sees itself, `sa` and the server roles in
  `sys.server_principals`, and nobody else; `sys.sql_logins` likewise.

So W18 proposes (question 9):

- **The folder lists what the login can see** (at least its own account or login) and a line at
  its top says what is hidden and which privilege shows the rest: `CREATE USER` or `SELECT` on
  `mysql.*`; `VIEW ANY DEFINITION` or `ALTER ANY LOGIN` on SQL Server.
- **New, Edit, Rename and Drop are disabled with the reason** when the login lacks what they
  need (`CREATE USER`, or `GRANT OPTION` for privileges, on MySQL and MariaDB; `ALTER ANY
  LOGIN`, `ALTER ANY USER` or `ALTER ANY ROLE` on SQL Server), read once with
  `user_management()`. A finer refusal the server makes (MySQL's `SYSTEM_USER` accounts, which
  only an account with `SYSTEM_USER` may change; a privilege the login doesn't hold itself and
  so can't grant) shows as the server's message in the dialog, which stays open.

## Recorded in the history

As E3: each **Execute** goes through `Command::ApplyDdl` and is recorded in the query history
with the origin **Object helper**, the statements as the review panel shows them, passwords and
hashes redacted. No new origin: a user's creation is a schema change like a table's, and
History's **Object helper** filter already finds it. The catalog reads behind the folders and
dialogs aren't recorded, as no explorer read is (question 10).

## MCP

No new tool (question 11). An assistant whose project allows **Admin** can already run `CREATE
USER`, `GRANT` and `CREATE LOGIN` through `execute`, which classifies them as administration
(`dbrover-sql`'s classify tests have them on MySQL and SQL Server). A tool that lists users and
their privileges would show an assistant who may do what on the server, which is worth asking
about before building, and isn't needed for any of the three tasks above.

## Engines

### In W18

| Engine | Versions | Read from | Tested against |
| --- | --- | --- | --- |
| MySQL | 8.0 and later (roles, dynamic privileges, `mysql.global_grants`); 5.7, past its end of life, gets no dialogs and says why | `mysql.user`, `global_grants`, `db`, `tables_priv`, `columns_priv`, `procs_priv`, `proxies_priv`, `role_edges`, `default_roles`; `information_schema` for a login without access | the compose `mysql` (8.4) |
| MariaDB | 10.4 and later (`mysql.global_priv`, `ACCOUNT LOCK`) | `mysql.global_priv` (JSON), `roles_mapping`, `db`, `tables_priv`, `columns_priv`, `procs_priv` | the compose `mariadb` (11.4) |
| SQL Server | 2016 and later | `sys.server_principals`, `sys.sql_logins`, `LOGINPROPERTY`, `sys.server_role_members`, `sys.server_permissions`; per database `sys.database_principals`, `sys.database_role_members`, `sys.database_permissions`, `sys.schemas` | the compose `sqlserver` (2022) |

These are the containers `docker-compose.yml` already starts for per-pull-request CI, so no
service is added. Each driver's tests log in as the account or login they create, with the
password DbRover hashed, before changing and dropping it.

### Managed services

The providers' MySQL and SQL Server services give the first account fewer privileges than
`root` or `sa`, and keep accounts of their own (question 12):

- **Amazon RDS and Aurora (MySQL, MariaDB), Cloud SQL for MySQL, Azure Database for MySQL:**
  the same dialogs. The first account can create users and grant what it holds, but not
  `SUPER` and not every dynamic privilege, so a grant of one is refused with the server's
  message. The providers' own accounts (`rdsadmin`, `cloudsqladmin`, `azure_superuser`, *to
  check* each) are system objects.
- **Amazon RDS for SQL Server, Cloud SQL for SQL Server:** the first login isn't `sysadmin`;
  creating logins and database users works, and the server roles it can't grant are refused by
  the server.
- **Azure SQL Database:** logins exist only in the logical server's `master`, and Microsoft
  recommends contained users (with a password, or Microsoft Entra) in each database. The
  **logins** and **server roles** folders show when the data source connects to `master`;
  otherwise each database's **users** folder offers contained users only (*to check*: which
  `##MS_…##` server roles Azure SQL Database grants, and whether it takes `HASHED`).
  **Azure SQL Managed Instance** is SQL Server, with Microsoft Entra logins.
- **Tested** by golden statements only: no CI account on any of these services. The manual
  says what each one's first account can't do.

### Left out

- **Amazon Redshift**, reached through the PostgreSQL driver, has users, groups and roles of its
  own; it belongs with C7 (#693), as it did for W15 and W16.
- **Azure Synapse and Microsoft Fabric:** Microsoft Entra only, managed in the portal.
- **ClickHouse** has SQL users, roles, grants, quotas, settings profiles and row policies: the
  closest to these three and the natural next engine, in a following milestone.
- **Snowflake** (users, roles, grants), **MongoDB** (`createUser`, roles), **Cassandra and
  ScyllaDB** (CQL roles), **Redis and Valkey** (`ACL SETUSER`), **Elasticsearch and OpenSearch**
  (their security APIs): each has users of its own, and each is a following milestone's.
- **BigQuery, Athena, Databricks, DynamoDB:** their access is the cloud's IAM or Unity Catalog,
  managed outside the database.
- **SQLite and DuckDB:** no users.

## How it's built

- **`dbrover-core`:** the descriptions per kind and `PrincipalDescription`; `AccountName { user,
  host }` with its display as `name@host`; `UserManagement` (flavour, version, plugins, password
  checks, failed-login locking, containment, what the login may do); new node and folder kinds
  (`NodeKind::Account`, `Login`, `DatabaseUser`, `ServerRole`, `DatabaseRole`;
  `FolderKind::Users`, `Logins`, `ServerRoles`, with PostgreSQL's `Roles` reused for MySQL's
  and a database's roles). `Connection::user_management()` and `describe_principal()`,
  defaulting to `Unsupported`.
- **`dbrover-sql`:** `UserModel::{Accounts, LoginsAndUsers}`; the specs and operations above;
  the hashes (`caching_sha2_password`'s SHA-256 crypt, `mysql_native_password`'s double SHA-1,
  SQL Server's salted SHA-512), each a type whose `Debug` hides it, as `ScramVerifier`'s does,
  with test vectors made by the compose servers; `Password::{Hashed, AsTyped}`; the builders,
  golden tested.
- **`dbrover-store`:** `redact_secrets` for SQL Server's `HASHED` binary literal, with tests.
- **The drivers** (MySQL for both flavours, SQL Server): the folders in their catalogs, the
  descriptions, `user_management`, and the definer and ownership counts for Drop.
- **`dbrover-engine`:** `Command::DescribePrincipal` and `LoadUserManagement`, answered on the
  explorer's connection, beside `DescribeRole`; `ApplyDdl` as it is.
- **`dbrover-app`:** the folders' menus by `UserModel`, and the dialogs, built from E3's parts.

## Options weighed

### One model for every engine, or one per kind

A single "principal" with optional host, optional database and a bag of flags would let one
dialog serve every engine, and would make every field mean something different per engine:
a host that is only MySQL's, a default schema that is only SQL Server's, a lock that is a
`DISABLE` here and an `ACCOUNT LOCK` there. Per-kind descriptions and specs keep each engine's
model exact and testable; what is shared is the dialog's parts and the statements' review,
which is where the duplication would be.

### A users folder for MySQL, or users and roles

MySQL's roles are accounts, so one folder would be the truest picture. But everyone who grants
roles thinks of them as roles, MariaDB marks them, and the dialogs differ (a role has no
password or limits). Two folders, with MySQL's guessed from how `CREATE ROLE` makes them;
question 2 asks whether the guess is acceptable.

### Hash always, or send as typed always

Hashing always keeps E3's promise and loses the server's password policy and `MUST_CHANGE`.
Sending as typed always keeps the policy, relies on TLS and on the server's log rewriting, and
breaks the promise on engines where it needn't. Hashing unless the server would check the
password keeps both where they matter, at the cost of a sentence under the password field
explaining which it does.

### A grants editor now, or roles and broad privileges first

A full grants editor (every object, every privilege, `GRANT` and `REVOKE` and `DENY`) is a
milestone of its own and would serve PostgreSQL too, which has none. W18 edits what is needed
to give a login what it needs: roles, and on MySQL global and database privileges. Finer grants
are listed and revocable, and granted from a console.

## Layers

1. **W18a — The plan:** this design, and the roadmap's entry.
2. **W18b — The model and the statements:** the core types, `UserModel`'s new variants, the specs
   and operations, the builders for MySQL, MariaDB and SQL Server, the three hashes with test
   vectors made by the compose servers, and the redaction of SQL Server's `HASHED` literal.
   Golden tests: every statement on both MySQL flavours and SQL Server; a name with a quote, a
   backquote and a bracket; `'name'@'host'` with `%`, `_` and an IP mask; revokes before grants;
   administration flagged.
3. **W18c — MySQL and MariaDB, read and round trip:** the **users** and **roles** folders,
   `describe_principal`, `user_management`, the privileges at every level, the definer counts;
   `Command::DescribePrincipal` and `LoadUserManagement`. A conformance check,
   `check_accounts`, on both servers: create an account with a hashed password and a role, log
   in as it and read, lock it and see the login refused, change its privileges, rename it, drop
   it; and what the `dbrover` login, which has no global privileges, sees.
4. **W18d — SQL Server, read and round trip:** the **logins** and **server roles** folders, each
   database's **users** and **roles**, the descriptions, `LOGINPROPERTY`, permissions, orphaned
   users. `check_logins`: a hashed SQL login logs in, gets a user in `dbrover` with
   `db_datareader` and reads, is disabled and refused, is dropped and leaves its user orphaned,
   which is mapped to a new login; a user owning a schema is refused a drop until the schema is
   given away; a login with no permissions sees only what it should.
5. **W18e — The MySQL and MariaDB dialogs:** the menus, New user, New role, Edit with its
   sections, Lock, Unlock, Expire password, Rename, Drop with its definers, Copy DDL (rebuilt
   from the description, never from `SHOW CREATE USER` or MariaDB's `SHOW GRANTS`, which hold the
   hash). Kittest tests against the MySQL test server: a user created with a role and a
   database's privileges, its password redacted in the panel and the history, a careful data
   source asking, a read-only one refusing, a login without `CREATE USER` seeing New disabled.
6. **W18f — The SQL Server dialogs:** New login of each kind, Edit login, Disable and Enable,
   Unlock, server roles and their members, New user of each kind, Edit user, Map to login, database
   roles and their members, Drop login with its users, Drop user with its schemas, Copy DDL.
   Kittest tests against the SQL Server test server, as W18e's.
7. **W18g — Manual and demo note:** [Users and roles](../book/src/users-roles.md) extended with
   MySQL and MariaDB, and SQL Server, with pictures from new scenes (`new-account`,
   `account-privileges`, `new-login`, `sqlserver-users`) on the test servers; the explorer's
   folders in [The explorer](../book/src/explorer.md) and the menus in
   [Data sources](../book/src/data-sources.md); the roadmap's demo note; the exit as an engine
   test, `w18_exit`.

Each layer compiles and passes on its own, with tests in the driver, engine and app test
binaries CI already runs against the compose servers; no job and no service is added. SQL
Server's image runs only under emulation on an Apple-silicon Mac, slowly, so its tests are the
ones CI's Linux runner is trusted for.

**Exit:** on the MySQL test server, a user `report@%` created with a password, `SELECT` on the
`dbrover` database and the role `app_read` as its default, logs in from a new data source and
reads; locked, its login is refused; dropped, it is gone; on MariaDB the same. On the SQL
Server test server, a SQL login with a user in `dbrover` that is a member of `db_datareader`
logs in and reads; disabled, it is refused; dropped with its users, nothing is left. On a
careful data source each Execute asks first; on a read-only one it is refused. No password or
hash appears in any history entry, log line or review panel.

## Decisions

The twelve open questions below were answered on 2026-11-27, all as proposed; the rest are
confirmed as their layers are built.

| Question | Proposed | Consequence |
| --- | --- | --- |
| The engines | **MySQL 8.0+, MariaDB 10.4+, SQL Server 2016+** and their managed services; Redshift with C7; ClickHouse, Snowflake and the NoSQL engines later | The servers CI already starts; the rest each a milestone of its own. |
| MySQL's roles | **A roles folder**, an account shown as a role when it is locked and has no password | Matches how people think of roles; a hand-made locked account without password shows as one. |
| SQL Server's levels | **Logins and server roles at the top; users and roles in each database** | The explorer shows where each one lives. |
| Privileges | **Roles everywhere; MySQL's global and database privileges edited; finer grants listed with Revoke** | The three tasks are covered; a grants editor is its own milestone. |
| Passwords | **Hashed on the client, unless the server would check it; then as typed, over TLS only** | E3's promise kept wherever it costs nothing; the server's policy kept where it is on. |
| Plugins | **The password plugins offered; the rest shown and kept** | No dialog half-supports socket, LDAP or Kerberos accounts. |
| Administration granted | **A warning line and the danger Execute** | `sysadmin` and `ALL ON *.*` never look like routine. |
| A login without privileges | **What it can see, a line saying what's hidden, actions disabled with the reason** | No dialog that the server will refuse anyway. |
| History | **As E3: Object helper, redacted** | Nothing new to learn in History. |
| MCP | **Nothing new** | Admin's `execute` already covers it. |
| MySQL's non-transactional DDL | **Riskiest statement first, the account reloaded after a failure** | A partial change is shown as it is. |

## Risks

- **Locking someone out, including oneself.** Dropping or locking the account DbRover is
  connected as, or revoking its own `CREATE USER`. The dialog says when the account is the
  data source's own login (`CURRENT_USER()`, `SUSER_SNAME()`), and Execute becomes the danger
  button with that said.
- **A hash the server reads differently.** A wrong hash is a login nobody can use. The hash
  functions are tested against vectors made by each server, and each driver's round trip logs
  in with the password after DbRover hashed it.
- **Partial changes on MySQL.** Covered above: ordered statements, the account reloaded, the
  failing statement named.
- **The host part.** `app@%` doesn't match a socket connection on `localhost` on some servers,
  and an account for `10.0.%` isn't found by a user who thinks of `app`. The dialog's Host field
  says what `%` covers, and the folder lists every `app@…` together.
- **Policy differences between flavours.** MySQL's and MariaDB's syntax diverged after 5.7
  (`SET DEFAULT ROLE … TO` and `… FOR`, `IDENTIFIED VIA`, `global_priv`); every statement is
  golden tested on both and every round trip runs on both servers.
- **SQL Server under emulation.** Slow and sometimes crashing on an Apple-silicon Mac; its
  checks poll for the state they wait for rather than sleeping, and run in CI's Linux runner.
- **Unverified service details,** marked *to check* above, are confirmed or corrected in the
  layer that needs them, and the manual says what wasn't tried.

## What stays out

- **A grants editor** on tables, columns, schemas and routines, for every engine, PostgreSQL
  included (question 4).
- **Editing the server's password policy** (`validate_password.*`, Windows's policy).
- **TLS requirements per account** (`REQUIRE SSL`, `REQUIRE X509`) beyond showing them.
- **Proxy users, certificate- and key-mapped logins, application roles:** listed, not created.
- **Copying an account to another server** with its password hash: the hash would have to be
  read and carried, which is what E3 decided against.
- **Auditing who logged in when:** server activity (W15) shows who is connected now.

## Open questions

Answered on 2026-11-27: all twelve as proposed. Kept as they were asked.

1. **Engines.** MySQL 8.0 and later, MariaDB 10.4 and later and SQL Server 2016 and later, with
   their managed services (RDS, Aurora, Cloud SQL, Azure Database for MySQL, Azure SQL Database
   and Managed Instance), all in W18; Redshift with C7; ClickHouse, Snowflake, MongoDB,
   Cassandra and Redis in following milestones. Should SQL Server wait for a W18 of its own,
   which would split this into two milestones of four layers?
2. **MySQL's roles.** List an account that is locked and has no password in a **roles** folder,
   as `CREATE ROLE` makes it, or list every MySQL account in **users** with *role* only in its
   detail?
3. **SQL Server's layout.** Logins and server roles at the top of the data source; users and
   roles in each database beside its schemas; Windows and Microsoft Entra logins created by name
   (not tried live, since CI has no domain); certificate- and key-mapped logins and application
   roles listed but not created?
4. **Privileges.** Edit roles on every engine and MySQL's global and database privileges; list
   table, column and routine privileges and SQL Server's permissions with **Revoke** only, and
   leave a grants editor (every engine, PostgreSQL first) for a following milestone? Or build
   the grants editor in W18, which adds about two layers?
5. **Passwords.** Hash on the client by default (MySQL's `caching_sha2_password` and
   `mysql_native_password`, MariaDB's, SQL Server's `HASHED`), and send the password as typed,
   over an encrypted connection only, when the server would check it (`validate_password`,
   MariaDB's password checks, SQL Server's **Enforce password policy** or **Must change**) or
   has no hashed form (contained users)? Or always hash, giving up the server's policy and
   `MUST_CHANGE`, or always send as typed over TLS?
6. **Plugins.** Offer only the password plugins (`caching_sha2_password`, and
   `mysql_native_password` where active; MariaDB's `mysql_native_password` and `ed25519`), and
   show the others (socket, PAM, LDAP, Kerberos, FIDO, multi-factor) without editing them?
7. **Lock and expire on the menu.** **Lock**/**Unlock** and **Expire password** on a MySQL or
   MariaDB account's menu, and **Disable**/**Enable** on a SQL Server login's, each opening the
   review panel with its one statement, besides the same fields in Edit?
8. **Administration granted.** A warning line and the danger Execute when a change grants
   `ALL`/`SUPER`/`GRANT OPTION`/`CREATE USER`/`SYSTEM_USER` on MySQL, or `sysadmin`,
   `securityadmin`, `CONTROL SERVER` or `db_owner` on SQL Server, on every data source; or ask
   on careful data sources only, as any change does?
9. **A login without the privileges.** List what it can see with a line saying what's hidden,
   and disable New, Edit, Rename and Drop with the reason, from one read of what the login may
   do; or offer them anyway and show the server's refusal?
10. **History.** Record each Execute as **Object helper**, as E3 does, with no new origin and the
    folders' reads unrecorded?
11. **MCP.** No new tool, since **Admin**'s `execute` already runs `CREATE USER` and `GRANT`; or a
    read-only `list_users` (accounts, roles, privileges) behind a new permission?
12. **Managed services.** Include them in W18 with the providers' own accounts as system objects
    and Azure SQL Database's logins only when connected to `master`, tested by golden statements
    only; or leave them out until someone asks?
