use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dbrover_core::conformance;
use dbrover_core::{ChildrenQuery, KeyType, NodeKind, ResultShape, Row, TlsMode, describe_error};

use super::*;

/// Point the tests at servers, e.g. `redis://dbrover:dbrover@localhost:56379/0` from the
/// repository's `docker-compose.yml` (see `test-servers.env`). Each test runs on every server
/// set, Redis and Valkey, and is skipped when none is, so `cargo test` works offline.
const TEST_URL_VARS: [&str; 2] = ["DBROVER_TEST_REDIS_URL", "DBROVER_TEST_VALKEY_URL"];
/// The Redis server's TLS port, e.g. `redis://dbrover:dbrover@localhost:56443/0`.
const TLS_TEST_URL_VAR: &str = "DBROVER_TEST_REDIS_TLS_URL";

/// Tests that keep the whole server busy take this for writing, and every other server test for
/// reading. While a script runs, the server refuses every other client's command with `BUSY`
/// (and `DEBUG SLEEP` makes them wait), which would fail the tests running beside them.
static SERVER: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

struct TestServer {
    variable: &'static str,
    config: RedisConfig,
    password: Option<Password>,
}

fn test_servers() -> Vec<TestServer> {
    let servers: Vec<TestServer> = TEST_URL_VARS.into_iter().filter_map(server_from).collect();
    if servers.is_empty() {
        eprintln!("skipping: none of {TEST_URL_VARS:?} is set");
    }
    servers
}

fn server_from(variable: &'static str) -> Option<TestServer> {
    let url = std::env::var(variable).ok()?;
    Some(parse_url(variable, &url))
}

/// The authority that signed the TLS test server's certificate.
fn test_ca_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/postgres-tls/ca.crt")
}

/// Reads `redis://user:password@host:port/database`.
fn parse_url(variable: &'static str, url: &str) -> TestServer {
    let rest = url.strip_prefix("redis://").expect("redis:// URL");
    let (credentials, rest) = rest.split_once('@').expect("user:password@ in URL");
    let (user, password) = credentials.split_once(':').expect("password in URL");
    let (address, database) = rest.split_once('/').expect("/database in URL");
    let (host, port) = address.split_once(':').expect("host:port in URL");
    TestServer {
        variable,
        config: RedisConfig {
            server: ServerConfig {
                host: host.to_owned(),
                port: port.parse::<u16>().expect("numeric port").into(),
                database: database.to_owned(),
                user: user.to_owned(),
                tls: TlsMode::Disable,
                ssh: None,
                password_command: None,
                cloud_sign_in: None,
            },
            separator: RedisConfig::DEFAULT_SEPARATOR.to_owned(),
        },
        password: Some(Password::new(password)),
    }
}

impl TestServer {
    async fn connect(&self) -> RedisConnection {
        connect(&self.config, self.password.as_ref(), Route::Direct)
            .await
            .unwrap_or_else(|error| panic!("{}: {}", self.variable, describe_error(&error)))
    }

    async fn connect_with(&self, tls: TlsMode) -> Result<RedisConnection, DriverError> {
        let mut config = self.config.clone();
        config.server.tls = tls;
        connect(&config, self.password.as_ref(), Route::Direct).await
    }
}

/// Runs `command` to its end and returns its rows.
async fn run(connection: &mut RedisConnection, command: &str) -> Vec<Row> {
    let mut cursor = connection
        .execute(command)
        .await
        .unwrap_or_else(|error| panic!("{command}: {}", describe_error(&error)));
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(500).await.expect("page"));
    }
    cursor.finish().await.expect("finish");
    rows
}

/// The rows of `command`, each cell as text, `-` for nil.
async fn texts(connection: &mut RedisConnection, command: &str) -> Vec<Vec<String>> {
    run(connection, command)
        .await
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| cell.unwrap_or_else(|| "-".to_owned()))
                .collect()
        })
        .collect()
}

