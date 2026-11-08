//! The driver against BigQuery's exchanges, replayed by a server on this machine (`replay`),
//! and against BigQuery only when the user asks for it by hand (`live_bigquery`).

mod replay;

use dbrover_core::{
    BigQueryConfig, ByteCount, Connection, DriverError, GoogleCloudSignIn, JobProgress,
    conformance, describe_error,
};

use self::replay::Replay;
use super::*;

/// A data source on the replay server, signed in as nobody, which reads none of the machine's
/// Google files.
fn config(replay: &Replay) -> BigQueryConfig {
    BigQueryConfig {
        sign_in: GoogleCloudSignIn::Anonymous,
        project: "analytics".to_owned(),
        endpoint: replay.endpoint.clone(),
        ..BigQueryConfig::default()
    }
}

async fn connected(config: &BigQueryConfig) -> BigQueryConnection {
    connect(config)
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)))
}

fn text(value: &str) -> Option<String> {
    Some(value.to_owned())
}

#[tokio::test]
async fn opens_pings_and_closes_listing_a_dataset() {
    let replay = Replay::start(include_str!("exchanges/connect.json")).await;
    let config = BigQueryConfig {
        location: "europe-west2".to_owned(),
        ..config(&replay)
    };
    let connection = connected(&config).await;
    assert_eq!(connection.server_info().product, "BigQuery");
    assert_eq!(
        connection.server_info().version,
        "project analytics in europe-west2"
    );
    conformance::check_lifecycle(Box::new(connection)).await;
    replay.finish();
}

#[tokio::test]
async fn requests_carry_the_google_sign_ins_token() {
    let endpoint = dbrover_cloud::stand_in::token_endpoint(
        200,
        r#"{"access_token": "ya29.stand-in", "expires_in": 3599, "token_type": "Bearer"}"#,
    )
    .await
    .unwrap();
    let folder = tempfile::tempdir().unwrap();
    let credentials_file = folder.path().join("adc.json");
    std::fs::write(
        &credentials_file,
        format!(
            r#"{{"type": "authorized_user", "client_id": "id", "client_secret": "secret",
                "refresh_token": "refresh", "token_uri": "{}"}}"#,
            endpoint.address
        ),
    )
    .unwrap();
    let replay = Replay::start(include_str!("exchanges/connect.json")).await;
    let config = BigQueryConfig {
        sign_in: GoogleCloudSignIn::CredentialsFile { credentials_file },
        ..config(&replay)
    };
    connected(&config).await;
    assert_eq!(
        replay.authorizations(),
        [Some("Bearer ya29.stand-in".to_owned())]
    );
    replay.finish();
}

