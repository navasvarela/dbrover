use std::path::{Path, PathBuf};

use dbrover_core::conformance;
use dbrover_core::{
    ChildrenQuery, MongoUri, NodeKind, Port, ResultShape, Row, ServerConfig, TlsMode,
};

use super::*;

/// Point the tests at a server, e.g. `mongodb://dbrover:dbrover@localhost:57017/dbrover` from
/// the repository's `docker-compose.yml` (see `test-servers.env`). Skipped when unset, so
/// `cargo test` works offline. The server serves TLS and plain connections on the same port.
const TEST_URL_VAR: &str = "DBROVER_TEST_MONGODB_URL";

struct TestServer {
    config: MongoConfig,
    password: Option<Password>,
}

impl TestServer {
    /// The server, with `database` as the console's, emptied first so each test starts clean.
    async fn connect_to(&self, database: &str) -> MongoConnection {
        let mut config = self.config.clone();
        database.clone_into(&mut config.server.database);
        let mut connection = connect(&config, self.password.as_ref(), Route::Direct)
            .await
            .unwrap_or_else(|error| panic!("connect: {}", describe_error(&error)));
        run(&mut connection, "db.dropDatabase()").await;
        connection
    }

    async fn connect_with(&self, tls: TlsMode) -> Result<MongoConnection, DriverError> {
        let mut config = self.config.clone();
        config.server.tls = tls;
        connect(&config, self.password.as_ref(), Route::Direct).await
    }
}

fn test_server() -> Option<TestServer> {
    let Ok(url) = std::env::var(TEST_URL_VAR) else {
        eprintln!("skipping: {TEST_URL_VAR} is not set");
        return None;
    };
    let uri = MongoUri::parse(&url).expect("a mongodb:// URL");
    let port = url
        .rsplit_once(':')
        .and_then(|(_, rest)| rest.split('/').next()?.parse::<u16>().ok())
        .expect("host:port in the URL");
    Some(TestServer {
        config: MongoConfig {
            server: ServerConfig {
                host: uri.host,
                port: Port::from(port),
                database: uri.database.unwrap_or_else(|| "dbrover".to_owned()),
                user: uri.user,
                tls: TlsMode::Disable,
                ssh: None,
                password_command: None,
            },
            auth_database: String::new(),
            connection_string: None,
        },
        password: uri.password.map(Password::new),
    })
}

/// The authority that signed the TLS test server's certificate.
fn test_ca_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/postgres-tls/ca.crt")
}

/// Runs `command` to its end and returns its rows.
async fn rows(connection: &mut MongoConnection, command: &str) -> Vec<Row> {
    let mut cursor = connection
        .execute(command)
        .await
        .unwrap_or_else(|error| panic!("{command}: {}", describe_error(&error)));
    let mut rows = Vec::new();
    while !cursor.is_exhausted() {
        rows.extend(cursor.next_page(500).await.expect("a page"));
    }
    rows
}

async fn run(connection: &mut MongoConnection, command: &str) {
    rows(connection, command).await;
}

/// The single cell `command` answers with.
async fn value(connection: &mut MongoConnection, command: &str) -> Option<String> {
    let rows = rows(connection, command).await;
    let [row] = rows.as_slice() else {
        panic!("{command}: expected one row, got {rows:?}");
    };
    row[0].clone()
}

/// `count` documents `{n: 0}` to `{n: count - 1}` in `collection`.
async fn fill(connection: &mut MongoConnection, collection: &str, count: usize) {
    let documents: Vec<String> = (0..count).map(|n| format!("{{n: {n}}}")).collect();
    run(
        connection,
        &format!("db.{collection}.insertMany([{}])", documents.join(", ")),
    )
    .await;
}

