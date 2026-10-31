use std::path::{Path, PathBuf};

use dbrover_core::conformance;
use dbrover_core::{
    ElasticsearchAuth, NodeKind, Port, ResultShape, Row, ServerConfig, TlsMode, describe_error,
};
use dbrover_document::Value;

use super::*;
use crate::hits::{Paging, hit_document};

/// Point the tests at the servers, e.g. `https://elastic:dbrover@localhost:59200` and
/// `http://localhost:59201` from the repository's `docker-compose.yml` (see
/// `test-servers.env`). Each test runs on every server whose variable is set, and skips when
/// none is, so `cargo test` works offline.
const TEST_URL_VARS: [&str; 2] = [
    "DBROVER_TEST_ELASTICSEARCH_URL",
    "DBROVER_TEST_OPENSEARCH_URL",
];

/// How many documents make a search sleep: each runs a script of a million steps, the most a
/// script may loop, so a search of them all takes tens of seconds on either server.
const SLOW_DOCUMENTS: usize = 60_000;

struct TestServer {
    name: &'static str,
    config: ElasticsearchConfig,
    password: Option<Password>,
}

impl TestServer {
    async fn connect(&self) -> SearchConnection {
        connect(&self.config, self.password.as_ref(), Route::Direct)
            .await
            .unwrap_or_else(|error| panic!("{}: connect: {}", self.name, describe_error(&error)))
    }

    async fn connect_with(&self, tls: TlsMode) -> Result<SearchConnection, DriverError> {
        let mut config = self.config.clone();
        config.server.tls = tls;
        connect(&config, self.password.as_ref(), Route::Direct).await
    }
}

/// The servers whose variable is set: an `http://` or `https://` URL with an optional user and
/// password. An HTTPS server's certificate is checked against the test authority.
fn test_servers() -> Vec<TestServer> {
    let servers: Vec<TestServer> = TEST_URL_VARS
        .into_iter()
        .filter_map(|name| Some((name, std::env::var(name).ok()?)))
        .map(|(name, url)| parse_url(name, &url))
        .collect();
    if servers.is_empty() {
        eprintln!("skipping: none of {TEST_URL_VARS:?} is set");
    }
    servers
}

fn parse_url(name: &'static str, url: &str) -> TestServer {
    let (tls, rest) = match url.split_once("://") {
        Some(("https", rest)) => (
            TlsMode::VerifyFull {
                ca_file: Some(test_ca_file()),
            },
            rest,
        ),
        Some(("http", rest)) => (TlsMode::Disable, rest),
        _ => panic!("{name}: an http:// or https:// URL"),
    };
    let rest = rest.trim_end_matches('/');
    let (credentials, address) = rest.rsplit_once('@').unwrap_or(("", rest));
    let (user, password) = credentials.split_once(':').unwrap_or((credentials, ""));
    let (host, port) = address.rsplit_once(':').expect("host:port in the URL");
    TestServer {
        name,
        config: ElasticsearchConfig {
            server: ServerConfig {
                host: host.to_owned(),
                port: Port::from(port.parse::<u16>().expect("a port")),
                database: String::new(),
                user: user.to_owned(),
                tls,
                ssh: None,
                password_command: None,
            },
            auth: ElasticsearchAuth::Password,
        },
        password: (!password.is_empty()).then(|| Password::new(password.to_owned())),
    }
}

/// The authority that signed the Elasticsearch test server's certificate.
fn test_ca_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/postgres-tls/ca.crt")
}

/// Runs `request` to its end and returns its shape and rows.
async fn run(connection: &mut SearchConnection, request: &str) -> (ResultShape, Vec<Row>) {
    let mut cursor = connection
        .execute(request)
        .await
        .unwrap_or_else(|error| panic!("{request}: {}", describe_error(&error)));
    let shape = cursor.shape();
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(500).await.expect("a page"));
    }
    cursor.finish().await.expect("finish");
    (shape, rows)
}

/// Each row's one document, read.
fn documents(rows: &[Row]) -> Vec<Value> {
    rows.iter()
        .map(|row| match row.as_slice() {
            [Some(text)] => Value::parse(text).expect("a document"),
            other => panic!("not a document: {other:?}"),
        })
        .collect()
}

fn field<'a>(document: &'a Value, name: &str) -> Option<&'a Value> {
    json::field(document, name)
}

