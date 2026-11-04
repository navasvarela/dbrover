//! What the MCP tests share: the server started again as a child process, the test servers
//! from `test-servers.env`, the files the server reads, and calls as a client makes them.
//!
//! Each test binary runs without libtest's harness (`harness = false` in Cargo.toml),
//! because the server process is that same binary started again, and libtest would print
//! its own lines to the standard output the protocol runs on.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, MysqlConfig, Password, PostgresConfig, ProjectId,
    ServerConfig, SqliteConfig, TlsMode, describe_error,
};
use dbrover_engine::{PasswordCommands, ProcessRunner, Tunnels, open_data_source};
use dbrover_mcp::{McpConfig, serve_stdio};
use dbrover_store::{
    DataSourceFile, HistoryFile, ProjectFile, Projects, Secret, SecretError, SecretStore,
    TrustedCommands, TrustedCommandsFile,
};
use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::Value;
use tokio::io::AsyncReadExt as _;
use tokio::task::JoinHandle;

/// Set in the server process to the directory holding its files.
const CHILD_DIR: &str = "DBROVER_MCP_TEST_DIR";
const PASSWORDS: &str = "passwords.json";
/// The slug of the project every test serves.
pub const SLUG: &str = "test";

/// The test binary's `main`: serves as the child when started as one, and otherwise runs
/// `test`, unless the command line filters it out by `name`.
pub fn run<F: Future<Output = ()>>(name: &str, test: impl FnOnce() -> F) {
    if let Ok(dir) = std::env::var(CHILD_DIR) {
        serve_as_child(Path::new(&dir));
        return;
    }
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    if !filters.is_empty() && !filters.iter().any(|filter| name.contains(filter.as_str())) {
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(test());
    println!("{name}: ok");
}

/// Passwords in memory, standing in for the OS keychain; the server process reads them from
/// a file the test wrote. Keyed by account as the keychain is: the data source's id, or the
/// id and `/mcp` for its MCP login.
pub struct FileSecrets(HashMap<String, Password>);

impl SecretStore for FileSecrets {
    fn get(&self, id: DataSourceId, secret: Secret) -> Result<Option<Password>, SecretError> {
        let account = match secret {
            Secret::Database => id.to_string(),
            Secret::McpLogin => format!("{id}/mcp"),
            Secret::Ssh => format!("{id}/ssh"),
        };
        Ok(self.0.get(&account).cloned())
    }
    fn set(&self, _: DataSourceId, _: Secret, _: &Password) -> Result<(), SecretError> {
        Ok(())
    }
    fn delete(&self, _: DataSourceId, _: Secret) -> Result<(), SecretError> {
        Ok(())
    }
}

/// The server process, as `dbrover mcp` runs it: logs to standard error, the protocol on
/// standard output.
fn serve_as_child(dir: &Path) {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let passwords: HashMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(dir.join(PASSWORDS)).unwrap()).unwrap();
    let secrets = FileSecrets(
        passwords
            .into_iter()
            .map(|(account, password)| (account, Password::new(password)))
            .collect(),
    );
    let config = McpConfig {
        data_sources: DataSourceFile::new(dir.join("data_sources.toml")),
        projects: ProjectFile::new(dir.join("projects.toml")),
        history: HistoryFile::new(dir.join("history.sqlite")),
        secrets: Arc::new(secrets),
        known_hosts: dir.join("known_hosts"),
        trusted_commands: TrustedCommandsFile::new(dir.join("trusted_commands.toml")),
        reports: Some(Arc::new(StandInReports)),
    };
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(serve_stdio(config, SLUG))
        .unwrap();
}

/// Reports as the app's runner would list and run them, in the server process: `sales` reads
/// the data source `lake`, `broken` too but can't run, and `elsewhere` reads `warehouse`, which
/// no test project has. A run of `sales` writes `sales-<since>.csv` in the test's folder.
pub struct StandInReports;

impl dbrover_mcp::ReportRunner for StandInReports {
    fn list(&self) -> Result<Vec<dbrover_mcp::ReportListing>, String> {
        let listing =
            |name: &str, data_source: &str, problems: &[&str]| dbrover_mcp::ReportListing {
                name: name.to_owned(),
                title: format!("The {name} report"),
                description: None,
                params: vec![("since".to_owned(), "date".to_owned())],
                data_sources: Some(vec![data_source.to_owned()]),
                problems: problems
                    .iter()
                    .map(|problem| (*problem).to_owned())
                    .collect(),
            };
        Ok(vec![
            listing("sales", "lake", &[]),
            listing("broken", "lake", &["[combine]: needs a query"]),
            listing("elsewhere", "warehouse", &[]),
        ])
    }

