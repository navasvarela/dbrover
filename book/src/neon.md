# Neon

Choose **Neon** in **New data source › Other cloud services** ([adding
one](cloud-services.md#adding-one)), and copy the host, database and user from the project's
**Connect** dialog. **TLS** is `verify-full`, since Neon's certificates come from a public
authority; the database starts as `neondb`.

Neon finds your project's endpoint from the host name DbRover sends when it starts TLS, so
nothing else is needed for it.

## Pooled or direct

A host with `-pooler` in its first part (`ep-cool-name-123456-pooler.…`) goes through Neon's
connection pooler, in transaction mode; without it, straight to the database.
DbRover works through either, but [behind the pooler](cloud-services.md#behind-a-pooler) a
session's own state lasts only for its transaction: for `SET`, temporary tables or `LISTEN`,
use the host without `-pooler`.

A compute that has scaled to zero starts on the first connection, which can take a few
seconds.

## Not tried against Neon yet

The pooler was tried with PgBouncer in transaction mode standing in for it, not with Neon. To
try by hand: the pooled and the direct host, browsing, a console's statements and their
checks, and a read-only data source.