#[tokio::test]
async fn a_select_runs_where_bigquery_chose_and_reads_page_by_page() {
    let replay = Replay::start(include_str!("exchanges/select.json")).await;
    let config = BigQueryConfig {
        dataset: "sales".to_owned(),
        ..config(&replay)
    };
    let mut connection = connected(&config).await;
    let mut cursor = connection
        .execute("SELECT id, name, placed, tags, address FROM orders ORDER BY id")
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let job = replay.job().expect("a job was inserted");
    assert!(job.starts_with("dbrover_"), "{job}");
    let columns: Vec<(&str, Option<&str>)> = cursor
        .columns()
        .iter()
        .map(|column| (column.name.as_str(), column.type_name.as_deref()))
        .collect();
    assert_eq!(
        columns,
        [
            ("id", Some("INT64")),
            ("name", Some("STRING")),
            ("placed", Some("TIMESTAMP")),
            ("tags", Some("ARRAY<STRING>")),
            ("address", Some("STRUCT<city STRING, floor INT64>")),
        ]
    );
    let first = cursor.next_page(2).await.unwrap();
    assert_eq!(
        first,
        [
            vec![
                text("1"),
                text("Ada"),
                text("2026-01-02 03:04:05.123456 UTC"),
                text(r#"["new","gift"]"#),
                text(r#"{"city":"Paris","floor":3}"#),
            ],
            vec![text("2"), None, None, text("[]"), None],
        ]
    );
    let rest = cursor.next_page(500).await.unwrap();
    let ids: Vec<Option<String>> = rest.iter().map(|row| row[0].clone()).collect();
    assert_eq!(ids, [text("3"), text("4"), text("5")]);
    assert_eq!(rest[0][1], text(""), "the empty string isn't NULL");
    assert!(cursor.is_exhausted());
    assert_eq!(
        cursor.progress(),
        Some(JobProgress {
            rows_read: None,
            bytes_read: Some(ByteCount(2_500_000)),
        })
    );
    assert_eq!(cursor.finish().await.unwrap(), None);
    replay.finish();
}

#[tokio::test]
async fn an_update_says_how_many_rows_it_changed() {
    let replay = Replay::start(include_str!("exchanges/update.json")).await;
    let config = BigQueryConfig {
        location: "EU".to_owned(),
        ..config(&replay)
    };
    let mut connection = connected(&config).await;
    let cursor = connection
        .execute("UPDATE sales.orders SET total = 0 WHERE id < 4")
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    assert!(cursor.columns().is_empty());
    assert_eq!(cursor.finish().await.unwrap(), Some(3));
    replay.finish();
}

#[tokio::test]
async fn a_failed_statement_points_at_its_mistake() {
    let replay = Replay::start(include_str!("exchanges/failure.json")).await;
    let mut connection = connected(&config(&replay)).await;
    for (sql, position, message) in [
        (
            "SELECT 1 x y",
            11,
            "Expected end of input but got identifier \"y\"",
        ),
        (
            "SELECT id,\n  totl\nFROM orders",
            13,
            "Unrecognized name: totl",
        ),
    ] {
        let error = match connection.execute(sql).await {
            Ok(_) => panic!("{sql} ran"),
            Err(error) => error,
        };
        assert!(
            matches!(&error, DriverError::Statement { position: Some(at), .. } if *at == position),
            "{error:?}"
        );
        assert!(describe_error(&error).contains(message), "{error:?}");
    }
    replay.finish();
}

#[tokio::test]
async fn cancel_stops_the_job_by_its_id_and_location() {
    let replay = Replay::start(include_str!("exchanges/cancel.json")).await;
    let mut connection = connected(&config(&replay)).await;
    conformance::check_cancel(&mut connection, "SELECT COUNT(*) FROM big.events").await;
    replay.finish();
}

#[tokio::test]
async fn connecting_says_what_is_missing() {
    let replay = Replay::start(include_str!("exchanges/connect-failures.json")).await;
    let elsewhere = BigQueryConfig {
        project: "someone-else".to_owned(),
        ..config(&replay)
    };
    let result = connect(&elsewhere).await;
    let message = result
        .as_ref()
        .err()
        .map(|error| describe_error(error))
        .unwrap_or_default();
    assert!(
        message.contains("does not have bigquery.datasets.list permission"),
        "{message}"
    );
    conformance::check_connect_failure(result);

    let Err(error) = connect(&BigQueryConfig {
        project: " ".to_owned(),
        ..config(&replay)
    })
    .await
    else {
        panic!("connected without a project");
    };
    assert!(
        describe_error(&error).contains("Set the data source's project"),
        "{error:?}"
    );
    replay.finish();
}

/// The driver's own checks against BigQuery, signed in with the machine's Application Default
/// Credentials (or the credentials file named after the project), over a public dataset, so
/// each query is billed BigQuery's 10 MiB minimum per table at most.
#[tokio::test]
#[ignore = "needs a Google Cloud project; set DBROVER_TEST_BIGQUERY_LIVE and run with --ignored"]
async fn live_bigquery() {
    let Ok(live) = std::env::var("DBROVER_TEST_BIGQUERY_LIVE") else {
        eprintln!("DBROVER_TEST_BIGQUERY_LIVE is unset; nothing to try");
        return;
    };
    let parts: Vec<&str> = live.split(',').map(str::trim).collect();
    let [project, rest @ ..] = parts.as_slice() else {
        panic!("DBROVER_TEST_BIGQUERY_LIVE is project[,credentials file]");
    };
    let config = BigQueryConfig {
        project: (*project).to_owned(),
        sign_in: rest.first().map_or(GoogleCloudSignIn::Google, |file| {
            GoogleCloudSignIn::CredentialsFile {
                credentials_file: (*file).into(),
            }
        }),
        ..BigQueryConfig::default()
    };
    let mut connection = connect(&config)
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    eprintln!("connected: {}", connection.server_info());

    let mut cursor = connection
        .execute(
            "SELECT 1 AS n, CAST(NULL AS STRING) AS no_value, '' AS empty_text, \
             TIMESTAMP '2026-01-02 03:04:05.123456 UTC' AS t, [1, 2] AS xs, \
             STRUCT('Paris' AS city) AS address",
        )
        .await
        .unwrap_or_else(|error| panic!("{}", describe_error(&error)));
    let rows = cursor.next_page(500).await.unwrap();
    eprintln!("columns: {:?}\nrows: {rows:?}", cursor.columns());
    assert_eq!(
        rows,
        [vec![
            text("1"),
            None,
            text(""),
            text("2026-01-02 03:04:05.123456 UTC"),
            text("[1,2]"),
            text(r#"{"city":"Paris"}"#),
        ]]
    );
    eprintln!("progress: {:?}", cursor.progress());
    cursor.finish().await.unwrap();

    let sql = "SELECT *\nFORM t";
    let error = match connection.execute(sql).await {
        Ok(_) => panic!("{sql} ran"),
        Err(error) => error,
    };
    eprintln!("failure: {error:?}");
    assert!(matches!(
        error,
        DriverError::Statement {
            position: Some(_),
            ..
        }
    ));

    // `COUNT(*)` reads no column, so BigQuery bills nothing for it, yet the cross join of
    // 300 million rows with themselves runs long enough to be cancelled.
    conformance::check_cancel(
        &mut connection,
        "SELECT COUNT(*) FROM `bigquery-public-data.samples.wikipedia` a \
         CROSS JOIN `bigquery-public-data.samples.wikipedia` b",
    )
    .await;
}
