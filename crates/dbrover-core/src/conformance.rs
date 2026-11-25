//! Behaviour every driver must share, run by each driver crate against its own backend.
//! See `docs/testing.md`. These functions panic on failure, like any test assertion.

#![allow(clippy::expect_used)]

use std::time::{Duration, Instant};

use crate::{
    CatalogMatches, CatalogNode, CatalogPath, CatalogSearch, ChildrenQuery, Connection,
    DriverError, Identifier, NodeKind, RejectionKind, RelationName, ResultCursor, ServerActivity,
    ServerSession, SessionId, SessionState, StopKind, StopOutcome, TransactionStatus,
    blocking_order, describe_error,
};

/// A fresh connection reports its server, answers pings, and closes cleanly.
pub async fn check_lifecycle(mut connection: Box<dyn Connection>) {
    let info = connection.server_info().clone();
    assert!(!info.product.is_empty(), "server product is empty");
    assert!(!info.version.is_empty(), "server version is empty");

    connection.ping().await.expect("first ping");
    connection.ping().await.expect("second ping");
    connection.close().await.expect("close");
}

/// A search of the whole catalog for `text` (C0g) finds only nodes whose label contains it,
/// ignoring case, at most its limit, each exactly as the level it belongs to lists it (its
/// detail aside, which a match uses to say where it is). Returns the matches.
pub async fn check_catalog_search(connection: &mut dyn Connection, text: &str) -> CatalogMatches {
    let search = CatalogSearch::new(text);
    let matches = connection
        .search_catalog(&search)
        .await
        .expect("a search")
        .expect("the driver searches");
    assert!(matches.nodes.len() <= search.limit, "over the limit");
    let needle = text.to_lowercase();
    for found in &matches.nodes {
        assert!(
            found.label.to_lowercase().contains(&needle),
            "{} doesn't contain {text}",
            found.label
        );
        let parent = found.path.parent().expect("a match has a parent");
        let level = connection
            .children(&parent)
            .await
            .expect("the match's level");
        let listed = level
            .iter()
            .find(|node| node.path == found.path)
            .unwrap_or_else(|| panic!("{} isn't listed under {parent}", found.path));
        assert_eq!(
            CatalogNode {
                detail: None,
                ..found.clone()
            },
            CatalogNode {
                detail: None,
                ..listed.clone()
            },
            "{} differs from its level's node",
            found.path
        );
    }
    matches
}

/// Paging through the level at `path`, `limit` nodes at a time, gives exactly what
/// [`Connection::children`] lists, in its order, in `pages` pages; and a page filtered by
/// `filter` holds exactly the nodes whose label contains it, ignoring case. Returns the
/// labels, for the caller's own checks.
pub async fn check_children_pages(
    connection: &mut dyn Connection,
    path: &CatalogPath,
    limit: usize,
    pages: usize,
    filter: &str,
) -> Vec<String> {
    let labels = |nodes: &[CatalogNode]| -> Vec<String> {
        nodes.iter().map(|node| node.label.clone()).collect()
    };
    let whole = labels(&connection.children(path).await.expect("the whole level"));

    let mut paged = Vec::new();
    let mut query = ChildrenQuery {
        filter: None,
        after: None,
        limit: Some(limit),
    };
    let mut read = 0;
    loop {
        let page = connection
            .children_page(path, &query)
            .await
            .expect("a page of the level");
        read += 1;
        assert!(page.nodes.len() <= limit, "page {read} is over the limit");
        paged.extend(labels(&page.nodes));
        match page.next {
            Some(next) => query = query.after(next),
            None => break,
        }
    }
    assert_eq!(
        paged, whole,
        "the pages of {path} differ from the whole level"
    );
    assert_eq!(read, pages, "{path} came in {read} pages");

    let filtered = ChildrenQuery {
        filter: Some(filter.to_owned()),
        after: None,
        limit: None,
    };
    let page = connection
        .children_page(path, &filtered)
        .await
        .expect("the filtered level");
    let expected: Vec<String> = whole
        .iter()
        .filter(|label| filtered.matches(label))
        .cloned()
        .collect();
    assert_eq!(labels(&page.nodes), expected, "{path} filtered by {filter}");
    whole
}

