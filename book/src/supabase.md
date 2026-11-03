# Supabase

Choose **Supabase** in **New data source › Other cloud services** ([adding
one](cloud-services.md#adding-one)), and copy the host and user from the project's **Connect**
dialog. The database is `postgres`.

## Which connection

Supabase offers three, all through the same dialog:

| Connection | Host | Port | User |
| --- | --- | --- | --- |
| Session pooler, the preset | `aws-0-<region>.pooler.supabase.com` | 5432 | `postgres.<project>` |
| Transaction pooler | the same | 6543 | `postgres.<project>` |
| Direct | `db.<project>.supabase.co` | 5432 | `postgres` |

The session pooler keeps one server connection for as long as DbRover's, as a desktop tool
wants, and works over IPv4. The transaction pooler works too, but
[a session's own state](cloud-services.md#behind-a-pooler) lasts only for its transaction. The
direct host needs IPv6 on your network, unless the project has the IPv4 add-on.

**TLS** is `require`: Supabase signs its certificates with its own authority. To check them,
download the certificate from the project's **Database settings** page, set **TLS** to
`verify-full` and choose it as the **CA file**.

## Not tried against Supabase yet

The transaction pooler was tried with PgBouncer standing in for Supabase's own pooler
(Supavisor), not with Supabase. To try by hand: each of the three connections, with `require`
and with `verify-full` and Supabase's certificate.
