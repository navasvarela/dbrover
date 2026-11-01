//! Helpers shared by the engine's integration tests.

// Test helpers panic on failure like assertions do; clippy's test exemption only covers
// `#[test]` functions themselves, not helpers in an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use std::str::FromStr;

use dbrover_core::{
    CassandraConfig, ClickHouseConfig, ConnectionConfig, DataSource, DataSourceId, DriverKind,
    DynamoDbConfig, DynamoDbSignIn, ElasticsearchAuth, ElasticsearchConfig, MongoConfig,
    MysqlConfig, Password, PostgresConfig, RedisConfig, SafetyLevel, ServerConfig, SqlServerConfig,
    SqliteConfig, SshConfig, SshLogin, TlsMode,
};
use dbrover_engine::{
    Command, EngineConfig, EngineHandle, Event, InstallError, InstallSupport, InstalledUpdate,
    PasswordUpdate, ProcessRunner, Release, ReleaseError, ReleaseFeed, Repaint, Restart,
    SessionState, UpdateInstaller,
};
use dbrover_language::QueryLanguage;
use dbrover_sql::Dialect;
use dbrover_store::{
    ConsoleFiles, CustomFile, CustomFileKind, CustomFiles, DataSourceFile, DiagramFiles,
    HistoryFile, KeymapBindings, KeymapFile, ProjectFile, Projects, Secret, SecretError,
    SecretStore, Settings, SettingsFile, TrustedCommands, TrustedCommandsFile,
};

pub const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a test lets the engine take to stop. Far longer than the app's own two seconds:
/// what matters here is that it stops and saves, not how fast, and a busy Windows runner has
/// taken over two seconds to close the history database.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct MemorySecrets {
    saved: Mutex<HashMap<(DataSourceId, Secret), Password>>,
    /// Refuses every read and write, like a keychain that is locked or missing.
    locked: AtomicBool,
}

impl MemorySecrets {
    pub fn locked() -> Self {
        Self {
            locked: AtomicBool::new(true),
            ..Self::default()
        }
    }

    /// The data source's saved database password.
    pub fn database(&self, id: DataSourceId) -> Option<Password> {
        self.saved(id, Secret::Database)
    }

    pub fn mcp(&self, id: DataSourceId) -> Option<Password> {
        self.saved(id, Secret::McpLogin)
    }

    pub fn saved(&self, id: DataSourceId, secret: Secret) -> Option<Password> {
        self.saved.lock().unwrap().get(&(id, secret)).cloned()
    }

    fn check_unlocked(&self) -> Result<(), SecretError> {
        if self.locked.load(Ordering::Relaxed) {
            return Err(SecretError::unavailable("the keychain is locked"));
        }
        Ok(())
    }
}

impl SecretStore for MemorySecrets {
    fn get(&self, id: DataSourceId, secret: Secret) -> Result<Option<Password>, SecretError> {
        self.check_unlocked()?;
        Ok(self.saved(id, secret))
    }

    fn set(&self, id: DataSourceId, secret: Secret, value: &Password) -> Result<(), SecretError> {
        self.check_unlocked()?;
        self.saved
            .lock()
            .unwrap()
            .insert((id, secret), value.clone());
        Ok(())
    }

    fn delete(&self, id: DataSourceId, secret: Secret) -> Result<(), SecretError> {
        self.check_unlocked()?;
        self.saved.lock().unwrap().remove(&(id, secret));
        Ok(())
    }
}

/// A release feed that answers without a network: with this release, or that there is none.
pub struct FixedReleases(pub Option<Release>);

impl ReleaseFeed for FixedReleases {
    fn latest(&self) -> Result<Release, ReleaseError> {
        self.0.clone().ok_or(ReleaseError::NoReleases)
    }
}

