//! Explain plans (W10) through the engine, against the test servers: each engine answers with
//! one value that its `dbrover_plan` format reads, estimated and analysed, and leaves the
//! session as it found it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{TestEngine, mysql_test_servers, postgres_test_server, sqlserver_test_server};
use dbrover_core::{ConnectionConfig, DataSource, DataSourceId, Password, ServerInfo};
use dbrover_engine::{
    Command, ConsoleId, Event, PasswordUpdate, RunId, RunOrigin, RunPurpose, ScanApproval,
    SessionState, TransactionMode,
};
use dbrover_plan::Plan;
use dbrover_sql::{Dialect, ExplainDepth};

const CONSOLE: ConsoleId = ConsoleId(1);

/// A console on one test server, connected, and what it connected to.
struct Console {
    engine: TestEngine,
    data_source: DataSourceId,
    dialect: Dialect,
    server: ServerInfo,
    next_run: u64,
    _dir: tempfile::TempDir,
}

impl Console {
    fn connect(config: ConnectionConfig, password: Password, dialect: Dialect) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = TestEngine::start(&dir.path().join("data_sources.toml"), Arc::default());
        engine.expect_started(vec![]);
        let data_source = DataSource {
            id: DataSourceId::generate(),
            name: "test server".to_owned(),
            color: None,
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config,
        };
        let id = data_source.id;
        engine.send(Command::SaveDataSource {
            data_source,
            password: PasswordUpdate::Set(password),
            ssh_secret: PasswordUpdate::Keep,
        });
        assert!(matches!(engine.next_event(), Event::DataSourceSaved(_)));
        let mut console = Self {
            engine,
            data_source: id,
            dialect,
            server: ServerInfo {
                product: String::new(),
                version: String::new(),
            },
            next_run: 0,
            _dir: dir,
        };
        console.first_value("SELECT 1", RunPurpose::Run).unwrap();
        console
    }

    /// Runs `sql` for `purpose` and returns the first value it returned, or its failure.
    fn first_value(&mut self, sql: &str, purpose: RunPurpose) -> Result<Option<String>, String> {
        self.next_run += 1;
        let run = RunId(self.next_run);
        self.engine.send(Command::Execute {
            console: CONSOLE,
            data_source: self.data_source,
            run,
            statements: vec![sql.to_owned()],
            parameters: dbrover_sql::ParameterValues::default(),
            transactions: TransactionMode::Auto,
            purpose,
            origin: RunOrigin::Console,
            scans: ScanApproval::Ask,
        });
        let mut value = None;
        let mut failure = None;
        loop {
            match self.engine.next_event() {
                Event::ConsoleSessionChanged {
                    state: SessionState::Connected(server),
                    ..
                } => self.server = server,
                Event::RowsFetched { rows, .. } if value.is_none() => {
                    value = rows
                        .into_iter()
                        .next()
                        .and_then(|row| row.into_iter().next());
                }
                Event::StatementFinished {
                    result: Err(error), ..
                } => failure = Some(error.message),
                Event::RunFinished { run: finished, .. } if finished == run => break,
                _ => {}
            }
        }
        match failure {
            Some(failure) => Err(failure),
            None => Ok(value.flatten()),
        }
    }

    /// Explains `sql` to `depth` as the console asks for a drawn plan, and reads the answer.
    /// `None` where the server can't explain to that depth.
    fn plan(&mut self, sql: &str, depth: ExplainDepth) -> Option<Plan> {
        let request = self.dialect.plan_request(depth, Some(&self.server))?;
        let purpose = match depth {
            ExplainDepth::Estimated => RunPurpose::Explain,
            ExplainDepth::Analysed => RunPurpose::ExplainAnalyse,
        };
        let value = self
            .first_value(&format!("{}{sql}", request.prefix), purpose)
            .unwrap()
            .expect("a plan");
        Some(request.format.parse(&value).unwrap())
    }

    fn run(&mut self, sql: &str) {
        self.first_value(sql, RunPurpose::Run).unwrap();
    }
}

/// The titles of the plan's steps, in reading order.
fn titles(plan: &Plan) -> Vec<String> {
    plan.outline()
        .iter()
        .map(|line| line.step.title())
        .collect()
}