    fn start(
        &self,
        report: &str,
        values: dbrover_report::TypedValues,
    ) -> Result<Box<dyn dbrover_mcp::RunningReport>, Vec<String>> {
        use dbrover_report::{RunEvent, WrittenFile};
        let since = values
            .get("since")
            .cloned()
            .ok_or_else(|| vec!["since needs a value".to_owned()])?;
        let dir = std::env::var(CHILD_DIR).map_err(|error| vec![error.to_string()])?;
        let path = Path::new(&dir).join(format!("{report}-{since}.csv"));
        std::fs::write(&path, "n\n1\n").map_err(|error| vec![error.to_string()])?;
        let events = vec![
            RunEvent::OutputWritten(WrittenFile {
                path,
                rows: 1,
                size: 4,
                elapsed_ms: 1,
                warnings: Vec::new(),
            }),
            RunEvent::Finished { elapsed_ms: 3 },
        ];
        Ok(Box::new(Scripted(events.into_iter().collect())))
    }
}

struct Scripted(std::collections::VecDeque<dbrover_report::RunEvent>);

impl dbrover_mcp::RunningReport for Scripted {
    fn next(&mut self) -> Option<dbrover_report::RunEvent> {
        self.0.pop_front()
    }

    fn stopper(&self) -> Box<dyn Fn() + Send + Sync> {
        Box::new(|| {})
    }
}

/// One engine under test: its data source and how its fixture is spelt.
pub struct Engine {
    pub data_source: DataSource,
    pub password: Option<Password>,
    /// What the table is called; unique per run, since other test runs share the servers.
    pub table: String,
    /// A function that writes, called from a `SELECT`. SQLite has no SQL functions.
    pub writing_function: Option<String>,
}

pub fn suffix() -> String {
    DataSourceId::generate().to_string()[..8].to_owned()
}

pub fn engines(dir: &Path, project: ProjectId) -> Vec<Engine> {
    let table = format!("mcp_items_{}", suffix());
    let sqlite_path = dir.join("shop.db");
    rusqlite::Connection::open(&sqlite_path).unwrap();
    let mut engines = vec![Engine {
        data_source: data_source(
            "sqlite",
            project,
            ConnectionConfig::Sqlite(SqliteConfig {
                path: sqlite_path,
                read_only: false,
            }),
        ),
        password: None,
        table: table.clone(),
        writing_function: None,
    }];
    if let Some((server, password)) = server_from_env("DBROVER_TEST_POSTGRES_URL") {
        engines.push(Engine {
            data_source: data_source(
                "postgres",
                project,
                ConnectionConfig::Postgres(PostgresConfig { server }),
            ),
            password: Some(password),
            table: table.clone(),
            writing_function: Some(format!("mcp_bump_{}", suffix())),
        });
    }
    for (name, variable) in [
        ("mysql", "DBROVER_TEST_MYSQL_URL"),
        ("mariadb", "DBROVER_TEST_MARIADB_URL"),
    ] {
        if let Some((server, password)) = server_from_env(variable) {
            engines.push(Engine {
                data_source: data_source(
                    name,
                    project,
                    ConnectionConfig::Mysql(MysqlConfig { server }),
                ),
                password: Some(password),
                table: table.clone(),
                writing_function: Some(format!("mcp_bump_{}", suffix())),
            });
        }
    }
    engines
}

pub fn data_source(name: &str, project: ProjectId, config: ConnectionConfig) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: name.to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: Some(project),
        mcp_login: None,
        config,
    }
}

/// Reads `scheme://user:password@host:port/database`, the form `test-servers.env` uses.
pub fn server_from_env(variable: &str) -> Option<(ServerConfig, Password)> {
    let url = std::env::var(variable).ok()?;
    let (_, rest) = url.split_once("://")?;
    let (credentials, rest) = rest.split_once('@')?;
    let (user, password) = credentials.split_once(':')?;
    let (address, database) = rest.split_once('/')?;
    let (host, port) = address.split_once(':')?;
    Some((
        ServerConfig {
            host: host.to_owned(),
            port: port.parse::<u16>().ok()?.into(),
            database: database.to_owned(),
            user: user.to_owned(),
            tls: TlsMode::default(),
            ssh: None,
            password_command: None,
            cloud_sign_in: None,
        },
        Password::new(password),
    ))
}