#[tokio::test]
async fn a_connection_reports_its_server_and_lists_the_databases() {
    let Some(server) = test_server() else {
        return;
    };
    let mut connection = server.connect_to("dbrover_catalog").await;
    run(&mut connection, "db.things.insertOne({a: 1})").await;
    assert_eq!(connection.server_info().product, "MongoDB");
    // CI's server is 7.0 (see docker-compose.yml); either reports its version.
    let version = &connection.server_info().version;
    assert!(
        version.starts_with('7') || version.starts_with('8'),
        "{version}"
    );
    let nodes = conformance::walk_catalog(&mut connection, 1).await;
    let databases: Vec<(&str, bool)> = nodes
        .iter()
        .map(|node| (node.label.as_str(), node.system))
        .collect();
    assert!(databases.contains(&("admin", true)), "{databases:?}");
    assert!(
        databases.contains(&("dbrover_catalog", false)),
        "{databases:?}"
    );
    conformance::check_invalid_path(&mut connection).await;
    conformance::check_lifecycle(Box::new(connection)).await;
}

#[tokio::test]
async fn a_wrong_password_fails_to_connect() {
    let Some(server) = test_server() else {
        return;
    };
    conformance::check_connect_failure(
        connect(&server.config, Some(&Password::new("wrong")), Route::Direct).await,
    );
}