/// A port on this machine that forwards every connection to `server` (`host:port`), standing
/// in for an SSH tunnel's local end: a driver connected to it with [`crate::Route::Tunnel`]
/// must reach the server, and still check TLS against the server's own name. Forwards until
/// the test's runtime ends.
pub async fn forwarder(server: &str) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the forwarding port");
    let local = listener.local_addr().expect("forwarding port's address");
    let server = server.to_owned();
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            let server = server.clone();
            tokio::spawn(async move {
                if let Ok(mut outbound) = tokio::net::TcpStream::connect(&server).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    local
}

/// A connection whose session the server has ended (killed, or the server restarted) reports
/// [`DriverError::Lost`], so the engine stops calling it connected: a ping, a statement and
/// closing it all go through without hanging.
pub async fn check_lost(mut connection: Box<dyn Connection>) {
    match connection.ping().await {
        Err(DriverError::Lost(_)) => {}
        other => panic!("expected the ping to find the connection lost, got {other:?}"),
    }
    match connection.execute("SELECT 1").await {
        Err(DriverError::Lost(_)) => {}
        Err(other) => panic!("expected the statement to find the connection lost, got {other:?}"),
        Ok(_) => panic!("expected the statement to find the connection lost, but it ran"),
    }
    // Closing a lost connection may fail; it only has to come back.
    let _ = connection.close().await;
}

/// Waits until `condition` holds, polling, rather than for a fixed time: how long a server
/// takes to end a session varies, and a fixed sleep is either slow or flaky.
pub async fn wait_until<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition().await {
        assert!(Instant::now() < deadline, "{what} never happened");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A connection attempt that can't succeed is reported as [`DriverError::Connect`].
pub fn check_connect_failure<C>(result: Result<C, DriverError>) {
    match result {
        Err(DriverError::Connect(_)) => {}
        Err(other) => panic!("expected DriverError::Connect, got {other:?}"),
        Ok(_) => panic!("expected the connection attempt to fail"),
    }
}

/// Walks the catalog breadth-first, `max_depth` levels deep, skipping system nodes, and checks
/// the invariants every driver keeps: each child's path extends its parent's, labels are not
/// empty, and every node that claims a definition can produce one. Returns every node visited,
/// so driver tests can assert on what their backend should contain.
pub async fn walk_catalog(connection: &mut dyn Connection, max_depth: usize) -> Vec<CatalogNode> {
    walk_catalog_under(connection, &CatalogPath::root(), max_depth).await
}

/// Walks the catalog below `start` as [`walk_catalog`] walks it below the root. For a server
/// that other tests change while this one runs: a walk that lists another test's table and
/// then asks for its definition after that test dropped it fails, so such a test walks only
/// the part of the catalog it made.
pub async fn walk_catalog_under(
    connection: &mut dyn Connection,
    start: &CatalogPath,
    max_depth: usize,
) -> Vec<CatalogNode> {
    let mut visited = Vec::new();
    let mut frontier = vec![start.clone()];
    for _ in 0..max_depth {
        let mut next = Vec::new();
        for path in frontier {
            let children = connection
                .children(&path)
                .await
                .unwrap_or_else(|error| panic!("children of {path}: {}", describe_error(&error)));
            for child in children {
                check_node(connection, &path, &child).await;
                if child.has_children && !child.system {
                    next.push(child.path.clone());
                }
                visited.push(child);
            }
        }
        frontier = next;
    }
    visited
}

async fn check_node(connection: &mut dyn Connection, parent: &CatalogPath, node: &CatalogNode) {
    assert_eq!(
        node.path.parent().as_ref(),
        Some(parent),
        "{} is not a child of {parent}",
        node.path
    );
    assert!(!node.label.is_empty(), "empty label at {}", node.path);
    if node.has_definition {
        let definition = connection
            .definition(&node.path)
            .await
            .unwrap_or_else(|error| {
                panic!("definition of {}: {}", node.path, describe_error(&error))
            });
        assert!(
            !definition.trim().is_empty(),
            "empty definition at {}",
            node.path
        );
    }
}

/// Paths the driver never produced are reported as [`DriverError::InvalidPath`], not as a
/// failed query or a panic.
pub async fn check_invalid_path(connection: &mut dyn Connection) {
    let nonsense = CatalogPath::root()
        .child(NodeKind::Column, "no such column")
        .child(NodeKind::Database, "no such database");

    let children = connection.children(&nonsense).await;
    assert!(
        matches!(children, Err(DriverError::InvalidPath(_))),
        "children of a nonsense path: {children:?}"
    );
    let definition = connection.definition(&nonsense).await;
    assert!(
        matches!(definition, Err(DriverError::InvalidPath(_))),
        "definition of a nonsense path: {definition:?}"
    );
}

const TABLE_ROWS: u64 = 1200;
const PAGE: usize = 500;

/// How an engine spells the few statements the checks can't write one way for all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spelling {
    temporary_tables: TemporaryTables,
    /// Opens a transaction.
    begin: &'static str,
    /// The type of a column of text.
    text_type: &'static str,
    /// The type a number is cast to before `concat` joins it to text, on an engine whose
    /// `concat` takes only text; `None` where it converts numbers itself.
    concat_cast: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemporaryTables {
    /// `CREATE TEMPORARY TABLE name`.
    Keyword,
    /// `CREATE TABLE #name`, and `#name` wherever it is used.
    HashPrefix,
    /// An ordinary table, in the schema the connection starts in, which the check drops
    /// itself: for an engine without temporary tables (Trino, in its `memory` catalog).
    Plain,
}

impl Spelling {
    /// PostgreSQL, SQLite and MySQL.
    pub const STANDARD: Self = Self {
        temporary_tables: TemporaryTables::Keyword,
        begin: "BEGIN",
        text_type: "text",
        concat_cast: None,
    };

    /// SQL Server, where `BEGIN` alone opens a block, not a transaction.
    pub const SQL_SERVER: Self = Self {
        temporary_tables: TemporaryTables::HashPrefix,
        begin: "BEGIN TRANSACTION",
        text_type: "text",
        concat_cast: None,
    };

    /// Trino, which has no temporary tables, calls text `varchar`, and whose `concat` takes
    /// only text. The connection must start in a schema that takes tables, such as
    /// `memory.default`.
    pub const TRINO: Self = Self {
        temporary_tables: TemporaryTables::Plain,
        begin: "START TRANSACTION",
        text_type: "varchar",
        concat_cast: Some("varchar"),
    };

    /// What statements call the temporary table `name`.
    fn temporary(self, name: &str) -> String {
        match self.temporary_tables {
            TemporaryTables::Keyword | TemporaryTables::Plain => name.to_owned(),
            TemporaryTables::HashPrefix => format!("#{name}"),
        }
    }

    /// Creates the temporary table `name` with `columns`, a parenthesised list.
    fn create_temporary(self, name: &str, columns: &str) -> String {
        match self.temporary_tables {
            TemporaryTables::Keyword => format!("CREATE TEMPORARY TABLE {name} {columns}"),
            TemporaryTables::HashPrefix => format!("CREATE TABLE #{name} {columns}"),
            TemporaryTables::Plain => format!("CREATE TABLE {name} {columns}"),
        }
    }

    /// `number` as `concat` takes it.
    fn concat_operand(self, number: &str) -> String {
        match self.concat_cast {
            Some(text) => format!("CAST({number} AS {text})"),
            None => number.to_owned(),
        }
    }
}

/// The numbers `0` to `9` as a derived table with one column, `d`.
const DIGITS: &str = "(SELECT 0 AS d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 \
     UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 \
     UNION ALL SELECT 8 UNION ALL SELECT 9)";

/// Runs `sql` to completion, reading every row, and returns what `finish` reports.
async fn run_to_end(connection: &mut dyn Connection, sql: &str) -> Option<u64> {
    let mut cursor = connection
        .execute(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {}", describe_error(&error)));
    while !cursor.is_exhausted() {
        cursor.next_page(PAGE).await.expect("read rows");
    }
    cursor.finish().await.expect("finish")
}

/// Statements run, results page lazily, NULL is distinct from text, a cursor can be
/// abandoned halfway, and errors point at the offending text. Uses only SQL that every engine
/// accepts, apart from how a temporary table is named. The rows come from a cross join of
/// digits rather than a recursive query, which SQL Server writes without `RECURSIVE` and MySQL
/// stops at 1,000 levels.
pub async fn check_execution(connection: &mut dyn Connection, spelling: Spelling) {
    let table = spelling.temporary("dbrover_conformance");
    run_to_end(
        connection,
        &spelling.create_temporary(
            "dbrover_conformance",
            &format!("(id integer, name {})", spelling.text_type),
        ),
    )
    .await;
    let row_name = spelling.concat_operand("n.n");
    let inserted = run_to_end(
        connection,
        &format!(
            "INSERT INTO {table} SELECT n.n, concat('row ', {row_name}) FROM \
             (SELECT a.d + 10 * b.d + 100 * c.d + 1000 * e.d + 1 AS n \
             FROM {DIGITS} a CROSS JOIN {DIGITS} b CROSS JOIN {DIGITS} c \
             CROSS JOIN {DIGITS} e) n WHERE n.n <= {TABLE_ROWS}"
        ),
    )
    .await;
    assert_eq!(inserted, Some(TABLE_ROWS), "rows inserted");

    check_paging(connection, &table).await;
    check_null(connection).await;
    check_abandon(connection, &table).await;
    check_error_position(connection, &table).await;
    run_to_end(connection, &format!("DROP TABLE {table}")).await;
}

async fn check_paging(connection: &mut dyn Connection, table: &str) {
    let mut cursor = connection
        .execute(&format!("SELECT id, name FROM {table} ORDER BY id"))
        .await
        .expect("select");
    let names: Vec<&str> = cursor.columns().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "name"]);

    let mut page_sizes = Vec::new();
    let mut first_row = None;
    while !cursor.is_exhausted() {
        let page = cursor.next_page(PAGE).await.expect("page");
        first_row = first_row.or_else(|| page.first().cloned());
        page_sizes.push(page.len());
    }
    assert_eq!(
        first_row,
        Some(vec![Some("1".to_owned()), Some("row 1".to_owned())])
    );
    assert_eq!(
        page_sizes.iter().sum::<usize>(),
        1200,
        "pages: {page_sizes:?}"
    );
    assert!(
        page_sizes.iter().all(|size| *size <= PAGE),
        "pages: {page_sizes:?}"
    );
    cursor.finish().await.expect("finish select");
}

async fn check_null(connection: &mut dyn Connection) {
    let mut cursor = connection
        .execute("SELECT NULL AS no_value, '' AS empty_text")
        .await
        .expect("select null");
    let rows = cursor.next_page(PAGE).await.expect("null row");
    assert_eq!(rows, vec![vec![None, Some(String::new())]]);
    cursor.finish().await.expect("finish null");
}

async fn check_abandon(connection: &mut dyn Connection, table: &str) {
    let mut cursor = connection
        .execute(&format!("SELECT id FROM {table} ORDER BY id"))
        .await
        .expect("select");
    assert_eq!(cursor.next_page(10).await.expect("first rows").len(), 10);
    assert!(!cursor.is_exhausted());
    finish_ignoring_cancellation(cursor).await;
    connection
        .ping()
        .await
        .expect("usable after abandoning a result");
}

async fn finish_ignoring_cancellation(cursor: Box<dyn ResultCursor>) {
    match cursor.finish().await {
        Ok(_) | Err(DriverError::Statement { .. }) => {}
        Err(other) => panic!("finishing an abandoned cursor: {other:?}"),
    }
}

async fn check_error_position(connection: &mut dyn Connection, table: &str) {
    // A syntax error, because that is where every engine reports a position (SQLite gives
    // none for unknown tables or columns). Not a repeated keyword: ClickHouse reads
    // `WHERE WHERE id` as a column named `WHERE` and fails at `id`.
    let sql = format!("SELECT id FROM {table} WHERE id = = 1");
    let sql = sql.as_str();
    let expected = sql.rfind('=').expect("second =");
    let error = match connection.execute(sql).await {
        Ok(cursor) => {
            let mut cursor = cursor;
            let read = cursor.next_page(PAGE).await;
            match (read, cursor.finish().await) {
                (Err(error), _) | (Ok(_), Err(error)) => error,
                (Ok(_), Ok(_)) => panic!("{sql} succeeded"),
            }
        }
        Err(error) => error,
    };
    match error {
        DriverError::Statement { position, .. } => {
            assert_eq!(position, Some(expected), "error position in {sql:?}");
        }
        other => panic!("expected a statement error, got {other:?}"),
    }
    connection.ping().await.expect("usable after an error");
}

/// The transaction status follows `BEGIN`, `COMMIT` and `ROLLBACK`, committed changes stay,
/// rolled back ones don't, and a failure inside a transaction leaves it open (PostgreSQL also
/// marks it failed) until it is rolled back.
pub async fn check_transactions(connection: &mut dyn Connection, spelling: Spelling) {
    assert_eq!(
        status(connection).await,
        TransactionStatus::Idle,
        "at first"
    );
    let table = spelling.temporary("dbrover_transactions");
    run_to_end(
        connection,
        &spelling.create_temporary("dbrover_transactions", "(n integer)"),
    )
    .await;

    run_to_end(connection, spelling.begin).await;
    assert_eq!(
        status(connection).await,
        TransactionStatus::InTransaction,
        "after BEGIN"
    );
    run_to_end(connection, &format!("INSERT INTO {table} VALUES (1)")).await;
    run_to_end(connection, "COMMIT").await;
    assert_eq!(
        status(connection).await,
        TransactionStatus::Idle,
        "after COMMIT"
    );

    run_to_end(connection, spelling.begin).await;
    run_to_end(connection, &format!("INSERT INTO {table} VALUES (2)")).await;
    let failed = connection
        .execute("SELECT * FROM dbrover_no_such_table")
        .await;
    assert!(failed.is_err(), "selecting from a missing table");
    assert_ne!(
        status(connection).await,
        TransactionStatus::Idle,
        "after a failure"
    );
    run_to_end(connection, "ROLLBACK").await;
    assert_eq!(
        status(connection).await,
        TransactionStatus::Idle,
        "after ROLLBACK"
    );

    let mut cursor = connection
        .execute(&format!("SELECT n FROM {table}"))
        .await
        .expect("select");
    let rows = cursor.next_page(PAGE).await.expect("rows");
    cursor.finish().await.expect("finish");
    assert_eq!(
        rows,
        vec![vec![Some("1".to_owned())]],
        "only the committed row"
    );
    run_to_end(connection, &format!("DROP TABLE {table}")).await;
}

/// How an engine matches an unquoted table name, which a description must follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnquotedNames {
    /// PostgreSQL folds them to lowercase, SQLite compares ignoring case, and so does MySQL
    /// when `lower_case_table_names` is set (the default on macOS and Windows).
    IgnoreCase,
    /// MySQL on Linux by default: `Orders` and `orders` are different tables.
    ExactCase,
}

