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
        cost_guard: dbrover_core::CostGuard::AskBeforeScans,
    }
}

async fn connected(config: &DynamoDbConfig) -> DynamoConnection {
    connect(config)
        .await
        .unwrap_or_else(|error| panic!("connect: {}", describe_error(&error)))
}

/// Held to drop a table (shared) or to walk every table of the server (alone): the tests run
/// at once, and a walk describes each table it lists, which one dropped in between fails.
static CATALOG: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

/// A name for a test's table, unique to the run, so tests running at once and runs one after
/// another never meet.
fn unique(test: &str) -> String {
    format!("{test}-{}", uuid::Uuid::new_v4().simple())
}

/// A table of the tests' own, dropped when the test ends so the server keeps nothing between
/// runs.
struct TestTable {
    client: Client,
    name: String,
}

impl TestTable {
    /// A table named `name`, keyed by `pk` (a string) and, when `sort` is set, `sk` (a number).
    async fn create(connection: &DynamoConnection, name: String, sort: bool) -> Self {
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
        let _dropping = CATALOG.read().await;
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
    let table = TestTable::create(&connection, unique("listed"), false).await;
    let nodes = {
        let _walking = CATALOG.write().await;
        conformance::walk_catalog(&mut connection, 2).await
    };
    assert!(
        nodes
            .iter()
            .any(|node| node.label == table.name && node.kind() == Some(NodeKind::Table)),
        "{nodes:?}"
    );
    conformance::check_invalid_path(&mut connection).await;
    table.drop().await;
}

/// A table keyed by `customer` and `placed`, with a global index by `status` and a local one
/// by `total`: every shape of key the explorer shows.
async fn create_indexed_table(connection: &DynamoConnection, name: &str) {
    use aws_sdk_dynamodb::types::{
        GlobalSecondaryIndex, LocalSecondaryIndex, Projection, ProjectionType,
    };
    let projection = |kind: ProjectionType, attributes: &[&str]| {
        let mut projection = Projection::builder().projection_type(kind);
        for attribute in attributes {
            projection = projection.non_key_attributes(*attribute);
        }
        projection.build()
    };
    connection
        .client
        .create_table()
        .table_name(name)
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(key("customer", KeyType::Hash))
        .key_schema(key("placed", KeyType::Range))
        .attribute_definitions(attribute("customer", ScalarAttributeType::S))
        .attribute_definitions(attribute("placed", ScalarAttributeType::S))
        .attribute_definitions(attribute("status", ScalarAttributeType::S))
        .attribute_definitions(attribute("total", ScalarAttributeType::N))
        .global_secondary_indexes(
            GlobalSecondaryIndex::builder()
                .index_name("by-status")
                .key_schema(key("status", KeyType::Hash))
                .key_schema(key("placed", KeyType::Range))
                .projection(projection(ProjectionType::Include, &["total"]))
                .build()
                .expect("an index"),
        )
        .local_secondary_indexes(
            LocalSecondaryIndex::builder()
                .index_name("by-total")
                .key_schema(key("customer", KeyType::Hash))
                .key_schema(key("total", KeyType::Range))
                .projection(projection(ProjectionType::KeysOnly, &[]))
                .build()
                .expect("an index"),
        )
        .send()
        .await
        .expect("create the table");
}

#[tokio::test]
async fn browses_a_tables_keys_and_indexes_and_shows_its_definition() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    let name = unique("indexed");
    create_indexed_table(&connection, &name).await;
    let table = TestTable {
        client: connection.client.clone(),
        name,
    };
    let root = CatalogPath::root();
    let tables = {
        let _walking = CATALOG.write().await;
        connection.children(&root).await.expect("the tables")
    };
    let node = tables
        .iter()
        .find(|node| node.label == table.name)
        .expect("the table is listed");
    assert_eq!(
        node.detail.as_deref(),
        Some("\u{2248} 0 items \u{b7} 0.0 kB")
    );
    let shown = |nodes: Vec<CatalogNode>| -> Vec<(String, Option<String>)> {
        nodes
            .into_iter()
            .map(|node| (node.label, node.detail))
            .collect()
    };
    let children = connection.children(&node.path).await.expect("its keys");
    let folder = children.last().expect("an indexes folder").path.clone();
    assert_eq!(
        shown(children),
        [
            (
                "customer".to_owned(),
                Some("partition key \u{b7} S".to_owned())
            ),
            ("placed".to_owned(), Some("sort key \u{b7} S".to_owned())),
            ("indexes".to_owned(), None),
        ]
    );
    let indexes = connection.children(&folder).await.expect("its indexes");
    assert_eq!(
        shown(indexes),
        [
            (
                "by-status".to_owned(),
                Some("global \u{b7} status, placed".to_owned())
            ),
            (
                "by-total".to_owned(),
                Some("local \u{b7} customer, total".to_owned())
            ),
        ]
    );
    let definition = connection
        .definition(&node.path)
        .await
        .expect("a definition");
    let expected = format!(
        r#"{{
  "TableName": "{}",
  "KeySchema": [
    {{
      "AttributeName": "customer",
      "KeyType": "HASH"
    }},
    {{
      "AttributeName": "placed",
      "KeyType": "RANGE"
    }}
  ],"#,
        table.name
    );
    assert!(definition.starts_with(&expected), "{definition}");
    for part in [
        r#""BillingMode": "PAY_PER_REQUEST""#,
        r#""IndexName": "by-status""#,
        r#""ProjectionType": "INCLUDE""#,
        r#""NonKeyAttributes""#,
        r#""IndexName": "by-total""#,
        r#""ProjectionType": "KEYS_ONLY""#,
    ] {
        assert!(definition.contains(part), "{part} in {definition}");
    }
    assert!(Value::parse(&definition).is_ok());
    let nodes = {
        let _walking = CATALOG.write().await;
        conformance::walk_catalog(&mut connection, 3).await
    };
    assert!(nodes.iter().any(|node| node.label == "by-total"));
    table.drop().await;
}