/// A `find` and an `aggregate` page through 1,200 documents 500 at a time, as documents in
/// canonical Extended JSON, and know when they have no more.
#[tokio::test]
async fn pages_through_a_find_and_an_aggregate() {
    let Some(server) = test_server() else {
        return;
    };
    let mut connection = server.connect_to("dbrover_paging").await;
    fill(&mut connection, "numbers", 1200).await;
    for command in [
        "db.numbers.find({}, {_id: 0}).sort({n: 1})",
        "db.numbers.aggregate([{$sort: {n: 1}}, {$project: {_id: 0, n: 1}}])",
    ] {
        let mut cursor = connection.execute(command).await.expect(command);
        assert_eq!(cursor.shape(), ResultShape::Documents, "{command}");
        let mut pages = Vec::new();
        while !cursor.is_exhausted() {
            pages.push(cursor.next_page(500).await.expect("a page"));
        }
        let sizes: Vec<usize> = pages.iter().map(Vec::len).collect();
        assert_eq!(sizes, [500, 500, 200], "{command}");
        assert_eq!(
            pages[1][0][0].as_deref(),
            Some(r#"{"n":{"$numberInt":"500"}}"#),
            "{command}"
        );
        assert_eq!(cursor.finish().await.expect("finish"), None);
    }
}

/// A cursor left before its end leaves the connection usable: its task ends, and the driver
/// closes the cursor on the server.
#[tokio::test]
async fn a_cursor_can_be_left_halfway() {
    let Some(server) = test_server() else {
        return;
    };
    let mut connection = server.connect_to("dbrover_halfway").await;
    fill(&mut connection, "numbers", 1200).await;
    let mut cursor = connection.execute("db.numbers.find()").await.expect("find");
    assert_eq!(cursor.next_page(500).await.expect("a page").len(), 500);
    drop(cursor);
    assert_eq!(
        value(&mut connection, "db.numbers.countDocuments()").await,
        Some("1200".to_owned())
    );
}

/// Each method's result as the shell shows it: counts and distinct values in columns, writes
/// as what they changed, and the rest as documents.
#[tokio::test]
async fn shows_each_methods_result() {
    let Some(server) = test_server() else {
        return;
    };
    let mut connection = server.connect_to("dbrover_results").await;
    let inserted = value(
        &mut connection,
        "db.orders.insertOne({_id: 1, status: 'open', qty: 5})",
    )
    .await;
    assert_eq!(
        inserted.as_deref(),
        Some(r#"{"acknowledged":true,"insertedId":{"$numberInt":"1"}}"#)
    );
    run(
        &mut connection,
        "db.orders.insertMany([{_id: 2, status: 'open'}, {_id: 3, status: 'closed'}])",
    )
    .await;
    for (command, expected) in [
        ("db.orders.countDocuments({status: 'open'})", Some("2")),
        ("db.orders.estimatedDocumentCount()", Some("3")),
        ("db.getName()", Some("dbrover_results")),
    ] {
        assert_eq!(
            value(&mut connection, command).await.as_deref(),
            expected,
            "{command}"
        );
    }
    assert_eq!(
        rows(&mut connection, "db.orders.distinct('status')").await,
        [[Some("closed".to_owned())], [Some("open".to_owned())]]
    );
    let mut update = connection
        .execute("db.orders.updateMany({status: 'open'}, {$set: {status: 'paid'}})")
        .await
        .expect("updateMany");
    let page = update.next_page(500).await.expect("its answer");
    assert_eq!(
        page[0][0].as_deref(),
        Some(concat!(
            r#"{"acknowledged":true,"matchedCount":{"$numberInt":"2"},"#,
            r#""modifiedCount":{"$numberInt":"2"},"upsertedCount":{"$numberInt":"0"}}"#
        ))
    );
    assert_eq!(update.finish().await.expect("finish"), Some(2));
    assert_eq!(
        value(&mut connection, "db.orders.findOne({_id: 3}, {_id: 0})")
            .await
            .as_deref(),
        Some(r#"{"status":"closed"}"#)
    );
    assert_eq!(
        rows(&mut connection, "show collections").await,
        [[Some("orders".to_owned())]]
    );
    let explained = value(&mut connection, "db.orders.find({qty: 5}).explain()").await;
    assert!(
        explained.is_some_and(|plan| plan.contains("queryPlanner")),
        "explain shows a plan"
    );
    let duplicate = connection.execute("db.orders.insertOne({_id: 1})").await;
    assert!(
        matches!(&duplicate, Err(DriverError::Statement { source, .. }) if source.to_string().contains("E11000")),
        "a duplicate key is the command's error"
    );
    run(&mut connection, "use dbrover_results_other").await;
    assert_eq!(
        value(&mut connection, "db.getName()").await.as_deref(),
        Some("dbrover_results_other")
    );
}

/// What a document holds comes back with every type the shell wrote: a value read from the
/// server and written as canonical Extended JSON is the same value.
#[tokio::test]
async fn every_bson_type_comes_back_as_written() {
    let Some(server) = test_server() else {
        return;
    };
    let mut connection = server.connect_to("dbrover_types").await;
    let fields = "_id: ObjectId('507f1f77bcf86cd799439011'), at: ISODate('2026-10-31T09:30:00.123Z'), \
                  long: NumberLong('9007199254740993'), int: 7, double: 1.5, dec: NumberDecimal('1.10'), \
                  uuid: UUID('0e3e1d4b-6c5d-4f6e-8a1b-2c3d4e5f6a7b'), bin: BinData(0, 'AQID'), \
                  ts: Timestamp(1, 2), min: MinKey, max: MaxKey, re: /^a/i, nothing: null, \
                  nested: {list: [true, 'x', {deep: NumberInt(1)}]}";
    run(
        &mut connection,
        &format!("db.types.insertOne({{{fields}}})"),
    )
    .await;
    let stored = value(&mut connection, "db.types.findOne()")
        .await
        .expect("the document");
    let Ok(Command::Call(call)) = mongo::parse(&format!("db.c.insertOne({{{fields}}})")) else {
        panic!("the fields parse");
    };
    let written = convert::to_bson(&call.method.arguments[0]).expect("as BSON");
    assert_eq!(stored, written.into_canonical_extjson().to_string());
}

/// ⌘. stops a `find` and an aggregation the server would run for minutes, and the connection
/// stays usable: the run's comment finds them in `$currentOp`, and `killOp` ends them.
#[tokio::test]
async fn cancels_a_slow_find_and_a_slow_aggregation() {
    let Some(server) = test_server() else {
        return;
    };
    let mut connection = server.connect_to("dbrover_cancel").await;
    fill(&mut connection, "slow", 3).await;
    conformance::check_cancel(
        &mut connection,
        "db.slow.find({$where: 'sleep(100000) || true'})",
    )
    .await;
    conformance::check_cancel(
        &mut connection,
        "db.slow.aggregate([{$match: {$expr: {$function: {body: 'function() { sleep(100000); return true }', args: [], lang: 'js'}}}}])",
    )
    .await;
}

#[tokio::test]
async fn connects_over_tls_checking_the_certificate() {
    let Some(server) = test_server() else {
        return;
    };
    let ca_file = Some(test_ca_file());
    let mut verified = server
        .connect_with(TlsMode::VerifyFull { ca_file })
        .await
        .unwrap_or_else(|error| panic!("verify-full: {}", describe_error(&error)));
    verified.ping().await.expect("ping over TLS");
    server
        .connect_with(TlsMode::Require)
        .await
        .unwrap_or_else(|error| panic!("require: {}", describe_error(&error)));
    // The public authorities didn't sign the test server's certificate.
    conformance::check_connect_failure(
        server
            .connect_with(TlsMode::VerifyFull { ca_file: None })
            .await,
    );
}

#[tokio::test]
async fn connects_through_a_tunnel() {
    let Some(server) = test_server() else {
        return;
    };
    let address = format!(
        "{}:{}",
        server.config.server.host, server.config.server.port
    );
    let local = conformance::forwarder(&address).await;
    let mut tunnelled = connect(
        &server.config,
        server.password.as_ref(),
        Route::Tunnel(local),
    )
    .await
    .unwrap_or_else(|error| panic!("through the tunnel: {}", describe_error(&error)));
    tunnelled.ping().await.expect("ping through the tunnel");
    // The driver would check the certificate against the tunnel's end, so it isn't offered.
    let mut config = server.config.clone();
    config.server.tls = TlsMode::VerifyFull {
        ca_file: Some(test_ca_file()),
    };
    conformance::check_connect_failure(
        connect(&config, server.password.as_ref(), Route::Tunnel(local)).await,
    );
}

/// A connection string connects with the data source's own user and password, which DbRover
/// keeps out of the string; the form's host and port aren't used.
#[tokio::test]
async fn connects_with_a_connection_string() {
    let Some(server) = test_server() else {
        return;
    };
    let mut config = server.config.clone();
    config.connection_string = Some(format!(
        "mongodb://{}:{}/?directConnection=true",
        config.server.host, config.server.port
    ));
    "nowhere".clone_into(&mut config.server.host);
    config.server.port = Port::from(1);
    let mut connection = connect(&config, server.password.as_ref(), Route::Direct)
        .await
        .unwrap_or_else(|error| panic!("connection string: {}", describe_error(&error)));
    connection.ping().await.expect("ping");
}

#[tokio::test]
async fn points_at_what_it_cannot_read() {
    let Some(server) = test_server() else {
        return;
    };
    let mut connection = server.connect_to("dbrover_errors").await;
    let error = connection.execute("db.orders.find({qty: limit})").await;
    assert!(
        matches!(
            error,
            Err(DriverError::Statement {
                position: Some(21),
                ..
            })
        ),
        "{:?}",
        error.err()
    );
    assert_eq!(
        connection.validate("db.orders.find({a: 1})").await.ok(),
        Some(None)
    );
}

/// The explorer: each database's collections, views and time-series collections, a
/// collection's indexes, and the commands that make each again.
#[tokio::test]
async fn browses_collections_views_and_indexes() {
    let Some(server) = test_server() else {
        return;
    };
    let mut connection = server.connect_to("dbrover_explorer").await;
    run(
        &mut connection,
        "db.createCollection('orders', {validator: {$jsonSchema: {required: ['status']}}})",
    )
    .await;
    run(
        &mut connection,
        "db.orders.insertMany([{status: 'open', created: 1}, {status: 'paid', created: 2}, {status: 'open', created: 3}])",
    )
    .await;
    run(
        &mut connection,
        "db.orders.createIndex({status: 1, created: -1}, {unique: true, name: 'status_created'})",
    )
    .await;
    run(
        &mut connection,
        "db.createView('open_orders', 'orders', [{$match: {status: 'open'}}])",
    )
    .await;
    run(
        &mut connection,
        "db.createCollection('readings', {timeseries: {timeField: 'at'}})",
    )
    .await;

    let database = CatalogPath::root().child(NodeKind::Database, "dbrover_explorer");
    let collections = connection.children(&database).await.expect("collections");
    let shown: Vec<(&str, Option<&str>, bool)> = collections
        .iter()
        .filter(|node| !node.system)
        .map(|node| {
            (
                node.label.as_str(),
                node.detail.as_deref(),
                node.has_children,
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("open_orders", Some("view"), false),
            ("orders", Some("~3 documents"), true),
            ("readings", Some("time series"), true),
        ]
    );
    let orders = database.child(NodeKind::Collection, "orders");
    let indexes = connection.children(&orders).await.expect("indexes");
    let shown: Vec<(&str, Option<&str>)> = indexes
        .iter()
        .map(|node| (node.label.as_str(), node.detail.as_deref()))
        .collect();
    assert_eq!(
        shown,
        [
            ("_id_", Some("{_id: 1}")),
            ("status_created", Some("{status: 1, created: -1} · unique")),
        ]
    );
    let definition = connection.definition(&orders).await.expect("definition");
    assert_eq!(
        definition,
        concat!(
            "db.createCollection(\"orders\", {validator: {$jsonSchema: {required: [\"status\"]}}});\n",
            "db.getCollection(\"orders\").createIndex({status: 1, created: -1}, ",
            "{name: \"status_created\", unique: true});\n",
        )
    );
    let view = connection
        .definition(&database.child(NodeKind::Collection, "open_orders"))
        .await
        .expect("the view's definition");
    assert_eq!(
        view,
        "db.createView(\"open_orders\", \"orders\", [{$match: {status: \"open\"}}]);\n"
    );
    // What Show definition gives runs again as written.
    run(&mut connection, "db.dropDatabase()").await;
    for command in definition.lines().chain(view.lines()) {
        run(&mut connection, command.trim_end_matches(';')).await;
    }
    assert_eq!(
        connection.definition(&orders).await.expect("made again"),
        definition
    );
    let nodes = conformance::walk_catalog(&mut connection, 3).await;
    assert!(
        nodes
            .iter()
            .any(|node| node.path.last().map(|last| last.kind) == Some(NodeKind::Index)),
        "the walk reaches the indexes"
    );
}

/// A database of 1,100 collections lists 1,000 at a time, by name, with the filter applied by
/// the server.
#[tokio::test]
async fn pages_through_a_database_of_many_collections() {
    let Some(server) = test_server() else {
        return;
    };
    let connection = server.connect_to("dbrover_many").await;
    let database = connection.client.database("dbrover_many");
    let mut creating = tokio::task::JoinSet::new();
    for number in 0..1_100 {
        let database = database.clone();
        creating.spawn(async move {
            database
                .run_command(doc! { "create": format!("Collection_{number:04}") })
                .await
        });
        if creating.len() >= 16 {
            creating
                .join_next()
                .await
                .expect("a task")
                .expect("joined")
                .expect("created");
        }
    }
    while let Some(created) = creating.join_next().await {
        created.expect("joined").expect("created");
    }
    let mut connection: Box<dyn Connection> = Box::new(connection);
    let path = CatalogPath::root().child(NodeKind::Database, "dbrover_many");
    let first = connection
        .children_page(&path, &ChildrenQuery::first_page(None))
        .await
        .expect("the first page");
    assert_eq!(first.nodes.len(), 1_000);
    assert_eq!(first.nodes[999].label, "Collection_0999");
    let next = first.next.expect("a second page");
    let second = connection
        .children_page(&path, &ChildrenQuery::first_page(None).after(next))
        .await
        .expect("the second page");
    assert_eq!(second.nodes.len(), 100);
    assert_eq!(second.next, None);
    let filtered = connection
        .children_page(
            &path,
            &ChildrenQuery::first_page(Some("collection_105".to_owned())),
        )
        .await
        .expect("filtered");
    let labels: Vec<&str> = filtered
        .nodes
        .iter()
        .map(|node| node.label.as_str())
        .collect();
    assert_eq!(
        labels,
        (1050..1060)
            .map(|n| format!("Collection_{n}"))
            .collect::<Vec<_>>()
    );
}

/// ⌥⌘E's command shows the plan with what each stage did, and a read's mistakes are caught by
/// planning it, before it runs.
#[tokio::test]
async fn explains_and_validates_reads_on_the_server() {
    let Some(server) = test_server() else {
        return;
    };
    let mut connection = server.connect_to("dbrover_explain").await;
    fill(&mut connection, "numbers", 10).await;
    for command in [
        "db.numbers.find({n: {$gt: 4}}).sort({n: 1})",
        "db.numbers.aggregate([{$match: {n: {$gt: 4}}}, {$group: {_id: null, total: {$sum: '$n'}}}])",
    ] {
        let explained = dbrover_language::QueryLanguage::Mongo
            .explain(command, None, dbrover_language::ExplainDepth::Estimated)
            .expect("an explained command");
        let plan = value(&mut connection, &explained.text)
            .await
            .expect("a plan");
        assert!(plan.contains("executionStats"), "{command}: {plan}");
    }
    let unknown = connection
        .validate("db.numbers.find({n: {$gtx: 1}})")
        .await
        .expect("validated");
    assert!(
        unknown.is_some_and(|rejection| rejection.message.contains("$gtx")),
        "an unknown operator is caught"
    );
    let pipeline = connection
        .validate("db.numbers.aggregate([{$nosuchstage: {}}])")
        .await
        .expect("validated");
    assert!(pipeline.is_some(), "an unknown stage is caught");
    assert_eq!(
        connection
            .validate("db.numbers.find({n: {$gt: 1}})")
            .await
            .expect("validated"),
        None
    );
    // A write is only read: nothing is sent, so nothing is deleted.
    assert_eq!(
        connection
            .validate("db.numbers.deleteMany({})")
            .await
            .expect("validated"),
        None
    );
    assert_eq!(
        value(&mut connection, "db.numbers.countDocuments()").await,
        Some("10".to_owned())
    );
}

/// The grid's edits as the console writes them: each field set with the type it had, a field
/// inside an object by its path, a document added and deleted, all read back as the server
/// stored them (`docs/nosql.md`, N2·6).
#[tokio::test]
async fn edits_keep_each_fields_type() {
    use dbrover_document::{Path, Step, Value as Document};
    use dbrover_language::mongo::{CollectionView, document_id, typed_value, update_path};

    let Some(server) = test_server() else {
        return;
    };
    let mut connection = server.connect_to("dbrover_edits").await;
    run(
        &mut connection,
        "db.orders.insertOne({_id: ObjectId('507f1f77bcf86cd799439011'), qty: NumberLong(7), \
         n: 1, price: 1.5, dec: NumberDecimal('1.10'), at: ISODate('2026-01-01T00:00:00Z'), \
         paid: false, note: 'x', address: {city: 'Paris', zip: '75001'}})",
    )
    .await;
    let view = CollectionView::of("db.orders.find({})").expect("a find");
    let read = |text: Option<String>| Document::parse(&text.expect("a document")).expect("JSON");
    let document = read(value(&mut connection, "db.orders.findOne()").await);
    let id = document_id(&document).expect("an _id");
    let field = |document: &Document, name: &str| -> Document {
        let Document::Object(fields) = document else {
            panic!("not an object");
        };
        fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.clone())
            .expect("the field")
    };
    for (name, typed) in [
        ("qty", "8"),
        ("n", "2"),
        ("price", "3"),
        ("dec", "2.20"),
        ("at", "2026-10-31T09:30:00Z"),
        ("paid", "true"),
        ("note", "42"),
    ] {
        let old = field(&document, name);
        let new = typed_value(Some(&old), Some(typed)).expect("a value of the field's type");
        run(&mut connection, &view.update(&id, name, Some(&new))).await;
    }
    let city = update_path(&Path::from(vec![
        Step::Field("address".to_owned()),
        Step::Field("city".to_owned()),
    ]))
    .expect("a path");
    run(
        &mut connection,
        &view.update(&id, &city, Some("\"Madrid\"")),
    )
    .await;
    let stored = value(&mut connection, "db.orders.findOne({}, {_id: 0})")
        .await
        .expect("the document");
    assert_eq!(
        stored,
        concat!(
            r#"{"qty":{"$numberLong":"8"},"n":{"$numberInt":"2"},"price":{"$numberDouble":"3.0"},"#,
            r#""dec":{"$numberDecimal":"2.20"},"at":{"$date":{"$numberLong":"1793439000000"}},"#,
            r#""paid":true,"note":"42","address":{"city":"Madrid","zip":"75001"}}"#
        )
    );
    run(
        &mut connection,
        &view.insert("{_id: 2, qty: NumberLong(1)}"),
    )
    .await;
    run(&mut connection, &view.delete(&id)).await;
    assert_eq!(
        rows(&mut connection, "db.orders.find({}, {_id: 1})").await,
        [[Some(r#"{"_id":{"$numberInt":"2"}}"#.to_owned())]]
    );
}