/// `index` holding `count` documents numbered from 0 in `n`, made again from nothing.
async fn fill(connection: &mut SearchConnection, index: &str, count: usize) {
    let _ = connection.execute(&format!("DELETE /{index}")).await;
    let mut request = format!("POST /{index}/_bulk?refresh=true\n");
    for n in 0..count {
        request.push_str(&format!(
            "{{\"index\":{{\"_id\":\"{n}\"}}}}\n{{\"n\":{n},\"even\":{}}}\n",
            n % 2 == 0
        ));
    }
    let (_, rows) = run(connection, &request).await;
    let answer = &documents(&rows)[0];
    assert_eq!(
        field(answer, "errors"),
        Some(&Value::Bool(false)),
        "{index}"
    );
}

#[tokio::test]
async fn opens_pings_and_closes() {
    for server in test_servers() {
        let connection = server.connect().await;
        let product = connection.server_info().product.clone();
        assert!(
            product == "Elasticsearch" || product == "OpenSearch",
            "{}: {product}",
            server.name
        );
        conformance::check_lifecycle(Box::new(connection)).await;
    }
}

#[tokio::test]
async fn reports_which_server_it_reached() {
    for server in test_servers() {
        let connection = server.connect().await;
        let expected = match server.name {
            "DBROVER_TEST_OPENSEARCH_URL" => "OpenSearch",
            _ => "Elasticsearch",
        };
        assert_eq!(connection.server_info().product, expected);
    }
}

#[tokio::test]
async fn fails_to_connect_to_a_closed_port_or_with_a_wrong_password() {
    let mut closed = parse_url("closed", "http://localhost:1");
    closed.config.server.tls = TlsMode::Disable;
    conformance::check_connect_failure(connect(&closed.config, None, Route::Direct).await);
    for server in test_servers() {
        if server.password.is_none() {
            continue;
        }
        let wrong = Password::new("not the password".to_owned());
        conformance::check_connect_failure(
            connect(&server.config, Some(&wrong), Route::Direct).await,
        );
    }
}

#[tokio::test]
async fn lists_indices_and_rejects_paths_it_never_gave() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        fill(&mut connection, "dbrover_catalog", 3).await;
        let nodes = conformance::walk_catalog(&mut connection, 2).await;
        let index = nodes
            .iter()
            .find(|node| node.label == "dbrover_catalog")
            .unwrap_or_else(|| panic!("{}: no index in {nodes:?}", server.name));
        assert_eq!(index.kind(), Some(NodeKind::SearchIndex));
        assert_eq!(index.detail.as_deref(), Some("3 documents"));
        conformance::check_invalid_path(&mut connection).await;
    }
}

/// N3's exit: a search with a sort, paged past its first hundred hits, reads every hit once
/// and in order, as documents with `_index`, `_id` and `_score` first; its own `size` caps it.
#[tokio::test]
async fn reads_every_hit_of_a_search_a_page_at_a_time() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        fill(&mut connection, "dbrover_pages", 1_200).await;
        let (shape, rows) = run(
            &mut connection,
            "GET /dbrover_pages/_search\n{ \"sort\": [{ \"n\": \"desc\" }] }",
        )
        .await;
        assert_eq!(shape, ResultShape::Documents, "{}", server.name);
        let hits = documents(&rows);
        let numbers: Vec<String> = hits
            .iter()
            .map(|hit| match field(hit, "n") {
                Some(Value::Number(n)) => n.clone(),
                other => panic!("{}: n is {other:?}", server.name),
            })
            .collect();
        let expected: Vec<String> = (0..1_200).rev().map(|n| n.to_string()).collect();
        assert_eq!(numbers, expected, "{}", server.name);
        let Value::Object(first) = &hits[0] else {
            panic!("{}: {:?}", server.name, hits[0]);
        };
        let names: Vec<&str> = first.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            ["_index", "_id", "_score", "n", "even"],
            "{}",
            server.name
        );

        let (_, rows) = run(
            &mut connection,
            "POST /dbrover_pages/_search\n{ \"size\": 700, \"query\": { \"term\": { \"even\": true } } }",
        )
        .await;
        assert_eq!(rows.len(), 600, "{}: every even number", server.name);
        let (_, rows) = run(
            &mut connection,
            "GET /dbrover_pages/_search?size=7&q=even:false",
        )
        .await;
        assert_eq!(rows.len(), 7, "{}", server.name);
    }
}

