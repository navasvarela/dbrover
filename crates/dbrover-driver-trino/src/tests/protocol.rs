//! The protocol against a coordinator on this machine that answers from a list, each answer
//! shaped as `trinodb/trino:484` answered: every pull request tries the headers, `nextUri`,
//! retries and cancel without Trino's container.

use dbrover_core::{
    Connection, DriverError, JobProgress, ResultCursor, Route, TrinoProtocol, describe_error,
};

use super::config;
use super::stand_in::{Answer, Coordinator};
use crate::{TrinoConnection, connect};

const INFO: &str = r#"{"nodeVersion":{"version":"484"},"environment":"docker","coordinator":true,"starting":false,"uptime":"5.00m"}"#;

/// The answers to connecting: `GET /v1/info`, then `SELECT 1` queued and answered.
fn connecting() -> Vec<Answer> {
    vec![
        Answer::json(INFO),
        Answer::json(
            r#"{"id":"q0","nextUri":"http://coordinator.internal:8080/v1/statement/queued/q0/y0/1","stats":{"state":"QUEUED","processedRows":0,"processedBytes":0}}"#,
        ),
        Answer::json(
            r#"{"id":"q0","columns":[{"name":"_col0","type":"integer"}],"data":[[1]],"stats":{"state":"FINISHED","processedRows":0,"processedBytes":0}}"#,
        ),
    ]
}

async fn connected(coordinator: &Coordinator, protocol: TrinoProtocol) -> TrinoConnection {
    let mut config = config("127.0.0.1", coordinator.port(), "ada", "tpch", "tiny");
    config.protocol = protocol;
    connect(&config, None, Route::Direct)
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)))
}

async fn read_all(cursor: &mut Box<dyn ResultCursor>) -> Vec<Vec<Option<String>>> {
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(500).await.unwrap());
    }
    rows
}

#[tokio::test]
async fn follows_next_uris_here_and_asks_a_busy_coordinator_again() {
    let answers = connecting()
        .into_iter()
        .chain([
            Answer::status(503),
            Answer::json(
                r#"{"id":"q1","nextUri":"http://coordinator.internal:8080/v1/statement/queued/q1/y1/1","stats":{"state":"QUEUED"}}"#,
            ),
            Answer::json(
                r#"{"id":"q1","nextUri":"http://coordinator.internal:8080/v1/statement/executing/q1/y2/2","columns":[{"name":"name","type":"varchar(25)"}],"data":[["ALGERIA"],["ARGENTINA"]],"stats":{"state":"RUNNING","processedRows":2,"processedBytes":40}}"#,
            ),
            Answer::json(
                r#"{"id":"q1","columns":[{"name":"name","type":"varchar(25)"}],"data":[["BRAZIL"]],"stats":{"state":"FINISHED","processedRows":25,"processedBytes":102}}"#,
            ),
        ])
        .collect();
    let coordinator = Coordinator::start(answers).await;
    let mut connection = connected(&coordinator, TrinoProtocol::Trino).await;
    assert_eq!(connection.server_info().to_string(), "Trino 484");

    let mut cursor = connection.execute("SELECT name FROM nation").await.unwrap();
    assert_eq!(
        cursor.columns()[0].type_name.as_deref(),
        Some("varchar(25)")
    );
    let names: Vec<Option<String>> = read_all(&mut cursor)
        .await
        .into_iter()
        .map(|mut row| row.remove(0))
        .collect();
    assert_eq!(
        names,
        [
            Some("ALGERIA".to_owned()),
            Some("ARGENTINA".to_owned()),
            Some("BRAZIL".to_owned())
        ]
    );
    assert_eq!(
        cursor.progress(),
        Some(JobProgress {
            rows_read: Some(25),
            bytes_read: Some(dbrover_core::ByteCount(102)),
        })
    );
    cursor.finish().await.unwrap();

    let received = coordinator.received();
    let paths: Vec<(&str, &str)> = received
        .iter()
        .map(|request| (request.method.as_str(), request.path.as_str()))
        .collect();
    assert_eq!(
        paths,
        [
            ("GET", "/v1/info"),
            ("POST", "/v1/statement"),
            ("GET", "/v1/statement/queued/q0/y0/1"),
            ("POST", "/v1/statement"),
            ("POST", "/v1/statement"),
            ("GET", "/v1/statement/queued/q1/y1/1"),
            ("GET", "/v1/statement/executing/q1/y2/2"),
        ]
    );
    let statement = &received[4];
    assert_eq!(statement.body, "SELECT name FROM nation");
    for (name, value) in [
        ("x-trino-user", "ada"),
        ("x-trino-source", "dbrover"),
        ("x-trino-catalog", "tpch"),
        ("x-trino-schema", "tiny"),
        ("x-trino-transaction-id", "NONE"),
    ] {
        assert_eq!(statement.header(name), [value], "{name}");
    }
    assert!(statement.header("authorization").is_empty());
}