#[test]
fn explains_a_join_on_postgres_estimated_and_analysed() {
    let Some(server) = postgres_test_server() else {
        return;
    };
    let mut console = Console::connect(
        ConnectionConfig::Postgres(server.config),
        server.password,
        Dialect::Postgres,
    );
    let schema = format!("w10_engine_{}", std::process::id());
    console.run(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"));
    console.run(&format!("CREATE SCHEMA {schema}"));
    console.run(&format!(
        "CREATE TABLE {schema}.customers AS SELECT g AS id FROM generate_series(1, 100) g"
    ));
    console.run(&format!(
        "CREATE TABLE {schema}.orders AS SELECT g AS id, 1 + g % 100 AS customer_id \
         FROM generate_series(1, 2000) g"
    ));
    console.run(&format!("ANALYZE {schema}.customers"));
    console.run(&format!("ANALYZE {schema}.orders"));
    let join = format!(
        "SELECT count(*) FROM {schema}.orders o JOIN {schema}.customers c ON c.id = o.customer_id"
    );

    let estimated = console.plan(&join, ExplainDepth::Estimated).unwrap();
    let analysed = console.plan(&join, ExplainDepth::Analysed).unwrap();
    console.run(&format!("DROP SCHEMA {schema} CASCADE"));

    assert!(!estimated.is_analysed());
    assert!(
        titles(&estimated)
            .iter()
            .any(|title| title.ends_with("Join")),
        "{:?}",
        titles(&estimated)
    );
    assert!(analysed.is_analysed());
    let orders = analysed
        .outline()
        .iter()
        .find(|line| line.step.title() == "Seq Scan on orders o")
        .and_then(|line| line.step.actual)
        .unwrap();
    assert_eq!(orders.rows, Some(2000.0));
}

/// MySQL 8.4 and MariaDB: an estimated plan from each, and an analysed one where the server
/// has it, in its own format. MySQL's iterator format is switched on for the explain and back
/// to what the session had afterwards.
#[test]
fn explains_on_mysql_and_mariadb_and_puts_the_format_back() {
    for (config, password) in mysql_test_servers() {
        let mut console =
            Console::connect(ConnectionConfig::Mysql(config), password, Dialect::Mysql);
        let table = format!("w10_engine_{}", std::process::id());
        console.run(&format!("DROP TABLE IF EXISTS {table}"));
        console.run(&format!(
            "CREATE TABLE {table} (id int PRIMARY KEY, total decimal(10, 2))"
        ));
        console.run(&format!(
            "INSERT INTO {table} VALUES (1, 2.50), (2, 7.25), (3, 9.00)"
        ));
        let query = format!("SELECT sum(total) FROM {table} WHERE total > 5");

        let estimated = console.plan(&query, ExplainDepth::Estimated).unwrap();
        let analysed = console.plan(&query, ExplainDepth::Analysed);
        let format_after = (console.server.product == "MySQL").then(|| {
            console
                .first_value(
                    "SELECT @@SESSION.explain_json_format_version",
                    RunPurpose::Run,
                )
                .unwrap()
        });
        console.run(&format!("DROP TABLE {table}"));

        let server = console.server.clone();
        assert!(!estimated.is_analysed(), "{server}");
        assert!(
            titles(&estimated)
                .iter()
                .any(|title| title.contains(&format!("on {table}"))),
            "{server}: {:?}",
            titles(&estimated)
        );
        let analysed = analysed.expect("MySQL 8.4 and MariaDB both analyse");
        assert!(analysed.is_analysed(), "{server}");
        if let Some(format) = format_after {
            assert_eq!(format, Some("1".to_owned()), "{server}");
        }
    }
}

/// SQL Server: the estimated plan through `SHOWPLAN_XML`, the actual one through `STATISTICS
/// XML`, which the driver reads in place of the statement's rows; afterwards statements answer
/// with their rows again.
#[test]
fn explains_on_sqlserver_estimated_and_actual() {
    let Some((config, password)) = sqlserver_test_server() else {
        return;
    };
    let mut console = Console::connect(
        ConnectionConfig::SqlServer(config),
        password,
        Dialect::SqlServer,
    );
    let table = format!("w10_engine_{}", std::process::id());
    console.run(&format!("DROP TABLE IF EXISTS dbo.{table}"));
    console.run(&format!(
        "CREATE TABLE dbo.{table} (id int PRIMARY KEY, total decimal(10, 2))"
    ));
    console.run(&format!(
        "INSERT INTO dbo.{table} VALUES (1, 2.50), (2, 7.25), (3, 9.00)"
    ));
    let query = format!("SELECT SUM(total) FROM dbo.{table} WHERE total > 5");

    let estimated = console.plan(&query, ExplainDepth::Estimated).unwrap();
    let actual = console.plan(&query, ExplainDepth::Analysed).unwrap();
    let after = console.first_value(&query, RunPurpose::Run).unwrap();
    console.run(&format!("DROP TABLE dbo.{table}"));

    assert!(!estimated.is_analysed());
    assert!(
        titles(&estimated)
            .iter()
            .any(|title| title.contains(&format!("[{table}]"))),
        "{:?}",
        titles(&estimated)
    );
    assert!(actual.is_analysed());
    let scan = actual
        .outline()
        .iter()
        .find(|line| line.step.target.is_some())
        .and_then(|line| line.step.actual)
        .unwrap();
    assert_eq!(scan.loops, 1.0);
    assert_eq!(after, Some("16.25".to_owned()));
}
