use std::path::{Path, PathBuf};

use dbrover_core::conformance;
use dbrover_core::{
    CatalogPath, ChildrenQuery, ElasticsearchAuth, FolderKind, NodeKind, Port, ResultShape, Row,
    ServerConfig, TlsMode, describe_error,
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

/// Runs each request, ignoring how it went: clearing what an earlier run left.
async fn clear(connection: &mut SearchConnection, requests: &[&str]) {
    for request in requests {
        let _ = connection.execute(request).await;
    }
}

fn folder(kind: FolderKind) -> CatalogPath {
    CatalogPath::root().child(NodeKind::Folder(kind), kind.label())
}

async fn labelled(
    connection: &mut SearchConnection,
    path: &CatalogPath,
) -> Vec<(String, Option<String>)> {
    connection
        .children(path)
        .await
        .unwrap_or_else(|error| panic!("{path}: {}", describe_error(&error)))
        .into_iter()
        .map(|node| (node.label, node.detail))
        .collect()
}

/// The tree's folders, and each kind in them: an index with its count and its fields by path,
/// an alias with its index, a data stream with its backing index, a template with its
/// patterns.
#[tokio::test]
async fn lists_indices_aliases_data_streams_and_templates() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        clear(
            &mut connection,
            &[
                "DELETE /_data_stream/dbrover-catalog-stream",
                "DELETE /_index_template/dbrover_catalog_template",
                "DELETE /dbrover_catalog_mapped",
            ],
        )
        .await;
        fill(&mut connection, "dbrover_catalog", 3).await;
        for request in [
            "PUT /dbrover_catalog_mapped\n{ \"aliases\": { \"dbrover_catalog_alias\": {} }, \"mappings\": { \"properties\": { \"address\": { \"properties\": { \"city\": { \"type\": \"text\", \"fields\": { \"keyword\": { \"type\": \"keyword\" } } } } }, \"tags\": { \"type\": \"nested\", \"properties\": { \"name\": { \"type\": \"keyword\" } } } } } }",
            "PUT /_index_template/dbrover_catalog_template\n{ \"index_patterns\": [\"dbrover-catalog-stream*\"], \"data_stream\": {}, \"template\": { \"mappings\": { \"properties\": { \"@timestamp\": { \"type\": \"date\" } } } } }",
            "PUT /_data_stream/dbrover-catalog-stream",
        ] {
            run(&mut connection, request).await;
        }

        let root: Vec<String> = labelled(&mut connection, &CatalogPath::root())
            .await
            .into_iter()
            .map(|(label, _)| label)
            .collect();
        assert_eq!(
            root,
            ["indices", "aliases", "data streams", "index templates"]
        );
        conformance::walk_catalog(&mut connection, 2).await;

        let indices = labelled(&mut connection, &folder(FolderKind::Indices)).await;
        assert!(
            indices.contains(&("dbrover_catalog".to_owned(), Some("3 documents".to_owned()))),
            "{}: {indices:?}",
            server.name
        );
        let mapped =
            folder(FolderKind::Indices).child(NodeKind::SearchIndex, "dbrover_catalog_mapped");
        let fields = labelled(&mut connection, &mapped).await;
        let expected = [
            ("address.city", "text"),
            ("address.city.keyword", "keyword"),
            ("tags", "nested"),
            ("tags.name", "keyword"),
        ]
        .map(|(name, kind)| (name.to_owned(), Some(kind.to_owned())));
        assert_eq!(fields, expected, "{}", server.name);

        let aliases = labelled(&mut connection, &folder(FolderKind::Aliases)).await;
        assert!(
            aliases.contains(&(
                "dbrover_catalog_alias".to_owned(),
                Some("→ dbrover_catalog_mapped".to_owned())
            )),
            "{}: {aliases:?}",
            server.name
        );
        let streams = labelled(&mut connection, &folder(FolderKind::DataStreams)).await;
        assert!(
            streams.contains(&(
                "dbrover-catalog-stream".to_owned(),
                Some("1 backing index".to_owned())
            )),
            "{}: {streams:?}",
            server.name
        );
        let templates = labelled(&mut connection, &folder(FolderKind::IndexTemplates)).await;
        assert!(
            templates.contains(&(
                "dbrover_catalog_template".to_owned(),
                Some("dbrover-catalog-stream*".to_owned())
            )),
            "{}: {templates:?}",
            server.name
        );
        conformance::check_invalid_path(&mut connection).await;
    }
}