/// Tables resolve as statements would, with their columns in table order and their primary
/// key in key order; views and missing tables are `None`. The view is an ordinary one,
/// dropped first in case an earlier run failed, because MySQL has no temporary views.
pub async fn check_describe_table(
    connection: &mut dyn Connection,
    names: UnquotedNames,
    spelling: Spelling,
) {
    run_to_end(
        connection,
        &spelling.create_temporary(
            "dbrover_describe",
            "(id integer, part varchar(50), note text, PRIMARY KEY (part, id))",
        ),
    )
    .await;
    run_to_end(
        connection,
        &spelling.create_temporary("dbrover_keyless", "(n integer)"),
    )
    .await;
    run_to_end(connection, "DROP VIEW IF EXISTS dbrover_describe_view").await;
    run_to_end(
        connection,
        "CREATE VIEW dbrover_describe_view AS SELECT 1 AS one",
    )
    .await;
    let unquoted = |name: &str| RelationName {
        schema: None,
        name: Identifier {
            name: name.to_owned(),
            quoted: false,
        },
    };
    let written = match names {
        UnquotedNames::IgnoreCase => "DBROVER_DESCRIBE",
        UnquotedNames::ExactCase => "dbrover_describe",
    };
    let described = connection
        .describe_table(&unquoted(&spelling.temporary(written)))
        .await
        .expect("describe")
        .expect("the table exists");
    assert_eq!(described.name, spelling.temporary("dbrover_describe"));
    assert!(!described.schema.is_empty());
    let column_names: Vec<&str> = described.column_names().collect();
    assert_eq!(column_names, ["id", "part", "note"]);
    assert!(
        described
            .columns
            .iter()
            .all(|column| !column.data_type.is_empty()),
        "every column has a type: {:?}",
        described.columns
    );
    assert!(described.columns[2].nullable, "note is nullable");
    assert_eq!(described.primary_key, ["part", "id"]);
    if names == UnquotedNames::ExactCase {
        let other_case = connection
            .describe_table(&unquoted(&spelling.temporary("DBROVER_DESCRIBE")))
            .await
            .expect("describe");
        assert_eq!(other_case, None, "names match exactly");
    }
    let keyless = connection
        .describe_table(&unquoted(&spelling.temporary("dbrover_keyless")))
        .await
        .expect("describe")
        .expect("the table exists");
    assert!(keyless.primary_key.is_empty());
    for not_a_table in ["dbrover_describe_view", "dbrover_no_such_table"] {
        let found = connection
            .describe_table(&unquoted(not_a_table))
            .await
            .expect("describe");
        assert_eq!(found, None, "{not_a_table}");
    }
    for drop in [
        "DROP VIEW dbrover_describe_view".to_owned(),
        format!("DROP TABLE {}", spelling.temporary("dbrover_keyless")),
        format!("DROP TABLE {}", spelling.temporary("dbrover_describe")),
    ] {
        run_to_end(connection, &drop).await;
    }
}