/// An installer that pretends: it reports a download in three steps, then installs
/// `Installs(version)` or fails with `Fails(reason)`, touching nothing.
pub enum FakeInstaller {
    Installs(&'static str),
    Fails(&'static str),
}

impl UpdateInstaller for FakeInstaller {
    fn support(&self) -> InstallSupport {
        InstallSupport::Supported
    }

    fn install(
        &self,
        progress: &dyn Fn(u64, Option<u64>),
    ) -> Result<InstalledUpdate, InstallError> {
        for downloaded in [0, 50, 100] {
            progress(downloaded, Some(100));
        }
        match self {
            Self::Installs(version) => Ok(InstalledUpdate {
                version: (*version).to_owned(),
                restart: Restart::Relaunch,
            }),
            Self::Fails(reason) => Err(InstallError::Unsupported((*reason).to_owned())),
        }
    }
}

pub struct SignalRepaint(mpsc::Sender<()>);

impl Repaint for SignalRepaint {
    fn request_repaint(&self) {
        // The test may already have dropped the receiver; that's fine.
        let _ = self.0.send(());
    }
}

pub struct TestEngine {
    pub engine: EngineHandle,
    repaints: mpsc::Receiver<()>,
}

impl TestEngine {
    pub fn start(data_sources_file: &Path, secrets: Arc<MemorySecrets>) -> Self {
        Self::start_with_releases(data_sources_file, secrets, FixedReleases(None))
    }

    pub fn start_with_releases(
        data_sources_file: &Path,
        secrets: Arc<MemorySecrets>,
        releases: FixedReleases,
    ) -> Self {
        Self::start_with(
            data_sources_file,
            secrets,
            releases,
            FakeInstaller::Installs("9.9.9"),
        )
    }

    pub fn start_with_installer(data_sources_file: &Path, installer: FakeInstaller) -> Self {
        Self::start_with(
            data_sources_file,
            Arc::default(),
            FixedReleases(None),
            installer,
        )
    }

    /// An engine that pings a connection before every request, as if each had sat idle
    /// past [`dbrover_engine::IDLE_PING_AFTER`].
    pub fn start_pinging_before_every_request(
        data_sources_file: &Path,
        secrets: Arc<MemorySecrets>,
    ) -> Self {
        let config = EngineConfig {
            idle_ping_after: Duration::ZERO,
            ..Self::config(
                data_sources_file,
                secrets,
                FixedReleases(None),
                FakeInstaller::Installs("9.9.9"),
            )
        };
        Self::start_configured(config)
    }

    fn start_with(
        data_sources_file: &Path,
        secrets: Arc<MemorySecrets>,
        releases: FixedReleases,
        installer: FakeInstaller,
    ) -> Self {
        Self::start_configured(Self::config(
            data_sources_file,
            secrets,
            releases,
            installer,
        ))
    }

    fn start_configured(config: EngineConfig) -> Self {
        let (sender, repaints) = mpsc::channel();
        let engine = EngineHandle::start(config, SignalRepaint(sender)).unwrap();
        Self { engine, repaints }
    }

    fn config(
        data_sources_file: &Path,
        secrets: Arc<MemorySecrets>,
        releases: FixedReleases,
        installer: FakeInstaller,
    ) -> EngineConfig {
        let dir = data_sources_file.parent().unwrap_or(Path::new("."));
        EngineConfig {
            data_sources: DataSourceFile::new(data_sources_file),
            projects: ProjectFile::new(dir.join("projects.toml")),
            secrets,
            consoles: ConsoleFiles::new(dir.join("consoles")),
            diagrams: DiagramFiles::new(dir.join("diagrams")),
            history: HistoryFile::new(dir.join("history.sqlite")),
            keymap: KeymapFile::new(dir.join("keymap.toml")),
            settings: SettingsFile::new(dir.join("settings.toml")),
            custom_files: CustomFiles::new(dir),
            trusted_commands: TrustedCommandsFile::new(dir.join("trusted_commands.toml")),
            programs: Arc::new(ProcessRunner),
            releases: Arc::new(releases),
            updater: Arc::new(installer),
            known_hosts: dir.join("known_hosts"),
            idle_ping_after: dbrover_engine::IDLE_PING_AFTER,
        }
    }

    /// Stops the engine, waiting for it to save everything first.
    pub fn shut_down(self) {
        self.engine.shutdown_within(SHUTDOWN_TIMEOUT).unwrap();
    }

    pub fn send(&self, command: Command) {
        self.engine.send(command).unwrap();
    }

    pub fn next_event(&mut self) -> Event {
        loop {
            if let Some(event) = self.engine.try_recv().unwrap() {
                return event;
            }
            self.repaints
                .recv_timeout(EVENT_TIMEOUT)
                .expect("timed out waiting for an event");
        }
    }

    pub fn expect(&mut self, expected: Event) {
        assert_eq!(self.next_event(), expected);
    }

    /// Expects the start-up events of an engine with no saved consoles.
    pub fn expect_started(&mut self, data_sources: Vec<DataSource>) {
        self.expect_started_with_projects(Projects::default(), data_sources);
    }

    /// Expects the start-up events of an engine with these projects and no saved consoles.
    pub fn expect_started_with_projects(
        &mut self,
        projects: Projects,
        data_sources: Vec<DataSource>,
    ) {
        self.expect(Event::Started);
        self.expect_keymap(Ok(KeymapBindings::new()));
        self.expect_settings(Ok(Settings::default()));
        self.expect_no_custom_files();
        self.expect_nothing_trusted();
        self.expect(Event::ProjectsLoaded(projects));
        self.expect(Event::DataSourcesLoaded(data_sources));
        self.expect(Event::ConsolesLoaded(vec![]));
        self.expect(Event::DiagramsLoaded(vec![]));
    }

    pub fn expect_keymap(&mut self, expected: Result<KeymapBindings, String>) {
        match self.next_event() {
            Event::KeymapLoaded { result, .. } => match (result, expected) {
                (Ok(bindings), Ok(expected)) => assert_eq!(bindings, expected),
                (Err(_), Err(_)) => {}
                (other, expected) => panic!("expected keymap {expected:?}, got {other:?}"),
            },
            other => panic!("expected the keymap, got {other:?}"),
        }
    }

    pub fn expect_settings(&mut self, expected: Result<Settings, String>) {
        match self.next_event() {
            Event::SettingsLoaded { result, .. } => match (result, expected) {
                (Ok(settings), Ok(expected)) => assert_eq!(settings, expected),
                (Err(_), Err(_)) => {}
                (other, expected) => panic!("expected settings {expected:?}, got {other:?}"),
            },
            other => panic!("expected the settings, got {other:?}"),
        }
    }

    /// Expects `actions.toml`, `snippets.toml`, `theme.toml` and `exports.toml` to be reported,
    /// in that order, and returns what each held.
    pub fn expect_custom_files(&mut self) -> Vec<CustomFile> {
        CustomFileKind::ALL
            .into_iter()
            .map(|kind| match self.next_event() {
                Event::CustomFileLoaded { path, file } => {
                    assert_eq!(file.kind(), kind);
                    assert!(path.ends_with(kind.file_name()), "{}", path.display());
                    file
                }
                other => panic!("expected {}, got {other:?}", kind.file_name()),
            })
            .collect()
    }

    /// Expects the customisation files to be reported as absent: empty, with no problems.
    pub fn expect_no_custom_files(&mut self) {
        for file in self.expect_custom_files() {
            assert_eq!(file, CustomFile::parse(file.kind(), ""));
        }
    }

    /// Expects the start-up report of the trusted commands, with none trusted yet.
    pub fn expect_nothing_trusted(&mut self) {
        self.expect(Event::TrustedCommandsLoaded(TrustedCommands::default()));
    }

    pub fn expect_session(&mut self, id: DataSourceId) -> SessionState {
        match self.next_event() {
            Event::SessionChanged {
                id: event_id,
                state,
            } if event_id == id => state,
            other => panic!("expected a session change for {id}, got {other:?}"),
        }
    }
}

pub fn sqlite_database(dir: &Path) -> PathBuf {
    let path = dir.join("app.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    path
}

pub fn sqlite_data_source(path: PathBuf) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: "app".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Sqlite(SqliteConfig {
            path,
            read_only: false,
        }),
    }
}

/// The server named by `DBROVER_TEST_POSTGRES_URL`, like the Postgres driver tests use, or
/// `None` (with a note) when it is unset, so tests that need it skip.
pub fn postgres_test_server() -> Option<ParsedUrl> {
    let Ok(url) = std::env::var("DBROVER_TEST_POSTGRES_URL") else {
        eprintln!("skipping: DBROVER_TEST_POSTGRES_URL is not set");
        return None;
    };
    Some(parse_test_url(&url))
}

/// The MySQL-family servers named by `DBROVER_TEST_MYSQL_URL` and `DBROVER_TEST_MARIADB_URL`
/// (`mysql://user:password@host:port/database`), as data source settings and passwords.
/// Empty, with a note, when neither is set, so tests that need one skip.
pub fn mysql_test_servers() -> Vec<(MysqlConfig, Password)> {
    let servers: Vec<(MysqlConfig, Password)> =
        ["DBROVER_TEST_MYSQL_URL", "DBROVER_TEST_MARIADB_URL"]
            .into_iter()
            .filter_map(|variable| std::env::var(variable).ok())
            .map(|url| {
                let rest = url.strip_prefix("mysql://").expect("mysql:// URL");
                let (config, password) = parse_server(rest, "3306");
                (MysqlConfig { server: config }, password)
            })
            .collect();
    if servers.is_empty() {
        eprintln!("skipping: neither DBROVER_TEST_MYSQL_URL nor DBROVER_TEST_MARIADB_URL is set");
    }
    servers
}

/// The SQL Server named by `DBROVER_TEST_SQLSERVER_URL`
/// (`sqlserver://user:password@host:port/database`), or `None`, with a note, so tests that
/// need it skip.
pub fn sqlserver_test_server() -> Option<(SqlServerConfig, Password)> {
    let Ok(url) = std::env::var("DBROVER_TEST_SQLSERVER_URL") else {
        eprintln!("skipping: DBROVER_TEST_SQLSERVER_URL is not set");
        return None;
    };
    let rest = url.strip_prefix("sqlserver://").expect("sqlserver:// URL");
    let (server, password) = parse_server(rest, "1433");
    Some((SqlServerConfig { server }, password))
}

pub fn clickhouse_test_server() -> Option<(ClickHouseConfig, Password)> {
    let Ok(url) = std::env::var("DBROVER_TEST_CLICKHOUSE_URL") else {
        eprintln!("skipping: DBROVER_TEST_CLICKHOUSE_URL is not set");
        return None;
    };
    let rest = url
        .strip_prefix("clickhouse://")
        .expect("clickhouse:// URL");
    let (server, password) = parse_server(rest, "8123");
    Some((
        ClickHouseConfig {
            server,
            read_only: false,
        },
        password,
    ))
}

/// The Redis and Valkey test servers that are set, from `DBROVER_TEST_REDIS_URL` and
/// `DBROVER_TEST_VALKEY_URL` (`redis://user:password@host:port/database`).
pub fn redis_test_servers() -> Vec<(RedisConfig, Password)> {
    let servers: Vec<(RedisConfig, Password)> =
        ["DBROVER_TEST_REDIS_URL", "DBROVER_TEST_VALKEY_URL"]
            .into_iter()
            .filter_map(|variable| std::env::var(variable).ok())
            .map(|url| {
                let rest = url.strip_prefix("redis://").expect("redis:// URL");
                let (server, password) = parse_server(rest, "6379");
                let separator = RedisConfig::DEFAULT_SEPARATOR.to_owned();
                (RedisConfig { server, separator }, password)
            })
            .collect();
    if servers.is_empty() {
        eprintln!("skipping: neither DBROVER_TEST_REDIS_URL nor DBROVER_TEST_VALKEY_URL is set");
    }
    servers
}

/// The MongoDB test server, from `DBROVER_TEST_MONGODB_URL`
/// (`mongodb://user:password@host:port/database`), with `database` as the console's.
pub fn mongo_test_server(database: &str) -> Option<(MongoConfig, Password)> {
    let Ok(url) = std::env::var("DBROVER_TEST_MONGODB_URL") else {
        eprintln!("skipping: DBROVER_TEST_MONGODB_URL is not set");
        return None;
    };
    let rest = url.strip_prefix("mongodb://").expect("mongodb:// URL");
    let (mut server, password) = parse_server(rest, "27017");
    database.clone_into(&mut server.database);
    server.tls = TlsMode::Disable;
    let config = MongoConfig {
        server,
        auth_database: String::new(),
        connection_string: None,
    };
    Some((config, password))
}

/// The search test servers, from `DBROVER_TEST_ELASTICSEARCH_URL` and
/// `DBROVER_TEST_OPENSEARCH_URL` (`http[s]://[user:password@]host:port`), each with its name. An
/// HTTPS server's certificate is checked against the test authority.
pub fn search_test_servers() -> Vec<(&'static str, ElasticsearchConfig, PasswordUpdate)> {
    let mut servers = Vec::new();
    for name in [
        "DBROVER_TEST_ELASTICSEARCH_URL",
        "DBROVER_TEST_OPENSEARCH_URL",
    ] {
        let Ok(url) = std::env::var(name) else {
            eprintln!("skipping: {name} is not set");
            continue;
        };
        let (tls, rest) = match url.split_once("://") {
            Some(("https", rest)) => (
                TlsMode::VerifyFull {
                    ca_file: Some(
                        Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("../../docker/postgres-tls/ca.crt"),
                    ),
                },
                rest,
            ),
            _ => (TlsMode::Disable, url.split_once("://").expect("a URL").1),
        };
        let (credentials, address) = rest.rsplit_once('@').unwrap_or(("", rest));
        let (user, password) = credentials.split_once(':').unwrap_or((credentials, ""));
        let (host, port) = address.rsplit_once(':').expect("host:port");
        let config = ElasticsearchConfig {
            server: ServerConfig {
                host: host.to_owned(),
                port: u16::from_str(port).expect("numeric port").into(),
                database: String::new(),
                user: user.to_owned(),
                tls,
                ssh: None,
                password_command: None,
            },
            auth: ElasticsearchAuth::Password,
        };
        // A node without security takes an empty password, saved as a user would save one
        // in the dialog, so the engine doesn't ask for it.
        servers.push((name, config, PasswordUpdate::Set(Password::new(password))));
    }
    servers
}

/// The CQL test servers, from `DBROVER_TEST_SCYLLA_URL` and `DBROVER_TEST_CASSANDRA_URL`
/// (`cql://user:password@host:port`), each with its name.
pub fn cql_test_servers() -> Vec<(&'static str, CassandraConfig, PasswordUpdate)> {
    let mut servers = Vec::new();
    for name in ["DBROVER_TEST_SCYLLA_URL", "DBROVER_TEST_CASSANDRA_URL"] {
        let Some(url) = std::env::var(name).ok().filter(|url| !url.is_empty()) else {
            eprintln!("skipping: {name} is not set");
            continue;
        };
        let rest = url.strip_prefix("cql://").expect("a cql:// URL");
        let (credentials, address) = rest.rsplit_once('@').expect("user:password@host:port");
        let (user, password) = credentials.split_once(':').expect("user:password");
        let (host, port) = address.rsplit_once(':').expect("host:port");
        let config = CassandraConfig {
            server: ServerConfig {
                host: host.to_owned(),
                port: u16::from_str(port).expect("numeric port").into(),
                database: String::new(),
                user: user.to_owned(),
                tls: TlsMode::Disable,
                ssh: None,
                password_command: None,
            },
            local_datacenter: String::new(),
            statement_timeout: None,
        };
        servers.push((name, config, PasswordUpdate::Set(Password::new(password))));
    }
    servers
}

/// DynamoDB Local, from `DBROVER_TEST_DYNAMODB_URL` (its endpoint), signed in to as a local
/// emulator: no AWS account or file is ever read.
pub fn dynamodb_test_config() -> Option<DynamoDbConfig> {
    let Ok(endpoint) = std::env::var("DBROVER_TEST_DYNAMODB_URL") else {
        eprintln!("skipping: DBROVER_TEST_DYNAMODB_URL is not set");
        return None;
    };
    Some(DynamoDbConfig {
        sign_in: DynamoDbSignIn::LocalEmulator,
        profile: String::new(),
        region: String::new(),
        endpoint,
        cost_guard: dbrover_core::CostGuard::AskBeforeScans,
    })
}

/// Creates a table keyed by a string `pk` on the DynamoDB server `config` names, with a name
/// unique to the run, which PartiQL can't do; returns its name.
pub fn create_dynamodb_table(config: &DynamoDbConfig, prefix: &str) -> String {
    use aws_sdk_dynamodb::types::{
        AttributeDefinition, BillingMode, KeySchemaElement, KeyType, ScalarAttributeType,
    };
    let name = format!("{prefix}-{}", uuid::Uuid::new_v4().simple());
    let sign_in = dbrover_cloud::aws::AwsSignIn {
        credentials: dbrover_cloud::aws::AwsCredentials::LocalEmulator,
        region: None,
        endpoint: Some(config.endpoint.clone()),
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let client = aws_sdk_dynamodb::Client::new(&sign_in.sdk_config().await.unwrap());
        client
            .create_table()
            .table_name(&name)
            .billing_mode(BillingMode::PayPerRequest)
            .key_schema(
                KeySchemaElement::builder()
                    .attribute_name("pk")
                    .key_type(KeyType::Hash)
                    .build()
                    .unwrap(),
            )
            .attribute_definitions(
                AttributeDefinition::builder()
                    .attribute_name("pk")
                    .attribute_type(ScalarAttributeType::S)
                    .build()
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();
    });
    name
}

/// The `ssh` compose service and the PostgreSQL service as it reaches it, from
/// `DBROVER_TEST_SSH_URL` (`ssh://user:password@host:port`) and `DBROVER_TEST_SSH_POSTGRES_URL`.
pub struct SshTestServer {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Password,
    pub postgres: ParsedUrl,
}

impl SshTestServer {
    /// Signing in as the test user, with `login`.
    pub fn ssh(&self, login: SshLogin) -> SshConfig {
        SshConfig {
            host: self.host.clone(),
            port: self.port.into(),
            user: self.user.clone(),
            login,
        }
    }