/// A search for its aggregations, or with `from`, runs as typed, its answer one document.
#[tokio::test]
async fn shows_an_aggregation_as_one_document() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        fill(&mut connection, "dbrover_aggregations", 10).await;
        let (shape, rows) = run(
            &mut connection,
            "POST /dbrover_aggregations/_search\n{ \"size\": 0, \"aggs\": { \"total\": { \"sum\": { \"field\": \"n\" } } } }",
        )
        .await;
        assert_eq!(shape, ResultShape::Documents);
        let [answer] = documents(&rows).try_into().expect("one document");
        let total = json::at(&answer, &["aggregations", "total", "value"]);
        assert_eq!(
            total,
            Some(&Value::Number("45.0".to_owned())),
            "{}",
            server.name
        );

        let (_, rows) = run(
            &mut connection,
            "GET /dbrover_aggregations/_search\n{ \"from\": 2, \"size\": 3, \"sort\": [\"n\"] }",
        )
        .await;
        let [answer] = documents(&rows).try_into().expect("one document");
        let Some(Value::Array(hits)) = json::at(&answer, &["hits", "hits"]) else {
            panic!("{}: {answer:?}", server.name);
        };
        assert_eq!(hits.len(), 3, "{}", server.name);
    }
}

/// N3's exit: a SQL statement answers as rows with its columns' names and types, read past the
/// endpoint's first page.
#[tokio::test]
async fn shows_sql_answers_as_rows() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        fill(&mut connection, "dbrover_sql", 1_200).await;
        let endpoint = match connection.flavor {
            Flavor::Elasticsearch => "/_sql",
            Flavor::OpenSearch => "/_plugins/_sql",
        };
        let mut cursor = connection
            .execute(&format!(
                "POST {endpoint}\n{{ \"query\": \"SELECT n, even FROM dbrover_sql ORDER BY n\" }}"
            ))
            .await
            .unwrap_or_else(|error| panic!("{}: {}", server.name, describe_error(&error)));
        assert_eq!(cursor.shape(), ResultShape::Rows);
        let columns: Vec<(String, Option<String>)> = cursor
            .columns()
            .iter()
            .map(|column| (column.name.clone(), column.type_name.clone()))
            .collect();
        assert_eq!(
            columns,
            [
                ("n".to_owned(), Some("long".to_owned())),
                ("even".to_owned(), Some("boolean".to_owned()))
            ],
            "{}",
            server.name
        );
        let mut rows = Vec::new();
        while !cursor.is_exhausted() {
            rows.extend(cursor.next_page(500).await.expect("a page"));
        }
        cursor.finish().await.expect("finish");
        assert_eq!(rows.len(), 1_200, "{}", server.name);
        assert_eq!(
            rows[3],
            [Some("3".to_owned()), Some("false".to_owned())],
            "{}",
            server.name
        );
    }
}

/// A `_cat` answer comes as JSON, a document a line; text that isn't JSON is a row a line.
#[tokio::test]
async fn shows_cat_answers_as_documents() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        fill(&mut connection, "dbrover_cat", 1).await;
        let (shape, rows) = run(&mut connection, "GET /_cat/indices/dbrover_cat?h=index").await;
        assert_eq!(shape, ResultShape::Documents);
        assert_eq!(
            documents(&rows),
            [Value::Object(vec![(
                "index".to_owned(),
                Value::String("dbrover_cat".to_owned())
            )])],
            "{}",
            server.name
        );
        let (shape, rows) = run(
            &mut connection,
            "GET /_cat/indices/dbrover_cat?h=index&format=txt",
        )
        .await;
        assert_eq!(
            (shape, rows),
            (
                ResultShape::Rows,
                vec![vec![Some("dbrover_cat".to_owned())]]
            ),
            "{}",
            server.name
        );
    }
}

/// A refused request shows the server's type and reason, pointing into the body where the
/// server says; a request DbRover can't read points at what it can't.
#[tokio::test]
async fn errors_name_the_servers_reason_and_point_into_the_body() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        fill(&mut connection, "dbrover_errors", 1).await;
        let request = "POST /dbrover_errors/_count\n{\"query\": {\"bogus\": {}}}";
        let error = connection.execute(request).await.err();
        let Some(DriverError::Statement {
            source, position, ..
        }) = error
        else {
            panic!("{}: {error:?}", server.name);
        };
        assert!(
            source.to_string().starts_with("parsing_exception: "),
            "{}: {source}",
            server.name
        );
        let at = position.unwrap_or_else(|| panic!("{}: no position", server.name));
        assert!(
            request[at..].starts_with("{}}") || request[at..].starts_with("\"bogus"),
            "{}: points at {:?}",
            server.name,
            &request[at..]
        );

        let error = connection
            .execute("GET /no_such_index_here/_doc/1")
            .await
            .err();
        let Some(DriverError::Statement { source, .. }) = error else {
            panic!("{}: {error:?}", server.name);
        };
        assert!(
            source.to_string().contains("index_not_found_exception"),
            "{}: {source}",
            server.name
        );

        let error = connection.execute("POST /a/_search\n{ nope }").await.err();
        assert!(
            matches!(
                error,
                Some(DriverError::Statement {
                    position: Some(18),
                    ..
                })
            ),
            "{}: {error:?}",
            server.name
        );
    }
}