/// Validating prepares without running: an accepted `INSERT` inserts nothing, the connection's
/// temporary tables are visible, unknown names and syntax errors are told apart (syntax errors
/// with their position), and no transaction is left open.
pub async fn check_validate(connection: &mut dyn Connection, spelling: Spelling) {
    let table = spelling.temporary("dbrover_validate");
    run_to_end(
        connection,
        &spelling.create_temporary("dbrover_validate", "(n integer)"),
    )
    .await;
    for accepted in [
        format!("SELECT n FROM {table} WHERE n > 1"),
        format!("INSERT INTO {table} VALUES (1)"),
        format!("DELETE FROM {table} WHERE n = 1"),
    ] {
        let rejection = connection.validate(&accepted).await.expect("validate");
        assert_eq!(rejection, None, "{accepted}");
    }
    let mut cursor = connection
        .execute(&format!("SELECT count(*) FROM {table}"))
        .await
        .expect("count");
    let rows = cursor.next_page(PAGE).await.expect("read the count");
    cursor.finish().await.expect("finish");
    assert_eq!(
        rows,
        vec![vec![Some("0".to_owned())]],
        "nothing was inserted"
    );

    for (unknown, sql) in [
        ("table", "SELECT * FROM dbrover_no_such_table".to_owned()),
        ("column", format!("SELECT missing FROM {table}")),
    ] {
        let rejection = connection
            .validate(&sql)
            .await
            .expect("validate")
            .unwrap_or_else(|| panic!("an unknown {unknown} is rejected"));
        assert_eq!(rejection.kind, RejectionKind::UnknownName, "{rejection:?}");
        assert!(!rejection.message.is_empty());
    }
    let sql = format!("SELECT n n n FROM {table}");
    let rejection = connection
        .validate(&sql)
        .await
        .expect("validate")
        .expect("a syntax error is rejected");
    assert_eq!(rejection.kind, RejectionKind::Syntax, "{rejection:?}");
    assert_eq!(
        rejection.position,
        Some("SELECT n n ".len()),
        "{rejection:?}"
    );
    assert_eq!(status(connection).await, TransactionStatus::Idle);
    run_to_end(connection, &format!("DROP TABLE {table}")).await;
}