#[tokio::test]
async fn presto_is_sent_its_own_headers() {
    let coordinator = Coordinator::start(connecting()).await;
    let connection = connected(&coordinator, TrinoProtocol::Presto).await;
    assert_eq!(connection.server_info().product, "Presto");
    let received = coordinator.received();
    assert_eq!(received[1].header("x-presto-user"), ["ada"]);
    assert_eq!(received[1].header("x-presto-catalog"), ["tpch"]);
    assert!(received[1].header("x-trino-user").is_empty());
}

#[tokio::test]
async fn use_changes_what_the_next_statement_is_sent_with() {
    let answers = connecting()
        .into_iter()
        .chain([
            Answer::json(r#"{"id":"q1","updateType":"USE","stats":{"state":"FINISHED"}}"#)
                .with_header("X-Trino-Set-Catalog", "memory")
                .with_header("X-Trino-Set-Schema", "default"),
            Answer::json(
                r#"{"id":"q2","columns":[{"name":"_col0","type":"integer"}],"data":[[1]],"stats":{"state":"FINISHED"}}"#,
            ),
        ])
        .collect();
    let coordinator = Coordinator::start(answers).await;
    let mut connection = connected(&coordinator, TrinoProtocol::Trino).await;
    let cursor = connection.execute("USE memory.default").await.unwrap();
    assert!(cursor.columns().is_empty());
    cursor.finish().await.unwrap();
    let mut cursor = connection.execute("SELECT 1").await.unwrap();
    read_all(&mut cursor).await;
    let received = coordinator.received();
    let last = received.last().unwrap();
    assert_eq!(last.header("x-trino-catalog"), ["memory"]);
    assert_eq!(last.header("x-trino-schema"), ["default"]);
}

#[tokio::test]
async fn an_insert_is_its_count_and_an_error_its_place() {
    let answers = connecting()
        .into_iter()
        .chain([
            Answer::json(
                r#"{"id":"q1","nextUri":"http://coordinator.internal:8080/v1/statement/executing/q1/y1/1","columns":[{"name":"rows","type":"bigint"}],"updateType":"INSERT","stats":{"state":"RUNNING"}}"#,
            ),
            Answer::json(
                r#"{"id":"q1","columns":[{"name":"rows","type":"bigint"}],"data":[[3]],"updateType":"INSERT","updateCount":3,"stats":{"state":"FINISHED"}}"#,
            ),
            Answer::json(
                r#"{"id":"q2","stats":{"state":"FAILED"},"error":{"message":"line 2:1: mismatched input 'FORM'. Expecting: ',', 'FROM', <EOF>","errorCode":1,"errorName":"SYNTAX_ERROR","errorType":"USER_ERROR","errorLocation":{"lineNumber":2,"columnNumber":1}}}"#,
            ),
        ])
        .collect();
    let coordinator = Coordinator::start(answers).await;
    let mut connection = connected(&coordinator, TrinoProtocol::Trino).await;
    let cursor = connection
        .execute("INSERT INTO t VALUES (1), (2), (3)")
        .await
        .unwrap();
    assert!(cursor.columns().is_empty());
    assert_eq!(cursor.finish().await.unwrap(), Some(3));

    let sql = "SELECT *\nFORM t";
    let Err(error) = connection.execute(sql).await else {
        panic!("{sql} ran");
    };
    assert!(
        describe_error(&error).contains("SYNTAX_ERROR: line 2:1: mismatched input 'FORM'"),
        "{error:?}"
    );
    assert!(
        matches!(
            error,
            DriverError::Statement {
                position: Some(9),
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_result_left_unread_is_cancelled() {
    let answers = connecting()
        .into_iter()
        .chain([
            Answer::json(
                r#"{"id":"q1","nextUri":"http://coordinator.internal:8080/v1/statement/executing/q1/y1/1","columns":[{"name":"n","type":"bigint"}],"data":[[1]],"stats":{"state":"RUNNING"}}"#,
            ),
            Answer::status(204),
        ])
        .collect();
    let coordinator = Coordinator::start(answers).await;
    let mut connection = connected(&coordinator, TrinoProtocol::Trino).await;
    let mut cursor = connection.execute("SELECT * FROM huge").await.unwrap();
    assert_eq!(cursor.next_page(1).await.unwrap().len(), 1);
    assert!(!cursor.is_exhausted());
    cursor.finish().await.unwrap();
    let received = coordinator.received();
    let last = received.last().unwrap();
    assert_eq!(
        (last.method.as_str(), last.path.as_str()),
        ("DELETE", "/v1/statement/executing/q1/y1/1")
    );
}