/// N3's exit: ⌘. stops a slow search, on both servers.
#[tokio::test]
async fn cancels_a_slow_search() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        fill(&mut connection, "dbrover_slow", SLOW_DOCUMENTS).await;
        conformance::check_cancel(
            &mut connection,
            "POST /dbrover_slow/_count\n{ \"query\": { \"script\": { \"script\": \"long x = 0; for (int i = 0; i < 999999; i++) { x += i } return x > 0\" } } }",
        )
        .await;
    }
}

#[tokio::test]
async fn connects_over_tls_checking_the_certificate() {
    for server in test_servers() {
        if !matches!(server.config.server.tls, TlsMode::VerifyFull { .. }) {
            continue;
        }
        server
            .connect_with(TlsMode::Require)
            .await
            .unwrap_or_else(|error| panic!("require: {}", describe_error(&error)));
        // The public authorities didn't sign the test certificate.
        conformance::check_connect_failure(
            server
                .connect_with(TlsMode::VerifyFull { ca_file: None })
                .await,
        );
        // Plain HTTP to a node that only speaks HTTPS.
        conformance::check_connect_failure(server.connect_with(TlsMode::Disable).await);
    }
}

/// An API key, made by the test user, signs in in the password's place.
#[tokio::test]
async fn signs_in_with_an_api_key() {
    for server in test_servers() {
        if server.password.is_none() {
            continue;
        }
        let mut connection = server.connect().await;
        let (_, rows) = run(
            &mut connection,
            "POST /_security/api_key\n{ \"name\": \"dbrover-test\", \"expiration\": \"1h\" }",
        )
        .await;
        let [answer] = documents(&rows).try_into().expect("one document");
        let key = json::text_of(&answer, "encoded").expect("an encoded key");
        let mut config = server.config.clone();
        config.auth = ElasticsearchAuth::ApiKey;
        config.server.user.clear();
        let mut connection = connect(&config, Some(&Password::new(key)), Route::Direct)
            .await
            .unwrap_or_else(|error| panic!("API key: {}", describe_error(&error)));
        connection.ping().await.expect("ping with an API key");
        conformance::check_connect_failure(
            connect(
                &config,
                Some(&Password::new("bm90OmFrZXk=".to_owned())),
                Route::Direct,
            )
            .await,
        );
    }
}

#[test]
fn a_hit_is_its_metadata_then_its_source() {
    let hit = Value::parse(
        r#"{"_index":"orders","_id":"1","_score":1.5,"_source":{"status":"open","total":12.50},"sort":[1],"highlight":{"status":["<em>open</em>"]}}"#,
    )
    .unwrap();
    assert_eq!(
        hit_document(&hit).to_json(),
        r#"{"_index":"orders","_id":"1","_score":1.5,"status":"open","total":12.50,"highlight":{"status":["<em>open</em>"]}}"#
    );
    // A source field named as one of the hit's own keeps the source whole.
    let clash =
        Value::parse(r#"{"_index":"o","_id":"2","_score":null,"_source":{"_score":3}}"#).unwrap();
    assert_eq!(
        hit_document(&clash).to_json(),
        r#"{"_index":"o","_id":"2","_score":null,"_source":{"_score":3}}"#
    );
}

#[test]
fn pages_only_searches_that_ask_for_hits() {
    let paged = |text: &str| Paging::of(&search::parse(text).unwrap()).is_some();
    let cases = [
        ("GET /orders/_search", true),
        ("GET /_search?q=status:open", true),
        (
            "POST /orders,logs/_search\n{ \"query\": { \"match_all\": {} }, \"size\": 50 }",
            true,
        ),
        ("GET /orders/_search\n{ \"sort\": \"created\" }", true),
        ("GET /orders/_search\n{ \"aggs\": {} }", false),
        ("GET /orders/_search\n{ \"from\": 10 }", false),
        ("GET /orders/_search\n{ \"size\": 0 }", false),
        ("GET /orders/_search?scroll=1m", false),
        ("GET /orders/_search?routing=a", false),
        ("GET /orders/_search/template", false),
        ("GET /orders/_count", false),
        ("DELETE /orders/_search", false),
    ];
    for (request, expected) in cases {
        assert_eq!(paged(request), expected, "{request}");
    }
}