async fn status(connection: &mut dyn Connection) -> TransactionStatus {
    connection
        .transaction_status()
        .await
        .expect("transaction status")
}

/// How long `check_cancel` waits before cancelling, and the most the whole statement may
/// take once cancelled. Generous so slow CI machines don't flake.
const CANCEL_AFTER: Duration = Duration::from_millis(200);
const CANCEL_DEADLINE: Duration = Duration::from_secs(10);

/// A statement that would run far longer than [`CANCEL_DEADLINE`] stops promptly when
/// cancelled from another task, with a statement error, and the connection stays usable.
pub async fn check_cancel(connection: &mut dyn Connection, long_running_sql: &str) {
    let handle = connection.cancel_handle();
    let started = Instant::now();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(CANCEL_AFTER).await;
        handle.cancel().await
    });

    let result = async {
        let mut cursor = connection.execute(long_running_sql).await?;
        while !cursor.is_exhausted() {
            cursor.next_page(PAGE).await?;
        }
        cursor.finish().await
    }
    .await;

    assert!(
        matches!(result, Err(DriverError::Statement { .. })),
        "cancelled statement: {result:?}"
    );
    assert!(
        started.elapsed() < CANCEL_DEADLINE,
        "cancel took {:?}",
        started.elapsed()
    );
    canceller
        .await
        .expect("cancel task")
        .expect("cancel request");
    connection.ping().await.expect("usable after cancel");
}

