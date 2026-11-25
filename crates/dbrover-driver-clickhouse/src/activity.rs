//! The queries a ClickHouse server is running, from `system.processes`, and `KILL QUERY` (W15,
//! `docs/server-activity.md`).
//!
//! Over HTTP each query is a request of its own: there are no sessions between queries, no
//! transactions and no row locks. So every row is a running query, named by its `query_id`,
//! and the only stop is Cancel.

use std::time::Duration;

use dbrover_core::{
    DriverError, Row, ServerActivity, ServerSession, SessionId, SessionState, SessionStatement,
    SessionStop, StopKind, StopOutcome,
};

use crate::execution;

/// The node's running queries, each once: a distributed query's parts on this node are left
/// out, as is this read itself.
const QUERIES: &str = "\
SELECT query_id, user, toString(address), current_database, elapsed, read_rows,
       total_rows_approx, formatReadableSize(memory_usage), query, client_name, http_user_agent
FROM system.processes
WHERE is_initial_query AND query_id != queryID()
ORDER BY elapsed DESC";

/// A login needs `SELECT` on `system.processes` to read it at all, its own queries included
/// (ClickHouse 25.8). Without it the server refuses, naming the grant, and the tab shows that.
pub(crate) async fn read(client: &clickhouse::Client) -> Result<ServerActivity, DriverError> {
    let rows = execution::rows(client, QUERIES, &[]).await?;
    Ok(ServerActivity {
        sessions: rows.iter().map(query).collect(),
        restrictions: Vec::new(),
        // Whether the login may stop others' queries (`KILL QUERY`) can't be asked without
        // reading `system.grants`, which needs a grant of its own: Cancel is offered on every
        // query, and the server refuses one the login may not stop.
        others_stopped_by: None,
    })
}

fn query(row: &Row) -> ServerSession {
    let text = |index: usize| {
        row.get(index)
            .cloned()
            .flatten()
            .filter(|value| !value.is_empty())
    };
    let id = text(0).unwrap_or_default();
    let mut extra = Vec::new();
    if let Some(read) = text(5) {
        let of = text(6).filter(|total| total != "0");
        let rows = match of {
            Some(total) => format!("{read} of about {total}"),
            None => read,
        };
        extra.push(("Rows read".to_owned(), rows));
    }
    if let Some(memory) = text(7) {
        extra.push(("Memory".to_owned(), memory));
    }
    ServerSession {
        stops: vec![SessionStop {
            kind: StopKind::CancelQuery,
            statement: format!("KILL QUERY WHERE query_id = {} ASYNC", literal(&id)),
        }],
        id: SessionId::Query(id),
        user: text(1),
        client: text(2),
        database: text(3),
        state: SessionState::Active,
        engine_state: None,
        in_state_for: text(4)
            .and_then(|seconds| seconds.parse::<f64>().ok())
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map(Duration::from_secs_f64),
        transaction_for: None,
        statement: text(8).map(|text| SessionStatement {
            text,
            running: true,
            cut_short: false,
        }),
        application: text(9).or_else(|| text(10)),
        wait: None,
        lock: None,
        blocked_by: Vec::new(),
        extra,
    }
}

/// `KILL QUERY` answers one row per query it found; none means the query had already ended.
pub(crate) async fn stop(
    client: &clickhouse::Client,
    stop: &SessionStop,
) -> Result<StopOutcome, DriverError> {
    let found = execution::rows(client, &stop.statement, &[]).await?;
    Ok(if found.is_empty() {
        StopOutcome::AlreadyEnded
    } else {
        StopOutcome::Stopped
    })
}

/// A string literal: a query id is any text a client sent, so it is escaped as ClickHouse
/// reads strings, backslash and quote.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(values: &[Option<&str>]) -> Row {
        values
            .iter()
            .map(|value| value.map(str::to_owned))
            .collect()
    }

    #[test]
    fn reads_a_running_query_with_its_progress() {
        let session = query(&row(&[
            Some("4f1c-9a"),
            Some("etl"),
            Some("::ffff:10.0.3.7"),
            Some("analytics"),
            Some("12.5"),
            Some("1200000"),
            Some("50000000"),
            Some("1.21 GiB"),
            Some("SELECT count() FROM events"),
            Some(""),
            Some("airflow/2.9"),
        ]));

        assert_eq!(session.id, SessionId::Query("4f1c-9a".to_owned()));
        assert_eq!(session.state, SessionState::Active);
        assert_eq!(session.in_state_for, Some(Duration::from_millis(12_500)));
        assert_eq!(session.application.as_deref(), Some("airflow/2.9"));
        assert_eq!(
            session.extra,
            [
                (
                    "Rows read".to_owned(),
                    "1200000 of about 50000000".to_owned()
                ),
                ("Memory".to_owned(), "1.21 GiB".to_owned()),
            ]
        );
        assert_eq!(
            session
                .stop(StopKind::CancelQuery)
                .map(|stop| stop.statement.as_str()),
            Some("KILL QUERY WHERE query_id = '4f1c-9a' ASYNC")
        );
        assert!(session.stop(StopKind::EndSession).is_none());
    }

    #[test]
    fn escapes_a_query_id_a_client_chose() {
        assert_eq!(literal(r"it's \ mine"), r"'it\'s \\ mine'");
    }
}