#[tokio::test]
async fn pages_tables_by_name_a_hundred_at_a_time_with_the_filter() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    let prefix = unique("paged-tables");
    let mut tables = Vec::new();
    for number in 0..105 {
        let name = format!("{prefix}-{number:03}");
        tables.push(TestTable::create(&connection, name, false).await);
    }
    // Other tests' tables are in the same server, so every page is filtered by the prefix,
    // in capitals: the filter ignores case.
    let mut query = ChildrenQuery {
        filter: Some(prefix.to_uppercase()),
        after: None,
        limit: Some(1_000),
    };
    let mut pages = Vec::new();
    loop {
        let page = connection
            .children_page(&CatalogPath::root(), &query)
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
    assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [100, 5]);
    let listed: Vec<String> = pages.concat();
    let mut names: Vec<String> = tables.iter().map(|table| table.name.clone()).collect();
    names.sort();
    assert_eq!(listed, names);
    for table in tables {
        table.drop().await;
    }
}

/// The scan guard's question, answered from each table's or index's own partition key.
#[tokio::test]
async fn tells_a_scan_from_a_read_by_key_with_the_keys_of_what_it_reads() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    let name = unique("guarded");
    create_indexed_table(&connection, &name).await;
    let table = TestTable {
        client: connection.client.clone(),
        name,
    };
    let name = &table.name;
    let scan = |index: Option<&str>, partition_key: &str| {
        Some(FullScan {
            table: name.clone(),
            index: index.map(str::to_owned),
            partition_key: partition_key.to_owned(),
        })
    };
    let cases = [
        (
            format!("SELECT * FROM \"{name}\" WHERE customer = 'c'"),
            None,
        ),
        (
            format!("SELECT * FROM \"{name}\" WHERE customer IN ['a', 'b'] AND placed > '2026'"),
            None,
        ),
        (
            format!("SELECT * FROM \"{name}\" WHERE status = 'open'"),
            scan(None, "customer"),
        ),
        (format!("SELECT * FROM \"{name}\""), scan(None, "customer")),
        (
            format!("SELECT * FROM \"{name}\".\"by-status\" WHERE status = 'open'"),
            None,
        ),
        (
            format!("SELECT * FROM \"{name}\".\"by-status\" WHERE customer = 'c'"),
            scan(Some("by-status"), "status"),
        ),
        (
            format!("UPDATE \"{name}\" SET n = 1 WHERE customer = 'c' AND placed = 'p'"),
            None,
        ),
        // DynamoDB's own error says more when it runs.
        ("SELECT * FROM \"no-such-table\"".to_owned(), None),
        ("SELECT * FROM".to_owned(), None),
    ];
    for (statement, expected) in cases {
        let answer = connection.full_scan(&statement).await.expect("an answer");
        assert_eq!(answer, expected, "{statement}");
    }
    table.drop().await;
}