/// A prefix no other test's keys share, so tests on the same server don't see each other's.
fn prefix(test: &str) -> String {
    format!("dbrover:test:{test}:")
}

/// Deletes every key under `prefix`.
async fn delete_keys(connection: &mut RedisConnection, prefix: &str) {
    let keys = texts(connection, &format!("SCAN 0 MATCH {prefix}* COUNT 1000")).await;
    for chunk in keys.chunks(500) {
        let names: Vec<&str> = chunk.iter().map(|row| row[0].as_str()).collect();
        run(connection, &format!("UNLINK {}", names.join(" "))).await;
    }
}

#[tokio::test]
async fn conforms_to_the_connection_lifecycle() {
    let _server = SERVER.read().await;
    for server in test_servers() {
        let connection = server.connect().await;
        let product = connection.server_info().product.clone();
        let expected = if server.variable.contains("VALKEY") {
            "Valkey"
        } else {
            "Redis"
        };
        assert_eq!(product, expected, "{}", server.variable);
        conformance::check_lifecycle(Box::new(connection)).await;
    }
}

#[tokio::test]
async fn wrong_password_is_a_connect_error() {
    let _server = SERVER.read().await;
    for server in test_servers() {
        let wrong = Password::new("not the password");
        conformance::check_connect_failure(
            connect(&server.config, Some(&wrong), Route::Direct).await,
        );
    }
}

#[tokio::test]
async fn unreachable_server_is_a_connect_error() {
    let _server = SERVER.read().await;
    let mut config = RedisConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_owned(),
            port: 1.into(),
            database: "0".to_owned(),
            user: String::new(),
            tls: TlsMode::Disable,
            ssh: None,
            password_command: None,
            cloud_sign_in: None,
        },
        separator: RedisConfig::DEFAULT_SEPARATOR.to_owned(),
    };
    conformance::check_connect_failure(connect(&config, None, Route::Direct).await);
    config.server.port = 6379.into();
    config.server.database = "not a number".to_owned();
    conformance::check_connect_failure(connect(&config, None, Route::Direct).await);
}

#[tokio::test]
async fn encrypts_as_the_tls_mode_says() {
    let _server = SERVER.read().await;
    let Some(server) = server_from(TLS_TEST_URL_VAR) else {
        eprintln!("skipping: {TLS_TEST_URL_VAR} is not set");
        return;
    };
    let mut require = server
        .connect_with(TlsMode::Require)
        .await
        .expect("require");
    run(&mut require, "PING").await;
    let mut verified = server
        .connect_with(TlsMode::VerifyFull {
            ca_file: Some(test_ca_file()),
        })
        .await
        .expect("verify-full with the test authority");
    run(&mut verified, "PING").await;
    // The public authorities didn't sign the test certificate.
    conformance::check_connect_failure(
        server
            .connect_with(TlsMode::VerifyFull { ca_file: None })
            .await,
    );
    // The TLS port doesn't speak plain RESP.
    conformance::check_connect_failure(server.connect_with(TlsMode::Disable).await);
}

#[tokio::test]
async fn connects_through_a_tunnel_checking_tls_against_the_servers_name() {
    let _server = SERVER.read().await;
    let Some(server) = server_from(TLS_TEST_URL_VAR) else {
        eprintln!("skipping: {TLS_TEST_URL_VAR} is not set");
        return;
    };
    let address = format!(
        "{}:{}",
        server.config.server.host, server.config.server.port
    );
    let local = conformance::forwarder(&address).await;
    let mut config = server.config.clone();
    config.server.tls = TlsMode::VerifyFull {
        ca_file: Some(test_ca_file()),
    };
    let mut connection = connect(&config, server.password.as_ref(), Route::Tunnel(local))
        .await
        .expect("connect through the forwarder");
    run(&mut connection, "PING").await;
}