/// What **Show definition** gives runs again as written: an index made from its definition
/// under another name has the same definition, and so has a template; an alias's actions
/// can be sent again.
#[tokio::test]
async fn definitions_run_again_as_written() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        clear(
            &mut connection,
            &[
                "DELETE /dbrover_definition",
                "DELETE /dbrover_definition_copy",
                "DELETE /_index_template/dbrover_definition_template",
                "DELETE /_index_template/dbrover_definition_template_copy",
            ],
        )
        .await;
        run(
            &mut connection,
            "PUT /dbrover_definition\n{ \"settings\": { \"number_of_replicas\": 0, \"refresh_interval\": \"5s\" }, \"aliases\": { \"dbrover_definition_alias\": { \"filter\": { \"term\": { \"status\": \"open\" } } } }, \"mappings\": { \"properties\": { \"status\": { \"type\": \"keyword\" }, \"total\": { \"type\": \"scaled_float\", \"scaling_factor\": 100 } } } }",
        )
        .await;
        run(
            &mut connection,
            "PUT /_index_template/dbrover_definition_template\n{ \"index_patterns\": [\"dbrover-definition-*\"], \"priority\": 7, \"template\": { \"settings\": { \"number_of_shards\": 1 } } }",
        )
        .await;
        let cases = [
            (
                FolderKind::Indices,
                NodeKind::SearchIndex,
                "dbrover_definition",
            ),
            (
                FolderKind::IndexTemplates,
                NodeKind::IndexTemplate,
                "dbrover_definition_template",
            ),
        ];
        for (kind, node, name) in cases {
            let path = folder(kind).child(node, name);
            let definition = connection.definition(&path).await.unwrap_or_else(|error| {
                panic!("{}: {name}: {}", server.name, describe_error(&error))
            });
            let copy = format!("{name}_copy");
            // Two templates may not match the same names at the same priority.
            let made_again = definition
                .replacen(name, &copy, 1)
                .replace("dbrover-definition-*", "dbrover-copied-*");
            run(&mut connection, &made_again).await;
            let again = connection
                .definition(&folder(kind).child(node, &copy))
                .await
                .unwrap_or_else(|error| {
                    panic!("{}: {copy}: {}", server.name, describe_error(&error))
                });
            assert_eq!(again, made_again, "{}", server.name);
        }
        let alias =
            folder(FolderKind::Aliases).child(NodeKind::SearchIndex, "dbrover_definition_alias");
        let definition = connection
            .definition(&alias)
            .await
            .expect("the alias's definition");
        assert!(
            definition.contains("\"filter\""),
            "{}: {definition}",
            server.name
        );
        run(&mut connection, &definition).await;
    }
}

/// The indices page by name, the filter applied by the server, in any case.
#[tokio::test]
async fn pages_indices_by_name_with_the_filter_on_the_server() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        for n in 0..7 {
            let _ = connection
                .execute(&format!("PUT /dbrover_paging_{n}"))
                .await;
        }
        let mut query = ChildrenQuery {
            filter: Some("DBROVER_PAGING_".to_owned()),
            after: None,
            limit: Some(3),
        };
        let mut pages = Vec::new();
        loop {
            let page = connection
                .children_page(&folder(FolderKind::Indices), &query)
                .await
                .expect("a page");
            pages.push(
                page.nodes
                    .iter()
                    .map(|node| node.label.clone())
                    .collect::<Vec<_>>(),
            );
            match page.next {
                Some(next) => query = query.after(next),
                None => break,
            }
        }
        let expected: Vec<Vec<String>> = [vec![0, 1, 2], vec![3, 4, 5], vec![6]]
            .into_iter()
            .map(|page| {
                page.into_iter()
                    .map(|n| format!("dbrover_paging_{n}"))
                    .collect()
            })
            .collect();
        assert_eq!(pages, expected, "{}", server.name);
        let none = connection
            .children_page(
                &folder(FolderKind::Indices),
                &ChildrenQuery::first_page(Some("no/such".to_owned())),
            )
            .await
            .expect("an empty page");
        assert!(none.nodes.is_empty(), "{}", server.name);
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

