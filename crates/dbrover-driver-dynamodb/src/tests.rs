use std::collections::HashMap;
use std::time::{Duration, Instant};

use aws_sdk_dynamodb::primitives::Blob;
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, KeySchemaElement, KeyType, PutRequest,
    ScalarAttributeType, WriteRequest,
};
use dbrover_core::{NodeKind, ResultShape, Row, conformance, describe_error};
use dbrover_document::Value;

use super::*;

/// Point the tests at DynamoDB Local, e.g. `http://localhost:58000` from the repository's
/// `docker-compose.yml` (see `test-servers.env`). They sign in as a local emulator, with
/// made-up credentials, so no AWS account or file is ever read; and skip when it is unset, so
/// `cargo test` works offline.
const TEST_URL_VAR: &str = "DBROVER_TEST_DYNAMODB_URL";

fn test_config() -> Option<DynamoDbConfig> {
    let Ok(endpoint) = std::env::var(TEST_URL_VAR) else {
        eprintln!("skipping: {TEST_URL_VAR} is not set");
        return None;
    };
    Some(emulator(&endpoint))
}

fn emulator(endpoint: &str) -> DynamoDbConfig {
    DynamoDbConfig {
        sign_in: DynamoDbSignIn::LocalEmulator,
        profile: String::new(),
        region: String::new(),
        endpoint: endpoint.to_owned(),
    }
}

async fn connected(config: &DynamoDbConfig) -> DynamoConnection {
    connect(config)
        .await
        .unwrap_or_else(|error| panic!("connect: {}", describe_error(&error)))
}

/// A table of the tests' own, named for the test and unique to the run, dropped when the test
/// ends so the server keeps nothing between runs.
struct TestTable {
    client: Client,
    name: String,
}

impl TestTable {
    /// A table keyed by `pk` (a string) and, when `sort` is set, `sk` (a number).
    async fn create(connection: &DynamoConnection, test: &str, sort: bool) -> Self {
        let name = format!("{test}-{}", uuid::Uuid::new_v4().simple());
        let mut request = connection
            .client
            .create_table()
            .table_name(&name)
            .billing_mode(BillingMode::PayPerRequest)
            .key_schema(key("pk", KeyType::Hash))
            .attribute_definitions(attribute("pk", ScalarAttributeType::S));
        if sort {
            request = request
                .key_schema(key("sk", KeyType::Range))
                .attribute_definitions(attribute("sk", ScalarAttributeType::N));
        }
        request.send().await.expect("create the table");
        Self {
            client: connection.client.clone(),
            name,
        }
    }

    async fn put(&self, items: Vec<HashMap<String, AttributeValue>>) {
        // BatchWriteItem takes at most 25 items a request.
        for chunk in items.chunks(25) {
            let writes = chunk
                .iter()
                .map(|item| {
                    WriteRequest::builder()
                        .put_request(
                            PutRequest::builder()
                                .set_item(Some(item.clone()))
                                .build()
                                .expect("a put"),
                        )
                        .build()
                })
                .collect();
            self.client
                .batch_write_item()
                .request_items(&self.name, writes)
                .send()
                .await
                .expect("write the items");
        }
    }

    async fn drop(self) {
        self.client
            .delete_table()
            .table_name(&self.name)
            .send()
            .await
            .expect("drop the table");
    }
}

fn key(name: &str, key_type: KeyType) -> KeySchemaElement {
    KeySchemaElement::builder()
        .attribute_name(name)
        .key_type(key_type)
        .build()
        .expect("a key")
}

fn attribute(name: &str, scalar: ScalarAttributeType) -> AttributeDefinition {
    AttributeDefinition::builder()
        .attribute_name(name)
        .attribute_type(scalar)
        .build()
        .expect("an attribute")
}

fn keyed(pk: &str, sk: usize) -> HashMap<String, AttributeValue> {
    HashMap::from([
        ("pk".to_owned(), AttributeValue::S(pk.to_owned())),
        ("sk".to_owned(), AttributeValue::N(sk.to_string())),
    ])
}