#[tokio::test]
async fn shows_each_reply_by_its_shape() {
    let _server = SERVER.read().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keys = prefix("shapes");
        delete_keys(&mut connection, &keys).await;

        run(
            &mut connection,
            &format!("SET {keys}name \"Ada \\\"the first\\\"\""),
        )
        .await;
        assert_eq!(
            texts(&mut connection, &format!("GET {keys}name")).await,
            [["Ada \"the first\""]]
        );
        assert_eq!(
            texts(&mut connection, &format!("GET {keys}missing")).await,
            [["-"]]
        );
        run(
            &mut connection,
            &format!("SET {keys}binary \"\\xff\\x00a\""),
        )
        .await;
        assert_eq!(
            texts(&mut connection, &format!("GET {keys}binary")).await,
            [["\"\\xff\\x00a\""]]
        );

        run(
            &mut connection,
            &format!("HSET {keys}user name Ada city London"),
        )
        .await;
        let mut cursor = connection
            .execute(&format!("HGETALL {keys}user"))
            .await
            .expect("hgetall");
        let columns: Vec<String> = cursor.columns().iter().map(|c| c.name.clone()).collect();
        assert_eq!(columns, ["field", "value"]);
        let mut fields = cursor.next_page(10).await.expect("fields");
        fields.sort();
        assert_eq!(
            fields,
            [
                vec![Some("city".to_owned()), Some("London".to_owned())],
                vec![Some("name".to_owned()), Some("Ada".to_owned())],
            ]
        );
        cursor.finish().await.expect("finish");

        run(&mut connection, &format!("RPUSH {keys}queue a b c")).await;
        assert_eq!(
            texts(&mut connection, &format!("LRANGE {keys}queue 0 -1")).await,
            [["0", "a"], ["1", "b"], ["2", "c"]]
        );

        run(
            &mut connection,
            &format!("ZADD {keys}scores 10 ada 7.5 bob"),
        )
        .await;
        assert_eq!(
            texts(
                &mut connection,
                &format!("ZRANGE {keys}scores 0 -1 WITHSCORES")
            )
            .await,
            [["bob", "7.5"], ["ada", "10"]]
        );

        run(
            &mut connection,
            &format!("XADD {keys}events 1-0 kind login user ada"),
        )
        .await;
        run(
            &mut connection,
            &format!("XADD {keys}events 2-0 kind logout"),
        )
        .await;
        let cursor = connection
            .execute(&format!("XREVRANGE {keys}events + -"))
            .await
            .expect("xrevrange");
        assert_eq!(cursor.shape(), ResultShape::Documents);
        drop(cursor);
        assert_eq!(
            texts(&mut connection, &format!("XREVRANGE {keys}events + -")).await,
            [
                [r#"{"_id":"2-0","kind":"logout"}"#],
                [r#"{"_id":"1-0","kind":"login","user":"ada"}"#],
            ]
        );

        let failure = connection.execute(&format!("HGET {keys}queue x")).await;
        match failure {
            Err(DriverError::Statement { source, .. }) => {
                assert!(source.to_string().starts_with("WRONGTYPE"), "{source}");
            }
            Err(other) => panic!("expected the server's error, got {other:?}"),
            Ok(_) => panic!("HGET on a list ran"),
        }
        run(&mut connection, "PING").await;
        delete_keys(&mut connection, &keys).await;
    }
}

/// A `SCAN` is followed to its end in pages, whatever `COUNT` the command asks of each call.
#[tokio::test]
async fn follows_a_scan_to_its_end() {
    let _server = SERVER.read().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keys = prefix("scan");
        delete_keys(&mut connection, &keys).await;
        for batch in 0..5 {
            let pairs: Vec<String> = (0..500)
                .map(|index| format!("{keys}{} x", batch * 500 + index))
                .collect();
            run(&mut connection, &format!("MSET {}", pairs.join(" "))).await;
        }
        let mut cursor = connection
            .execute(&format!("SCAN 0 MATCH {keys}* COUNT 100"))
            .await
            .expect("scan");
        let mut seen = std::collections::BTreeSet::new();
        let mut pages = 0;
        while !cursor.is_exhausted() {
            let page = cursor.next_page(500).await.expect("a page of keys");
            assert!(page.len() <= 500);
            pages += 1;
            seen.extend(
                page.into_iter()
                    .filter_map(|row| row.into_iter().next().flatten()),
            );
        }
        assert_eq!(seen.len(), 2_500, "{}", server.variable);
        assert!(pages >= 5, "{pages} pages");

        run(&mut connection, &format!("HSET {keys}hash a 1 b 2 c 3")).await;
        let mut fields = texts(&mut connection, &format!("HSCAN {keys}hash 0")).await;
        fields.sort();
        assert_eq!(fields, [["a", "1"], ["b", "2"], ["c", "3"]]);
        delete_keys(&mut connection, &keys).await;
    }
}