#[tokio::test]
async fn reads_each_attribute_type_as_extended_json_with_the_keys_first() {
    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    let table = TestTable::create(&connection, unique("types"), true).await;
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
    let table = TestTable::create(&connection, unique("paged"), true).await;
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
    let table = TestTable::create(&connection, unique("bounded"), true).await;
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
    let table = TestTable::create(&connection, unique("writes"), false).await;
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

/// N5·7: what the grid's edits write runs on DynamoDB, and each attribute reads back with
/// the type it had; an item deleted comes back whole with its undo, and one added is there.
#[tokio::test]
async fn edits_written_by_the_table_view_keep_each_attributes_type() {
    use dbrover_document::{Path, Step};

    let Some(config) = test_config() else { return };
    let mut connection = connected(&config).await;
    let table = TestTable::create(&connection, unique("edits"), true).await;
    let select = format!("SELECT * FROM \"{}\" WHERE pk = 'a'", table.name);
    let insert = format!(
        "INSERT INTO \"{}\" VALUE {{'pk': 'a', 'sk': 1, 'n': 1, 's': 'x', 'b': true, \
         'm': {{'city': 'Leeds'}}, 'l': [1, 'two'], 'ss': <<'p'>>, 'ns': <<1>>}}",
        table.name
    );
    run(&mut connection, &insert).await;
    let read = |rows: Vec<Row>| {
        documents(&rows)
            .into_iter()
            .next()
            .map(|text| Value::parse(&text).unwrap())
    };
    let view = dbrover_language::QueryLanguage::PartiQl
        .described_view(&select, &["pk".to_owned(), "sk".to_owned()])
        .expect("the items of one table are editable");
    let path = |names: &[&str]| {
        names.iter().fold(Path::default(), |path, name| {
            path.child(Step::Field((*name).to_owned()))
        })
    };
    let field = |document: &Value, name: &str| match document {
        Value::Object(fields) => fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.clone()),
        _ => None,
    };
    let cases: [(&[&str], &str); 7] = [
        (&["n"], "2.5"),
        (&["s"], "it's"),
        (&["b"], "false"),
        (&["m", "city"], "York"),
        (&["l"], r#"[3, "four"]"#),
        (&["ss"], r#"["q", "r"]"#),
        (&["ns"], "[7, 8]"),
    ];
    for (names, typed) in cases {
        let (_, rows) = run(&mut connection, &select).await;
        let item = read(rows).expect("the item");
        let top = field(&item, names[0]).expect("the attribute");
        let old = match names {
            [_, inner] => field(&top, inner),
            _ => Some(top),
        };
        let (edit, _) = view
            .update(&item, &path(names), old.as_ref(), Some(typed))
            .expect("an edit");
        run(&mut connection, &edit.command).await;
        let (_, rows) = run(&mut connection, &select).await;
        let changed = read(rows).expect("the item");
        let value = field(&changed, names[0]).expect("the attribute");
        let value = match names {
            [_, inner] => field(&value, inner).expect("the inner attribute"),
            _ => value,
        };
        let expected = dbrover_language::partiql::typed_value(old.as_ref(), Some(typed)).unwrap();
        assert_eq!(value, expected, "{names:?}: {}", edit.command);
    }
    let (_, rows) = run(&mut connection, &select).await;
    let item = read(rows).expect("the item");
    let delete = view.delete(&item).expect("a delete");
    run(&mut connection, &delete.command).await;
    let (_, rows) = run(&mut connection, &select).await;
    assert!(rows.is_empty());
    run(&mut connection, &delete.revert.expect("an undo")).await;
    let (_, rows) = run(&mut connection, &select).await;
    assert_eq!(read(rows), Some(item));
    let added = view
        .insert(&[("pk", Some("a")), ("sk", Some("2")), ("note", Some("new"))])
        .expect("an insert");
    run(&mut connection, &added).await;
    let (_, rows) = run(&mut connection, &select).await;
    assert_eq!(rows.len(), 2);
    let description = connection
        .describe_table(&dbrover_core::RelationName {
            schema: None,
            name: dbrover_core::Identifier {
                name: table.name.clone(),
                quoted: true,
            },
        })
        .await
        .expect("a description")
        .expect("the table");
    assert_eq!(description.primary_key, ["pk", "sk"]);
    table.drop().await;
}
