# ClickHouse Cloud

Choose **ClickHouse Cloud** in **New data source › Other cloud services** ([adding
one](cloud-services.md#adding-one)), and copy the host and password from the service's
**Connect** dialog (choose **HTTPS** there). The host ends in `.clickhouse.cloud`, the port is
8443, the user and database are `default`, and **TLS** is `verify-full`: ClickHouse Cloud
answers only over HTTPS, with certificates from a public authority.

A service that has gone idle starts again on the first connection, which can take many
seconds. **Test connection**, like every connection, waits up to 15 seconds: if a service
takes longer to wake, try again once it has.

ClickHouse Cloud behaves as ClickHouse does: the explorer, consoles, the
[progress line](cloud.md#seeing-a-statement-run) and cancelling all work the same.

## Not tried against ClickHouse Cloud yet

HTTPS with `verify-full` was tried against a local ClickHouse with a certificate of its own,
not against ClickHouse Cloud. To try by hand: a service connected, browsed and queried, one
woken from idle, and a long query's progress and cancel.