/// What [`check_server_activity`] runs on one engine: a committed table with one row, that row
/// locked in a transaction left open, and a statement made to wait on it.
pub struct BlockingScript<'a> {
    /// Run on the watching connection: creates the table, with the row whose `id` is 1.
    pub setup: &'a [&'a str],
    /// Run on the holding connection: opens a transaction and locks the row.
    pub hold: &'a [&'a str],
    /// Run on the waiting connection before it waits: a lock timeout of some seconds, so a
    /// broken driver fails the test rather than hanging it.
    pub before_wait: &'a [&'a str],
    /// The statement that waits on the row.
    pub wait: &'a str,
    /// Run on the watching connection at the end: drops the table.
    pub teardown: &'a [&'a str],
    /// Whether the engine can cancel another session's statement and keep the session. SQL
    /// Server can't.
    pub cancels: bool,
}

/// How long a test waits for the server to show what it expects. Generous, since a loaded CI
/// runner can be slow, but far below the waiter's own lock timeout.
const ACTIVITY_DEADLINE: Duration = Duration::from_secs(10);

/// Server activity (W15) shows a session waiting on another's row lock, under it, with the
/// lock; cancelling the waiter stops its statement and keeps its session; killing the holder
/// ends its session; and the same kill sent again, once the session has gone, stops nothing.
///
/// `watcher` reads and stops; `holder` and `waiter` are two more connections to the same
/// server, which the check uses up.
pub async fn check_server_activity(
    watcher: &mut dyn Connection,
    mut holder: Box<dyn Connection>,
    mut waiter: Box<dyn Connection>,
    script: &BlockingScript<'_>,
) {
    for sql in script.setup {
        run_to_end(watcher, sql).await;
    }
    for sql in script.hold {
        run_to_end(holder.as_mut(), sql).await;
    }
    let watcher_id = own_session(watcher).await;
    let holder_id = own_session(holder.as_mut()).await;
    let waiter_id = own_session(waiter.as_mut()).await;
    for sql in script.before_wait {
        run_to_end(waiter.as_mut(), sql).await;
    }
    let wait_sql = script.wait.to_owned();
    let waiting = tokio::spawn(async move {
        let result = run_once(waiter.as_mut(), &wait_sql).await;
        (waiter, result)
    });

    let activity = read_until(
        watcher,
        "the waiter listed as waiting on the holder",
        |activity| {
            session(activity, &waiter_id).is_some_and(|waiter| {
                waiter.state == SessionState::Waiting && waiter.blocked_by.contains(&holder_id)
            })
        },
    )
    .await;
    let holder_session = session(&activity, &holder_id).expect("the holder is listed");
    let waiter_session = session(&activity, &waiter_id).expect("the waiter is listed");
    assert_eq!(
        holder_session.state,
        SessionState::IdleInTransaction,
        "{holder_session:#?}"
    );
    assert!(
        holder_session.transaction_for.is_some(),
        "{holder_session:#?}"
    );
    assert!(waiter_session.lock.is_some(), "{waiter_session:#?}");
    let statement = waiter_session
        .statement
        .as_ref()
        .expect("the waiter's statement");
    assert!(statement.running, "{statement:#?}");
    assert!(
        statement.text.contains(script.wait.trim_end_matches(';')),
        "{statement:#?}"
    );
    let watching = session(&activity, &watcher_id).expect("the watcher lists itself");
    assert_eq!(watching.state, SessionState::Active, "{watching:#?}");

    let rows = blocking_order(&activity.sessions, |_| true);
    let holder_row = rows
        .iter()
        .position(|row| activity.sessions[row.session].id == holder_id)
        .expect("the holder has a row");
    assert_eq!(rows[holder_row].depth, 0);
    let under = rows.get(holder_row + 1).expect("a row under the holder");
    assert_eq!(activity.sessions[under.session].id, waiter_id);
    assert_eq!(under.depth, 1);

    let kill = holder_session
        .stop(StopKind::EndSession)
        .expect("the holder can be killed")
        .clone();
    if script.cancels {
        let cancel = waiter_session
            .stop(StopKind::CancelQuery)
            .expect("the waiter's statement can be cancelled");
        assert_eq!(stop(watcher, cancel).await, StopOutcome::Stopped);
        let (mut waiter, result) = waiting.await.expect("the waiting task");
        assert!(
            matches!(result, Err(DriverError::Statement { .. })),
            "the cancelled statement: {result:?}"
        );
        waiter
            .ping()
            .await
            .expect("the waiter's session outlives Cancel");
        assert_eq!(stop(watcher, &kill).await, StopOutcome::Stopped);
    } else {
        assert!(
            waiter_session.stop(StopKind::CancelQuery).is_none(),
            "no Cancel is offered: {waiter_session:#?}"
        );
        assert_eq!(stop(watcher, &kill).await, StopOutcome::Stopped);
        let (_, result) = waiting.await.expect("the waiting task");
        result.expect("the waiter goes on once the holder is gone");
    }

    read_until(watcher, "the holder's session ending", |activity| {
        session(activity, &holder_id).is_none()
    })
    .await;
    check_lost(holder).await;
    assert_eq!(stop(watcher, &kill).await, StopOutcome::AlreadyEnded);

    for sql in script.teardown {
        run_to_end(watcher, sql).await;
    }
}