/// Reads every row of `command` in pages of 500, and returns them and how many pages it took.
async fn paged(connection: &mut RedisConnection, command: &str) -> (Vec<Row>, usize) {
    let mut cursor = connection.execute(command).await.expect(command);
    let mut rows = Vec::new();
    let mut pages = 0;
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(500).await.expect("a page"));
        pages += 1;
    }
    (rows, pages)
}

/// A list, a sorted set and a stream of 1,200 items each are read a page at a time, the list's
/// items with their index, the stream's entries newest first from `XREVRANGE`.
#[tokio::test]
async fn reads_large_values_a_page_at_a_time() {
    let _server = SERVER.read().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keys = prefix("ranges");
        delete_keys(&mut connection, &keys).await;
        for batch in 0..3 {
            let numbers: Vec<String> = (0..400)
                .map(|index| (batch * 400 + index).to_string())
                .collect();
            run(
                &mut connection,
                &format!("RPUSH {keys}list {}", numbers.join(" ")),
            )
            .await;
            let pairs: Vec<String> = numbers.iter().map(|n| format!("{n} m{n}")).collect();
            run(
                &mut connection,
                &format!("ZADD {keys}scores {}", pairs.join(" ")),
            )
            .await;
        }
        for index in 1..=1_200 {
            run(
                &mut connection,
                &format!("XADD {keys}events {index}-0 n {index}"),
            )
            .await;
        }

        let (rows, pages) = paged(&mut connection, &format!("LRANGE {keys}list 0 -1")).await;
        assert_eq!(rows.len(), 1_200);
        assert!(pages >= 3, "{pages} pages");
        assert_eq!(
            rows[1_199],
            vec![Some("1199".to_owned()), Some("1199".to_owned())]
        );
        let (tail, _) = paged(&mut connection, &format!("LRANGE {keys}list -2 -1")).await;
        assert_eq!(tail[0][0].as_deref(), Some("1198"));

        let (rows, _) = paged(
            &mut connection,
            &format!("ZRANGE {keys}scores 0 -1 WITHSCORES REV"),
        )
        .await;
        assert_eq!(rows.len(), 1_200);
        assert_eq!(
            rows[0],
            vec![Some("m1199".to_owned()), Some("1199".to_owned())]
        );

        let cursor = connection
            .execute(&format!("XREVRANGE {keys}events + -"))
            .await
            .expect("xrevrange");
        assert_eq!(cursor.shape(), ResultShape::Documents);
        drop(cursor);
        let (rows, pages) = paged(&mut connection, &format!("XREVRANGE {keys}events + -")).await;
        assert_eq!(rows.len(), 1_200);
        assert!(pages >= 3, "{pages} pages");
        assert_eq!(
            rows[0],
            vec![Some(r#"{"_id":"1200-0","n":"1200"}"#.to_owned())]
        );
        assert_eq!(
            rows[1_199],
            vec![Some(r#"{"_id":"1-0","n":"1"}"#.to_owned())]
        );
        delete_keys(&mut connection, &keys).await;
    }
}

/// `CLIENT UNBLOCK` ends a blocking command, and the session goes on.
#[tokio::test]
async fn cancels_a_blocking_command() {
    let _server = SERVER.read().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        conformance::check_cancel(&mut connection, "BLPOP dbrover:test:never 30").await;
    }
}

/// `SCRIPT KILL` stops a script that hasn't written. The test servers answer other clients
/// after a script has run 100 ms (`busy-reply-threshold`), rather than the default 5 s.
#[tokio::test]
async fn cancels_a_script() {
    let _server = SERVER.write().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        conformance::check_cancel(&mut connection, "EVAL \"while true do end\" 0").await;
    }
}

/// A command nothing can stop is left to finish while a new session takes over.
#[tokio::test]
async fn stops_waiting_for_a_command_nothing_stops() {
    let _server = SERVER.write().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        run(&mut connection, "SELECT 2").await;
        let handle = connection.cancel_handle();
        let started = Instant::now();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            handle.cancel().await
        });
        let result = connection.execute("DEBUG SLEEP 2").await;
        match result {
            Err(DriverError::Statement { source, .. }) => {
                assert!(
                    source.to_string().contains("may still be finishing"),
                    "{source}"
                );
            }
            other => panic!("expected a cancelled command, got {:?}", other.err()),
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        canceller.await.expect("cancel task").expect("cancel");
        // The new session selects the database the old one had chosen.
        run(&mut connection, &format!("SET {}db marked", prefix("stop"))).await;
        run(&mut connection, "SELECT 2").await;
        assert_eq!(
            texts(&mut connection, &format!("GETDEL {}db", prefix("stop"))).await,
            [["marked"]]
        );
    }
}