/// Runs `statement` to its end and returns its shape and rows.
async fn run(connection: &mut DynamoConnection, statement: &str) -> (ResultShape, Vec<Row>) {
    let mut cursor = connection
        .execute(statement)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {}", describe_error(&error)));
    let shape = cursor.shape();
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(500).await.expect("a page"));
    }
    cursor.finish().await.expect("finish");
    (shape, rows)
}

fn documents(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|row| match row.as_slice() {
            [Some(text)] => text.clone(),
            other => panic!("not a document: {other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn connects_pings_and_closes() {
    let Some(config) = test_config() else { return };
    let connection = connected(&config).await;
    assert_eq!(connection.server_info().product, "DynamoDB");
    assert!(
        connection.server_info().version.contains(&config.endpoint),
        "{:?}",
        connection.server_info()
    );
    conformance::check_lifecycle(Box::new(connection)).await;
}

#[tokio::test]
async fn refuses_an_endpoint_that_does_not_answer_and_an_emulator_without_one() {
    conformance::check_connect_failure(connect(&emulator("http://127.0.0.1:1")).await);
    conformance::check_connect_failure(connect(&emulator("")).await);
}

#[tokio::test]
async fn lists_its_tables_and_refuses_paths_it_never_gives() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    let table = TestTable::create(&connection, "listed", false).await;
    let nodes = conformance::walk_catalog(&mut connection, 2).await;
    assert!(
        nodes
            .iter()
            .any(|node| node.label == table.name && node.kind() == Some(NodeKind::Table)),
        "{nodes:?}"
    );
    conformance::check_invalid_path(&mut connection).await;
    table.drop().await;
}

#[tokio::test]
async fn reads_each_attribute_type_as_extended_json_with_the_keys_first() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    let table = TestTable::create(&connection, "types", true).await;
    let mut item = keyed("a", 1);
    item.extend([
        ("text".to_owned(), AttributeValue::S("it's".into())),
        (
            "amount".to_owned(),
            AttributeValue::N("12345678901234567890123456789.10".into()),
        ),
        ("flag".to_owned(), AttributeValue::Bool(false)),
        ("nothing".to_owned(), AttributeValue::Null(true)),
        (
            "bytes".to_owned(),
            AttributeValue::B(Blob::new(vec![0, 1, 2])),
        ),
        (
            "list".to_owned(),
            AttributeValue::L(vec![AttributeValue::S("x".into())]),
        ),
        (
            "map".to_owned(),
            AttributeValue::M(HashMap::from([(
                "inner".to_owned(),
                AttributeValue::N("2".into()),
            )])),
        ),
        ("tags".to_owned(), AttributeValue::Ss(vec!["blue".into()])),
        ("sizes".to_owned(), AttributeValue::Ns(vec!["10".into()])),
        (
            "blobs".to_owned(),
            AttributeValue::Bs(vec![Blob::new(vec![255])]),
        ),
    ]);
    table.put(vec![item]).await;
    let (shape, rows) = run(&mut connection, &partiql::select_all(&table.name, None)).await;
    assert_eq!(shape, ResultShape::Documents);
    // DynamoDB keeps a number's digits, all 38 of them, but not a trailing zero: `.10` comes
    // back `.1`, and is shown as it comes.
    assert_eq!(
        documents(&rows),
        [
            r#"{"pk":"a","sk":1,"amount":12345678901234567890123456789.1,"blobs":{"$binarySet":["/w=="]},"bytes":{"$binary":{"base64":"AAEC","subType":"00"}},"flag":false,"list":["x"],"map":{"inner":2},"nothing":null,"sizes":{"$numberSet":[10]},"tags":{"$stringSet":["blue"]},"text":"it's"}"#
        ]
    );
    let document = Value::parse(&documents(&rows)[0]).expect("a document");
    assert!(matches!(document, Value::Object(_)));
    table.drop().await;
}

#[tokio::test]
async fn pages_a_partition_past_500_items() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    let table = TestTable::create(&connection, "paged", true).await;
    table
        .put((0..1_200).map(|sk| keyed("p", sk)).collect())
        .await;
    table.put(vec![keyed("other", 0)]).await;
    let statement = format!("SELECT * FROM \"{}\" WHERE pk = 'p'", table.name);
    let mut cursor = connection.execute(&statement).await.expect("execute");
    let mut pages = Vec::new();
    while !cursor.is_exhausted() {
        pages.push(cursor.next_page(500).await.expect("a page").len());
    }
    assert_eq!(pages, [500, 500, 200]);
    table.drop().await;
}