    /// The test client key, in docker/ssh, that the server accepts.
    pub fn key_login(&self) -> SshLogin {
        SshLogin::KeyFile {
            key_file: docker_ssh("client_ed25519"),
        }
    }

    /// A `known_hosts` line trusting the test server's host key.
    pub fn trusted_line(&self) -> String {
        let key = std::fs::read_to_string(docker_ssh("ssh_host_ed25519_key.pub")).unwrap();
        let mut fields = key.split_whitespace();
        let (key_type, key_data) = (fields.next().unwrap(), fields.next().unwrap());
        format!("[{}]:{} {key_type} {key_data}\n", self.host, self.port)
    }

    /// A PostgreSQL data source reached through the SSH server with `login`.
    pub fn postgres_data_source(&self, login: SshLogin) -> DataSource {
        DataSource {
            id: DataSourceId::generate(),
            name: "through ssh".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Postgres(PostgresConfig {
                server: ServerConfig {
                    ssh: Some(self.ssh(login)),
                    ..self.postgres.config.server.clone()
                },
            }),
        }
    }
}

pub fn ssh_test_server() -> Option<SshTestServer> {
    let (Ok(ssh), Ok(postgres)) = (
        std::env::var("DBROVER_TEST_SSH_URL"),
        std::env::var("DBROVER_TEST_SSH_POSTGRES_URL"),
    ) else {
        eprintln!("skipped: set DBROVER_TEST_SSH_URL and DBROVER_TEST_SSH_POSTGRES_URL");
        return None;
    };
    let rest = ssh.strip_prefix("ssh://").expect("ssh:// URL");
    let (credentials, address) = rest.split_once('@').expect("user:password@ in URL");
    let (user, password) = credentials.split_once(':').expect("password in URL");
    let (host, port) = address.split_once(':').expect("host:port in URL");
    Some(SshTestServer {
        host: host.to_owned(),
        port: port.parse().expect("numeric port"),
        user: user.to_owned(),
        password: Password::new(password),
        postgres: parse_test_url(&postgres),
    })
}

/// A file of the `ssh` compose service's, in docker/ssh.
pub fn docker_ssh(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docker/ssh")
        .join(file)
}

pub struct ParsedUrl {
    pub config: PostgresConfig,
    pub password: Password,
}

/// Parses `postgres://user:password@host:port/database` without pulling tokio-postgres
/// into this crate's dev-dependencies just for its URL parser.
fn parse_test_url(url: &str) -> ParsedUrl {
    let rest = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))
        .expect("postgres:// URL");
    let (server, password) = parse_server(rest, "5432");
    ParsedUrl {
        config: PostgresConfig { server },
        password,
    }
}