#[tokio::test]
async fn reports_a_killed_session_as_lost() {
    let _server = SERVER.read().await;
    for server in test_servers() {
        let connection = server.connect().await;
        let id = connection
            .session
            .client
            .expect("the test user may ask its client id");
        let mut killer = server.connect().await;
        run(&mut killer, &format!("CLIENT KILL ID {id}")).await;
        conformance::check_lost(Box::new(connection)).await;
    }
}

#[tokio::test]
async fn lists_the_databases_with_their_key_counts() {
    let _server = SERVER.read().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        run(&mut connection, "SELECT 5").await;
        run(
            &mut connection,
            &format!("SET {}one 1", prefix("databases")),
        )
        .await;
        let nodes = conformance::walk_catalog(&mut connection, 2).await;
        let labels: Vec<(&str, Option<&str>)> = nodes
            .iter()
            .map(|node| (node.label.as_str(), node.detail.as_deref()))
            .collect();
        assert!(
            labels.iter().any(|(label, _)| *label == "db0"),
            "{labels:?}"
        );
        assert!(labels.contains(&("db5", Some("1 key"))), "{labels:?}");
        conformance::check_invalid_path(&mut connection).await;
        run(&mut connection, &format!("DEL {}one", prefix("databases"))).await;
    }
}

/// Selects `database`, empties it, and fills it with `keys`, each a string set to `x`.
async fn fill_database(connection: &mut RedisConnection, database: u8, keys: &[String]) {
    run(connection, &format!("SELECT {database}")).await;
    run(connection, "FLUSHDB").await;
    for chunk in keys.chunks(500) {
        let pairs: Vec<String> = chunk.iter().map(|key| format!("{key} x")).collect();
        run(connection, &format!("MSET {}", pairs.join(" "))).await;
    }
}

/// The level at `path`, whole, as label, kind and detail, sorted by label.
async fn key_level(
    connection: &mut RedisConnection,
    path: CatalogPath,
) -> Vec<(String, Option<NodeKind>, Option<String>)> {
    let mut nodes: Vec<(String, Option<NodeKind>, Option<String>)> = connection
        .children(&path)
        .await
        .expect("a level of keys")
        .into_iter()
        .map(|node| (node.label.clone(), node.kind(), node.detail))
        .collect();
    nodes.sort_by(|a, b| a.0.cmp(&b.0));
    nodes
}