#[tokio::test]
async fn a_filter_that_keeps_little_shows_short_pages_rather_than_reading_on() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    connection.bound = PageBound {
        items_per_request: 10,
        requests_per_page: 3,
    };
    let table = TestTable::create(&connection, "bounded", true).await;
    table.put((0..100).map(|sk| keyed("p", sk)).collect()).await;
    let statement = format!("SELECT * FROM \"{}\" WHERE sk = 95", table.name);
    let mut cursor = connection.execute(&statement).await.expect("execute");
    // Three requests of ten items evaluate 30 of the 100: no match yet, and more to read.
    let first = cursor.next_page(500).await.expect("a page");
    assert!(first.is_empty() && !cursor.is_exhausted());
    let mut found = Vec::new();
    while !cursor.is_exhausted() {
        found.extend(cursor.next_page(500).await.expect("a page"));
    }
    assert_eq!(found.len(), 1);
    table.drop().await;
}

#[tokio::test]
async fn writes_answer_nothing_unless_they_return_items() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    let table = TestTable::create(&connection, "writes", false).await;
    let name = &table.name;
    let insert = format!("INSERT INTO \"{name}\" VALUE {{'pk': 'a', 'n': 1, 'tags': <<'x'>>}}");
    let cursor = connection.execute(&insert).await.expect("insert");
    assert!(cursor.columns().is_empty());
    let update = format!("UPDATE \"{name}\" SET n = 2 WHERE pk = 'a' RETURNING ALL NEW *");
    let (_, rows) = run(&mut connection, &update).await;
    assert_eq!(
        documents(&rows),
        [r#"{"pk":"a","n":2,"tags":{"$stringSet":["x"]}}"#]
    );
    let delete = format!("DELETE FROM \"{name}\" WHERE pk = 'a'");
    let cursor = connection.execute(&delete).await.expect("delete");
    assert!(cursor.columns().is_empty());
    let (_, rows) = run(&mut connection, &partiql::select_all(name, None)).await;
    assert!(rows.is_empty());
    table.drop().await;
}

#[tokio::test]
async fn reports_what_it_cannot_read_and_what_dynamodb_refuses() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    let unread = connection.execute("SELECT * FROM t WHERE a = 'b").await;
    assert!(
        matches!(
            unread,
            Err(DriverError::Statement {
                position: Some(26),
                ..
            })
        ),
        "{:?}",
        unread.err()
    );
    let missing = connection
        .execute("SELECT * FROM \"no-such-table\"")
        .await
        .err()
        .map(|error| describe_error(&error));
    assert!(
        missing
            .as_deref()
            .is_some_and(|message| message.contains("ResourceNotFound")),
        "{missing:?}"
    );
    let rejection = connection.validate("UPDATE").await.expect("validate");
    assert!(rejection.is_some_and(|rejection| rejection.kind == RejectionKind::Syntax));
}

/// DynamoDB Local answers too quickly to be stopped part-way, so a server that takes requests
/// and never answers stands in for a slow one.
#[tokio::test]
async fn cancel_drops_the_request_in_flight() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let address = listener.local_addr().expect("its address");
    let silent = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let config = emulator(&format!("http://{address}"));
    let sdk_config = sign_in(&config).sdk_config().await.expect("sign in");
    let running = Arc::new(Running::default());
    let handle = DynamoCancel(Arc::clone(&running));
    let started = Instant::now();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.cancel().await
    });
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        ItemCursor::start(
            Client::new(&sdk_config),
            running,
            PageBound::DEFAULT,
            "SELECT * FROM t",
            Vec::new(),
            true,
        ),
    )
    .await
    .expect("stopped before the timeout");
    assert!(matches!(result, Err(DriverError::Statement { .. })));
    assert!(started.elapsed() < Duration::from_secs(5));
    canceller.await.expect("cancel task").expect("cancel");
    silent.abort();
}