/// `user:password@host[:port]/database`, the part of a URL after its scheme.
fn parse_server(rest: &str, default_port: &str) -> (ServerConfig, Password) {
    let (credentials, rest) = rest.split_once('@').expect("user:password@ in URL");
    let (user, password) = credentials.split_once(':').expect("password in URL");
    let (host_port, database) = rest.split_once('/').expect("/database in URL");
    let (host, port) = host_port
        .split_once(':')
        .unwrap_or((host_port, default_port));
    let server = ServerConfig {
        host: host.to_owned(),
        port: u16::from_str(port).expect("numeric port").into(),
        database: database.to_owned(),
        user: user.to_owned(),
        tls: TlsMode::default(),
        ssh: None,
        password_command: None,
    };
    (server, Password::new(password))
}

/// A port on this machine where every connection is accepted and closed at once, so a
/// connection attempt fails straight away. A port nothing listens on isn't enough: some
/// machines (CI runners among them) drop the attempt instead of refusing it, and it then
/// waits for the connect timeout.
pub fn closing_server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            drop(connection);
        }
    });
    port
}

/// The SQL dialect of a driver, which every driver with a test server has.
pub fn sql_dialect(driver: DriverKind) -> Dialect {
    QueryLanguage::from(driver)
        .dialect()
        .expect("every driver with a test server speaks SQL")
}