fn database(number: u8) -> CatalogPath {
    CatalogPath::root().child(NodeKind::Database, number.to_string())
}

/// 2,500 keys come in three pages of at most 1,000, which hold every key once, and the filter,
/// as `SCAN`'s `MATCH`, finds the same keys the explorer's own filter would. The keys have no
/// separator, so grouping leaves them as they are. Database 9 is this test's alone.
///
/// Not `conformance::check_children_pages`, which compares the pages' order with a second
/// listing's: `SCAN` promises every key, not the same order twice, and a server still
/// rehashing a freshly filled table answers two passes in different orders.
#[tokio::test]
async fn pages_keys_with_scan_and_filters_with_match() {
    let _server = SERVER.read().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keys: Vec<String> = (1..=2_500).map(|index| format!("page{index:04}")).collect();
        fill_database(&mut connection, 9, &keys).await;
        let mut query = ChildrenQuery::first_page(None);
        let mut labels = Vec::new();
        let mut pages = 0;
        loop {
            let page = connection
                .children_page(&database(9), &query)
                .await
                .expect("a page of keys");
            pages += 1;
            assert!(page.nodes.len() <= 1_000, "page {pages} is over the limit");
            labels.extend(page.nodes.into_iter().map(|node| node.label));
            match page.next {
                Some(next) => query = query.after(next),
                None => break,
            }
        }
        assert_eq!(pages, 3, "{}", server.variable);
        labels.sort();
        assert_eq!(labels, keys, "every key once, on {}", server.variable);

        let filtered = connection
            .children_page(
                &database(9),
                &ChildrenQuery::first_page(Some("PAGE24".to_owned())),
            )
            .await
            .expect("the filtered level");
        let mut found: Vec<String> = filtered.nodes.into_iter().map(|node| node.label).collect();
        found.sort();
        let expected: Vec<String> = keys
            .iter()
            .filter(|key| key.contains("page24"))
            .cloned()
            .collect();
        assert_eq!(found, expected, "{}", server.variable);
        run(&mut connection, "SELECT 9").await;
        run(&mut connection, "FLUSHDB").await;
    }
}

/// Keys group by `:` level by level, each key with its type and time to live; a filter lists
/// the matching keys themselves, however deep. Database 8 is this test's alone.
#[tokio::test]
async fn groups_keys_by_their_separator() {
    let _server = SERVER.read().await;
    for server in test_servers() {
        let mut connection = server.connect().await;
        let keys = [
            "user:1:cart",
            "user:1:name",
            "user:2:name",
            "session:abc",
            "plain",
        ]
        .map(str::to_owned);
        fill_database(&mut connection, 8, &keys).await;
        run(&mut connection, "DEL user:1:cart").await;
        run(&mut connection, "HSET user:1:cart sku 42").await;
        run(&mut connection, "EXPIRE session:abc 7200").await;

        let string = Some(NodeKind::Key(KeyType::String));
        let group = Some(NodeKind::KeyPrefix);
        assert_eq!(
            key_level(&mut connection, database(8)).await,
            [
                ("plain".to_owned(), string, Some("string".to_owned())),
                ("session".to_owned(), group, None),
                ("user".to_owned(), group, None),
            ]
        );
        let user = database(8).child(NodeKind::KeyPrefix, "user:");
        let ones = key_level(&mut connection, user.child(NodeKind::KeyPrefix, "user:1:")).await;
        assert_eq!(
            ones,
            [
                (
                    "cart".to_owned(),
                    Some(NodeKind::Key(KeyType::Hash)),
                    Some("hash".to_owned())
                ),
                ("name".to_owned(), string, Some("string".to_owned())),
            ]
        );
        let session = key_level(
            &mut connection,
            database(8).child(NodeKind::KeyPrefix, "session:"),
        )
        .await;
        assert_eq!(session[0].2.as_deref(), Some("string · TTL 2 h"));

        let filtered = connection
            .children_page(
                &database(8),
                &ChildrenQuery::first_page(Some("NAME".to_owned())),
            )
            .await
            .expect("a filtered level");
        let mut labels: Vec<String> = filtered.nodes.into_iter().map(|node| node.label).collect();
        labels.sort();
        assert_eq!(labels, ["user:1:name", "user:2:name"]);
        run(&mut connection, "SELECT 8").await;
        run(&mut connection, "FLUSHDB").await;
    }
}