async fn own_session(connection: &mut dyn Connection) -> SessionId {
    connection
        .own_session()
        .await
        .expect("asking for the session's id")
        .expect("a session id")
}

fn session<'a>(activity: &'a ServerActivity, id: &SessionId) -> Option<&'a ServerSession> {
    activity.sessions.iter().find(|session| &session.id == id)
}

async fn stop(watcher: &mut dyn Connection, stop: &crate::SessionStop) -> StopOutcome {
    watcher
        .stop_session(stop)
        .await
        .unwrap_or_else(|error| panic!("{}: {}", stop.statement, describe_error(&error)))
}

/// Reads the server's activity until `done` holds, polling: how long a server takes to show a
/// wait or end a session varies, and a fixed sleep is either slow or flaky.
async fn read_until(
    watcher: &mut dyn Connection,
    what: &str,
    done: impl Fn(&ServerActivity) -> bool,
) -> ServerActivity {
    let deadline = Instant::now() + ACTIVITY_DEADLINE;
    loop {
        let activity = watcher
            .server_activity()
            .await
            .unwrap_or_else(|error| panic!("reading activity: {}", describe_error(&error)));
        if done(&activity) {
            return activity;
        }
        assert!(
            Instant::now() < deadline,
            "{what} never happened: {activity:#?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Runs `sql` to completion, as [`run_to_end`] does, but returns a failure instead of
/// panicking on it.
async fn run_once(connection: &mut dyn Connection, sql: &str) -> Result<(), DriverError> {
    let mut cursor = connection.execute(sql).await?;
    while !cursor.is_exhausted() {
        cursor.next_page(PAGE).await?;
    }
    cursor.finish().await.map(|_| ())
}