/// Runs statements on an ordinary, writable connection: the fixture, and checks after.
pub async fn run_directly(engine: &Engine, statements: &[String]) -> Vec<Vec<Option<String>>> {
    let secrets: Arc<dyn SecretStore> = Arc::new(FileSecrets(
        engine
            .password
            .iter()
            .map(|password| (engine.data_source.id.to_string(), password.clone()))
            .collect(),
    ));
    let mut writable = engine.data_source.clone();
    if let ConnectionConfig::Sqlite(config) = &mut writable.config {
        config.read_only = false;
    }
    // The fixtures' data sources have no SSH server, so no `known_hosts` is ever read.
    let tunnels = Tunnels::new(std::path::PathBuf::from("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let mut connection = open_data_source(&writable, &secrets, &tunnels, &commands)
        .await
        .unwrap();
    let mut last = Vec::new();
    for statement in statements {
        let mut cursor = connection
            .execute(statement)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error:?}"));
        last = cursor.next_page(100).await.unwrap();
        cursor.finish().await.unwrap();
    }
    connection.close().await.unwrap();
    last
}

/// Runs one statement as `engine`'s login, expecting the database to refuse it, and returns
/// why: for checking what a login's grants allow, beside DbRover's own checks.
pub async fn refused_directly(engine: &Engine, statement: &str) -> String {
    let secrets: Arc<dyn SecretStore> = Arc::new(FileSecrets(
        engine
            .password
            .iter()
            .map(|password| (engine.data_source.id.to_string(), password.clone()))
            .collect(),
    ));
    let tunnels = Tunnels::new(std::path::PathBuf::from("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let mut connection = open_data_source(&engine.data_source, &secrets, &tunnels, &commands)
        .await
        .unwrap();
    let error = match connection.execute(statement).await {
        Ok(cursor) => match cursor.finish().await {
            Ok(_) => panic!("{statement} ran"),
            Err(error) => error,
        },
        Err(error) => error,
    };
    connection.close().await.unwrap();
    describe_error(&error)
}

/// The account a password is kept under, as the keychain names it.
pub fn account(data_source: &DataSource) -> String {
    data_source.id.to_string()
}

/// The account of a data source's MCP login.
pub fn mcp_account(data_source: &DataSource) -> String {
    format!("{}/mcp", data_source.id)
}

/// Writes the files the server reads at start-up; `passwords` are keyed by [`account`] or
/// [`mcp_account`].
pub fn write_files(
    dir: &Path,
    data_sources: &[DataSource],
    projects: Projects,
    passwords: &HashMap<String, Password>,
) {
    DataSourceFile::new(dir.join("data_sources.toml"))
        .save(data_sources)
        .unwrap();
    ProjectFile::new(dir.join("projects.toml"))
        .save(&projects)
        .unwrap();
    let passwords: HashMap<&String, &str> = passwords
        .iter()
        .map(|(account, password)| (account, password.expose()))
        .collect();
    std::fs::write(
        dir.join(PASSWORDS),
        serde_json::to_string(&passwords).unwrap(),
    )
    .unwrap();
}

/// The engines' own passwords, as [`write_files`] takes them.
pub fn passwords(engines: &[Engine]) -> HashMap<String, Password> {
    engines
        .iter()
        .filter_map(|engine| {
            let password = engine.password.clone()?;
            Some((account(&engine.data_source), password))
        })
        .collect()
}

/// A server process and its client, with the log it writes to standard error.
pub struct Server {
    pub client: Client,
    log: Arc<Mutex<String>>,
    log_task: JoinHandle<()>,
}

impl Server {
    /// Starts `dbrover mcp` on the files in `dir`, as a client would.
    pub async fn start(dir: &Path) -> Self {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command.env(CHILD_DIR, dir);
        let (transport, stderr) = TokioChildProcess::builder(command)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let log = Arc::new(Mutex::new(String::new()));
        let log_task = {
            let log = Arc::clone(&log);
            let mut stderr = stderr.unwrap();
            tokio::spawn(async move {
                let mut text = String::new();
                stderr.read_to_string(&mut text).await.unwrap();
                log.lock().unwrap().push_str(&text);
            })
        };
        let client: Client = ().serve(transport).await.unwrap();
        Self {
            client,
            log,
            log_task,
        }
    }

    /// Ends the session, and hands back everything the server logged.
    pub async fn stop(self) -> String {
        self.client.cancel().await.unwrap();
        self.log_task.await.unwrap();
        self.log.lock().unwrap().clone()
    }

    pub async fn tools(&self) -> Vec<String> {
        self.client
            .list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }
}

pub type Client = rmcp::service::RunningService<rmcp::RoleClient, ()>;

pub async fn call(client: &Client, tool: &'static str, arguments: Value) -> (bool, Value) {
    let Value::Object(arguments) = arguments else {
        panic!("arguments must be an object");
    };
    let result = client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .unwrap();
    let value = result.structured_content.clone().unwrap_or(Value::Null);
    (result.is_error == Some(true), value)
}

pub async fn ok(client: &Client, tool: &'static str, arguments: Value) -> Value {
    let (failed, value) = call(client, tool, arguments.clone()).await;
    assert!(!failed, "{tool} {arguments} failed: {value}");
    value
}

pub async fn failure(client: &Client, tool: &'static str, arguments: Value) -> Value {
    let (failed, value) = call(client, tool, arguments.clone()).await;
    assert!(failed, "{tool} {arguments} should have failed: {value}");
    value["error"].clone()
}