/// What the server's `COMMAND INFO` says of each command DbRover's table lists, in the form the
/// table writes: a mismatch, or a command the table doesn't list (a module's, say), fails with
/// the lines to put in `dbrover-language/src/redis/commands.rs`.
#[tokio::test]
async fn the_command_table_classifies_as_the_server_does() {
    let _server = SERVER.read().await;
    let mut differences = Vec::new();
    for server in test_servers() {
        let mut connection = server.connect().await;
        let info = connection
            .run(&[b"COMMAND".to_vec()])
            .await
            .expect("COMMAND");
        let docs = connection
            .run(&[b"COMMAND".to_vec(), b"DOCS".to_vec()])
            .await
            .expect("COMMAND DOCS");
        let docs = command_docs(&docs);
        let mut entries = BTreeMap::new();
        for command in values(&info) {
            server_entries(command, &docs, &mut entries);
        }
        let mismatched: Vec<String> = entries
            .iter()
            .filter(|(name, entry)| !table_agrees(name, entry))
            .map(|(name, entry)| entry.line(name))
            .collect();
        if !mismatched.is_empty() {
            differences.push(format!(
                "{}: the command table differs from the server for {} commands; their entries:\n{}",
                server.variable,
                mismatched.len(),
                mismatched.join("\n")
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// One command as the server describes it.
#[derive(Debug)]
struct ServerEntry {
    arity: i64,
    flags: Vec<&'static str>,
    syntax: String,
}

impl ServerEntry {
    fn line(&self, name: &str) -> String {
        let flags: Vec<String> = self
            .flags
            .iter()
            .map(|flag| {
                let mut chars = flag.chars();
                let first = chars
                    .next()
                    .map(|c| c.to_ascii_uppercase())
                    .unwrap_or_default();
                format!("Flag::{first}{}", chars.as_str())
            })
            .collect();
        format!(
            "    command({name:?}, {}, &[{}], {:?}),",
            self.arity,
            flags.join(", "),
            self.syntax
        )
    }
}

/// Whether the table classifies `name` as the server's flags do, stops it the same way, and
/// accepts the argument counts the server does.
fn table_agrees(name: &str, entry: &ServerEntry) -> bool {
    let Some((_, spec)) = language::command(
        &name
            .split('|')
            .map(|part| part.as_bytes().to_vec())
            .collect::<Vec<_>>(),
    ) else {
        return false;
    };
    if spec.name != name {
        return false;
    }
    let has = |flag: &str| entry.flags.contains(&flag);
    let flags_agree = has("readonly") == spec.has(language::Flag::Readonly)
        && has("write") == spec.has(language::Flag::Write)
        && has("admin") == spec.has(language::Flag::Admin)
        && has("blocking") == spec.has(language::Flag::Blocking);
    let least = usize::try_from(entry.arity.unsigned_abs()).unwrap_or(usize::MAX);
    let arity_agrees = spec.accepts(least) && (entry.arity > 0 || spec.accepts(least + 3));
    flags_agree && arity_agrees
}

const FLAGS: [&str; 4] = ["readonly", "write", "admin", "blocking"];

/// Adds `info`, one command's `COMMAND INFO` reply, and its subcommands to `entries`.
fn server_entries(
    info: &Value,
    docs: &BTreeMap<String, String>,
    entries: &mut BTreeMap<String, ServerEntry>,
) {
    let parts = values(info);
    let Some(name) = parts.first().and_then(reply::text) else {
        return;
    };
    let name = name.to_ascii_lowercase();
    let arity = match parts.get(1) {
        Some(Value::Int(arity)) => *arity,
        _ => 0,
    };
    let declared: Vec<String> = parts
        .get(2)
        .map(|flags| values(flags).iter().filter_map(reply::text).collect())
        .unwrap_or_default();
    let flags = FLAGS
        .into_iter()
        .filter(|flag| declared.iter().any(|declared| declared == flag))
        .collect();
    for subcommand in parts.get(9).map(values).unwrap_or_default() {
        server_entries(subcommand, docs, entries);
    }
    let syntax = docs.get(&name).cloned().unwrap_or_default();
    entries.insert(
        name,
        ServerEntry {
            arity,
            flags,
            syntax,
        },
    );
}

fn values(value: &Value) -> &[Value] {
    match value {
        Value::Array(items) | Value::Set(items) => items,
        _ => &[],
    }
}

fn field<'a>(map: &'a Value, name: &str) -> Option<&'a Value> {
    match map {
        Value::Map(pairs) => pairs
            .iter()
            .find(|(key, _)| reply::text(key).as_deref() == Some(name))
            .map(|(_, value)| value),
        _ => None,
    }
}

/// Each command's arguments' syntax from `COMMAND DOCS`, by name, subcommands as
/// `container|subcommand`.
fn command_docs(docs: &Value) -> BTreeMap<String, String> {
    let mut syntaxes = BTreeMap::new();
    add_docs(docs, None, &mut syntaxes);
    syntaxes
}

fn add_docs(docs: &Value, container: Option<&str>, syntaxes: &mut BTreeMap<String, String>) {
    let Value::Map(commands) = docs else {
        return;
    };
    for (name, doc) in commands {
        let Some(name) = reply::text(name).map(|name| name.to_ascii_lowercase()) else {
            continue;
        };
        let full = match container {
            Some(container) if !name.contains('|') => format!("{container}|{name}"),
            _ => name,
        };
        let arguments: Vec<String> = field(doc, "arguments")
            .map(values)
            .unwrap_or_default()
            .iter()
            .map(argument_syntax)
            .collect();
        if let Some(subcommands) = field(doc, "subcommands") {
            add_docs(subcommands, Some(&full), syntaxes);
        }
        syntaxes.insert(full, arguments.join(" "));
    }
}

/// One argument as Redis's documentation writes it: `TOKEN name`, `[optional]`,
/// `name [name ...]` for one repeated, `a | b` for a choice, `<a | b>` when one is required.
/// Written as the table's generator wrote it from the servers' sources.
fn argument_syntax(argument: &Value) -> String {
    let text = |name: &str| field(argument, name).and_then(reply::text);
    let flags: Vec<String> = field(argument, "flags")
        .map(values)
        .unwrap_or_default()
        .iter()
        .filter_map(reply::text)
        .collect();
    let has = |flag: &str| flags.iter().any(|declared| declared == flag);
    let token = text("token");
    let children: Vec<String> = field(argument, "arguments")
        .map(values)
        .unwrap_or_default()
        .iter()
        .map(argument_syntax)
        .collect();
    let mut syntax = match text("type").as_deref() {
        Some("pure-token") => token.clone().or_else(|| text("name")).unwrap_or_default(),
        Some("oneof") => {
            let choice = children.join(" | ");
            let choice = if has("optional") {
                choice
            } else {
                format!("<{choice}>")
            };
            token.map_or(choice.clone(), |token| format!("{token} {choice}"))
        }
        Some("block") => {
            let block = children.join(" ");
            token.map_or(block.clone(), |token| format!("{token} {block}"))
        }
        _ => {
            let name = text("display_text")
                .or_else(|| text("name"))
                .unwrap_or_default();
            token.map_or(name.clone(), |token| format!("{token} {name}"))
        }
    };
    if has("multiple") {
        syntax = format!("{syntax} [{syntax} ...]");
    }
    if has("optional") {
        syntax = format!("[{syntax}]");
    }
    syntax
}