/// ⌥⌘E's request shows the search's profile, and a query's mistakes are caught by the server
/// before it runs.
#[tokio::test]
async fn explains_and_validates_searches_on_the_server() {
    for server in test_servers() {
        let mut connection = server.connect().await;
        fill(&mut connection, "dbrover_explain", 10).await;
        let explained = dbrover_language::QueryLanguage::Search
            .explain(
                "GET /dbrover_explain/_search\n{ \"query\": { \"range\": { \"n\": { \"gt\": 4 } } } }",
                None,
                dbrover_language::ExplainDepth::Estimated,
            )
            .expect("an explained search");
        let (_, rows) = run(&mut connection, &explained.text).await;
        let [answer] = documents(&rows).try_into().expect("one document");
        assert!(
            json::at(&answer, &["profile", "shards"]).is_some(),
            "{}: {answer:?}",
            server.name
        );
        for (request, caught) in [
            (
                "GET /dbrover_explain/_search\n{ \"query\": { \"bogus\": {} } }",
                "bogus",
            ),
            (
                "POST /dbrover_explain/_count\n{ \"query\": { \"range\": { \"n\": { \"gte\": \"abc\" } } } }",
                "abc",
            ),
        ] {
            let rejection = connection.validate(request).await.expect("validated");
            assert!(
                rejection
                    .as_ref()
                    .is_some_and(|rejection| rejection.message.contains(caught)),
                "{}: {request}: {rejection:?}",
                server.name
            );
        }
        for valid in [
            "GET /dbrover_explain/_search\n{ \"query\": { \"term\": { \"n\": 3 } } }",
            "GET /dbrover_explain/_search",
            // A write is only read: nothing is sent, so nothing is deleted.
            "POST /dbrover_explain/_delete_by_query\n{ \"query\": { \"match_all\": {} } }",
        ] {
            assert_eq!(
                connection.validate(valid).await.expect("validated"),
                None,
                "{}: {valid}",
                server.name
            );
        }
        let (_, rows) = run(&mut connection, "GET /dbrover_explain/_count").await;
        let [answer] = documents(&rows).try_into().expect("one document");
        assert_eq!(
            field(&answer, "count"),
            Some(&Value::Number("10".to_owned()))
        );
    }
}

/// The first document `request` answers with.
async fn first_document(connection: &mut SearchConnection, request: &str) -> Value {
    let (_, rows) = run(connection, request).await;
    documents(&rows).into_iter().next().expect("a document")
}

/// What the grid's edits send: each field set with its JSON type and read back so, an
/// object's field by its path, a document added, deleted, and put back by the deletion's undo.
#[tokio::test]
async fn edits_hits_keeping_each_fields_type() {
    use dbrover_document::{Path, Step};
    use dbrover_language::{DocumentView, QueryLanguage};

    for server in test_servers() {
        let mut connection = server.connect().await;
        let _ = connection.execute("DELETE /dbrover_edits").await;
        run(
            &mut connection,
            "PUT /dbrover_edits/_doc/1?refresh=true\n{ \"text\": \"a\", \"number\": 1, \"flag\": true, \"address\": { \"city\": \"Madrid\" } }",
        )
        .await;
        let search = "GET /dbrover_edits/_search";
        let view = QueryLanguage::Search
            .document_view(search)
            .expect("a search's hits are edited");
        let path = |names: &[&str]| {
            names.iter().fold(Path::default(), |path, name| {
                path.child(Step::Field((*name).to_owned()))
            })
        };
        let cases: [(&[&str], &str, Value); 4] = [
            (&["text"], "8", Value::String("8".to_owned())),
            (&["number"], "2.5", Value::Number("2.5".to_owned())),
            (&["flag"], "false", Value::Bool(false)),
            (
                &["address", "city"],
                "Paris",
                Value::String("Paris".to_owned()),
            ),
        ];
        for (names, typed, expected) in cases {
            let document = first_document(&mut connection, search).await;
            let old = names
                .iter()
                .try_fold(&document, |value, name| json::field(value, name));
            let (edit, _) = view
                .update(&document, &path(names), old, Some(typed))
                .unwrap_or_else(|error| panic!("{}: {names:?}: {error}", server.name));
            run(&mut connection, &edit.command).await;
            let document = first_document(&mut connection, search).await;
            let new = names
                .iter()
                .try_fold(&document, |value, name| json::field(value, name));
            assert_eq!(new, Some(&expected), "{}: {names:?}", server.name);
        }

        let added = view
            .insert(&[("_id", Some("2")), ("text", Some("b"))])
            .expect("an added document");
        run(&mut connection, &added).await;
        let (_, rows) = run(&mut connection, search).await;
        assert_eq!(rows.len(), 2, "{}", server.name);
        let document = first_document(&mut connection, search).await;
        let DocumentView::Index(_) = &view else {
            panic!("a search's view is an index's");
        };
        let deleted = view.delete(&document).expect("a deletion");
        run(&mut connection, &deleted.command).await;
        let (_, rows) = run(&mut connection, search).await;
        assert_eq!(rows.len(), 1, "{}", server.name);
        run(&mut connection, &deleted.revert.expect("an undo")).await;
        let (_, rows) = run(&mut connection, search).await;
        assert_eq!(rows.len(), 2, "{}", server.name);
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
