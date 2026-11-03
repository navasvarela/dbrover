# Google Cloud

## Cloud SQL

Choose **Cloud SQL for PostgreSQL**, **… MySQL** or **… SQL Server** in **New data source ›
Google Cloud** ([adding one](cloud-services.md#adding-one)).

- **Host** is the instance's IP address, from its overview page. With the
  [Cloud SQL Auth Proxy](https://cloud.google.com/sql/docs/postgres/sql-proxy) running on your
  machine, it is `localhost` and the proxy's port, and the proxy encrypts the connection itself.
  An instance with a DNS name (`*.sql.goog`) can be given by that name.
- **TLS** is `require`. Each instance's certificate is signed by the instance's own authority
  and doesn't name a host DbRover can check: for `verify-full`, download the instance's
  `server-ca.pem` from its **Connections › Security** page and choose it as the **CA file**,
  and give the host name the certificate names.
- **Sign in with** is a password, or on PostgreSQL and MySQL **Google Cloud** for Cloud SQL's
  IAM users, as described in [Cloud sign-in](cloud.md#google-cloud). Cloud SQL for SQL Server
  takes only passwords.

Cloud SQL behaves as its engines do; DbRover changes nothing else for it.

## Not tried against Google Cloud yet

Built and tested without a Google Cloud account. To try by hand: a Cloud SQL for PostgreSQL
and for MySQL instance with a password and with an IAM user, directly and through the Auth
Proxy; and a Cloud SQL for SQL Server instance.
