//! The managed database services DbRover reaches through a driver it already has (C1,
//! `docs/cloud-data.md`): what each one's data source starts with, and the host names that
//! recognise it.
//!
//! Nothing here is saved: a preset fills in a data source's settings, and the driver finds out
//! what a server is from the server itself, so a service reached through a tunnel or a host
//! name of its own is the same service.

use std::fmt;

use crate::{
    ClickHouseConfig, CloudProvider, CloudSignIn, ConnectionConfig, DriverKind, MysqlConfig, Port,
    PostgresConfig, ServerConfig, SqlServerConfig, TlsMode, TrinoConfig, TrinoProtocol,
};

/// A managed service of one of the engines DbRover has, in the providers' order (AWS, Google
/// Cloud, Azure, then the services tied to none of them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManagedService {
    /// Amazon RDS for PostgreSQL, and Aurora PostgreSQL, whose hosts are the same.
    RdsPostgres,
    /// Amazon RDS for MySQL and MariaDB, and Aurora MySQL.
    RdsMysql,
    /// Amazon Redshift, provisioned or Serverless: PostgreSQL's protocol, a catalog of its own.
    Redshift,
    CloudSqlPostgres,
    CloudSqlMysql,
    CloudSqlSqlServer,
    /// Azure Database for PostgreSQL, flexible server.
    AzurePostgres,
    /// Azure Database for MySQL, flexible server.
    AzureMysql,
    AzureSql,
    SynapseDedicated,
    SynapseServerless,
    /// A Fabric warehouse or SQL analytics endpoint, which no server property tells apart.
    Fabric,
    Neon,
    Supabase,
    ClickHouseCloud,
    /// Starburst's hosted Trino, on AWS, Google Cloud or Azure (C3).
    StarburstGalaxy,
}

/// Whose cloud a service runs in, for grouping services in menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceVendor {
    Aws,
    GoogleCloud,
    Azure,
    /// Services that run on more than one cloud, tied to no provider's sign-in.
    Independent,
}

/// How a service's users sign in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceSignIn {
    Password,
    /// A password, or a token from the provider's sign-in instead (C0d).
    PasswordOr(CloudProvider),
    /// Only a token: Fabric takes no password.
    Only(CloudProvider),
}

/// What a new data source for a service starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServicePreset {
    pub engine: ServiceEngine,
    pub port: u16,
    pub tls: ServiceTls,
    pub sign_in: ServiceSignIn,
    /// The database a new account has, where every account has the same one; empty where it
    /// is the user's own choice.
    pub database: &'static str,
    /// The user every account has, where there is one; empty otherwise.
    pub user: &'static str,
    /// A host name of the service's form, shown as the Host field's hint.
    pub host_example: &'static str,
}

/// The engines managed services run, the ones whose drivers [`ManagedService`] knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceEngine {
    Postgres,
    Mysql,
    SqlServer,
    ClickHouse,
    Trino,
}

impl ServiceEngine {
    pub fn driver(self) -> DriverKind {
        match self {
            Self::Postgres => DriverKind::Postgres,
            Self::Mysql => DriverKind::Mysql,
            Self::SqlServer => DriverKind::SqlServer,
            Self::ClickHouse => DriverKind::ClickHouse,
            Self::Trino => DriverKind::Trino,
        }
    }
}

/// The TLS mode a preset sets: checking the certificate where it chains to a public authority,
/// encrypting without checking where it is the provider's own, whose file the user must
/// download first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceTls {
    VerifyFull,
    Require,
}

/// Where a data source of a service is, as typed: what a preset can't know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceAddress<'a> {
    pub host: &'a str,
    pub database: &'a str,
    pub user: &'a str,
}

impl ManagedService {
    pub const ALL: [Self; 16] = [
        Self::RdsPostgres,
        Self::RdsMysql,
        Self::Redshift,
        Self::CloudSqlPostgres,
        Self::CloudSqlMysql,
        Self::CloudSqlSqlServer,
        Self::AzurePostgres,
        Self::AzureMysql,
        Self::AzureSql,
        Self::SynapseDedicated,
        Self::SynapseServerless,
        Self::Fabric,
        Self::Neon,
        Self::Supabase,
        Self::ClickHouseCloud,
        Self::StarburstGalaxy,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::RdsPostgres => "Amazon RDS or Aurora PostgreSQL",
            Self::RdsMysql => "Amazon RDS or Aurora MySQL",
            Self::Redshift => "Amazon Redshift",
            Self::CloudSqlPostgres => "Cloud SQL for PostgreSQL",
            Self::CloudSqlMysql => "Cloud SQL for MySQL",
            Self::CloudSqlSqlServer => "Cloud SQL for SQL Server",
            Self::AzurePostgres => "Azure Database for PostgreSQL",
            Self::AzureMysql => "Azure Database for MySQL",
            Self::AzureSql => "Azure SQL Database",
            Self::SynapseDedicated => "Azure Synapse dedicated SQL pool",
            Self::SynapseServerless => "Azure Synapse serverless SQL pool",
            Self::Fabric => "Microsoft Fabric",
            Self::Neon => "Neon",
            Self::Supabase => "Supabase",
            Self::ClickHouseCloud => "ClickHouse Cloud",
            Self::StarburstGalaxy => "Starburst Galaxy",
        }
    }

    pub fn vendor(self) -> ServiceVendor {
        match self {
            Self::RdsPostgres | Self::RdsMysql | Self::Redshift => ServiceVendor::Aws,
            Self::CloudSqlPostgres | Self::CloudSqlMysql | Self::CloudSqlSqlServer => {
                ServiceVendor::GoogleCloud
            }
            Self::AzurePostgres
            | Self::AzureMysql
            | Self::AzureSql
            | Self::SynapseDedicated
            | Self::SynapseServerless
            | Self::Fabric => ServiceVendor::Azure,
            Self::Neon | Self::Supabase | Self::ClickHouseCloud | Self::StarburstGalaxy => {
                ServiceVendor::Independent
            }
        }
    }

    pub fn preset(self) -> ServicePreset {
        use ServiceSignIn::{Only, Password, PasswordOr};
        use ServiceTls::{Require, VerifyFull};

        let preset = |engine, port, tls, sign_in, host_example| ServicePreset {
            engine,
            port,
            tls,
            sign_in,
            database: "",
            user: "",
            host_example,
        };
        let postgres = |tls, sign_in, host_example| ServicePreset {
            database: "postgres",
            ..preset(
                ServiceEngine::Postgres,
                PostgresConfig::DEFAULT_PORT,
                tls,
                sign_in,
                host_example,
            )
        };
        let mysql = |tls, sign_in, host_example| {
            preset(
                ServiceEngine::Mysql,
                MysqlConfig::DEFAULT_PORT,
                tls,
                sign_in,
                host_example,
            )
        };
        let sql_server = |tls, sign_in, host_example| {
            preset(
                ServiceEngine::SqlServer,
                SqlServerConfig::DEFAULT_PORT,
                tls,
                sign_in,
                host_example,
            )
        };
        let entra = PasswordOr(CloudProvider::Entra);
        match self {
            Self::RdsPostgres => postgres(
                Require,
                PasswordOr(CloudProvider::Aws),
                "mydb.abc123xyz.eu-west-1.rds.amazonaws.com",
            ),
            Self::RdsMysql => mysql(
                Require,
                PasswordOr(CloudProvider::Aws),
                "mydb.abc123xyz.eu-west-1.rds.amazonaws.com",
            ),
            // `dev` is the database every cluster and workgroup starts with.
            Self::Redshift => ServicePreset {
                database: "dev",
                ..preset(
                    ServiceEngine::Postgres,
                    REDSHIFT_PORT,
                    VerifyFull,
                    Password,
                    "examplecluster.abc123xyz.eu-west-1.redshift.amazonaws.com",
                )
            },
            Self::CloudSqlPostgres => postgres(
                Require,
                PasswordOr(CloudProvider::Google),
                "the instance's IP address",
            ),
            Self::CloudSqlMysql => mysql(
                Require,
                PasswordOr(CloudProvider::Google),
                "the instance's IP address",
            ),
            Self::CloudSqlSqlServer => sql_server(Require, Password, "the instance's IP address"),
            Self::AzurePostgres => {
                postgres(VerifyFull, entra, "myserver.postgres.database.azure.com")
            }
            Self::AzureMysql => mysql(VerifyFull, entra, "myserver.mysql.database.azure.com"),
            Self::AzureSql => sql_server(VerifyFull, entra, "myserver.database.windows.net"),
            Self::SynapseDedicated => {
                sql_server(VerifyFull, entra, "myworkspace.sql.azuresynapse.net")
            }
            Self::SynapseServerless => sql_server(
                VerifyFull,
                entra,
                "myworkspace-ondemand.sql.azuresynapse.net",
            ),
            Self::Fabric => sql_server(
                VerifyFull,
                Only(CloudProvider::Entra),
                "abc123xyz.datawarehouse.fabric.microsoft.com",
            ),
            Self::Neon => ServicePreset {
                database: "neondb",
                ..postgres(
                    VerifyFull,
                    Password,
                    "ep-cool-name-123456-pooler.eu-central-1.aws.neon.tech",
                )
            },
            // The session pooler's port, which keeps a server connection for the whole
            // session, as a desktop client wants; 6543 is the transaction pooler (C1c).
            Self::Supabase => postgres(Require, Password, "aws-0-eu-west-1.pooler.supabase.com"),
            Self::ClickHouseCloud => ServicePreset {
                database: "default",
                user: "default",
                ..preset(
                    ServiceEngine::ClickHouse,
                    ClickHouseConfig::DEFAULT_TLS_PORT,
                    VerifyFull,
                    Password,
                    "abc123xyz.eu-west-1.aws.clickhouse.cloud",
                )
            },
            // A cluster's HTTPS address, with a user that names the user and its role
            // (`ada@example.com/accountadmin`) and the user's password; no catalog is chosen
            // for it.
            Self::StarburstGalaxy => preset(
                ServiceEngine::Trino,
                TrinoConfig::DEFAULT_TLS_PORT,
                VerifyFull,
                Password,
                "mycluster.trino.galaxy.starburst.io",
            ),
        }
    }

    /// The endings of the host names the service gives its servers. Cloud SQL's are IP
    /// addresses unless the instance has a DNS name (`*.sql.goog`).
    fn host_suffixes(self) -> &'static [&'static str] {
        match self {
            Self::RdsPostgres | Self::RdsMysql => &[".rds.amazonaws.com", ".rds.amazonaws.com.cn"],
            Self::Redshift => &[
                ".redshift.amazonaws.com",
                ".redshift-serverless.amazonaws.com",
                ".redshift.amazonaws.com.cn",
            ],
            Self::CloudSqlPostgres | Self::CloudSqlMysql | Self::CloudSqlSqlServer => {
                &[".sql.goog"]
            }
            Self::AzurePostgres => &[".postgres.database.azure.com"],
            Self::AzureMysql => &[".mysql.database.azure.com"],
            Self::AzureSql => &[".database.windows.net"],
            Self::SynapseDedicated => &[".sql.azuresynapse.net"],
            Self::SynapseServerless => &["-ondemand.sql.azuresynapse.net"],
            Self::Fabric => &[".datawarehouse.fabric.microsoft.com"],
            Self::Neon => &[".neon.tech"],
            Self::Supabase => &[".supabase.co", ".supabase.com"],
            Self::ClickHouseCloud => &[".clickhouse.cloud"],
            Self::StarburstGalaxy => &[".galaxy.starburst.io"],
        }
    }

    /// The service `driver`'s data source on `host` is on, if its host name says so.
    ///
    /// The longest matching ending wins, so a Synapse serverless pool's
    /// `-ondemand.sql.azuresynapse.net` isn't taken for a dedicated pool's
    /// `.sql.azuresynapse.net`; the driver tells RDS for PostgreSQL from RDS for MySQL, whose
    /// hosts look alike.
    pub fn recognise(driver: DriverKind, host: &str) -> Option<Self> {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .filter(|service| service.driver() == driver)
            .filter_map(|service| {
                service
                    .host_suffixes()
                    .iter()
                    .filter(|suffix| host.ends_with(*suffix) && host.len() > suffix.len())
                    .map(|suffix| suffix.len())
                    .max()
                    .map(|length| (length, service))
            })
            .max_by_key(|(length, _)| *length)
            .map(|(_, service)| service)
    }

    /// The services of `driver`'s engine, in the providers' order.
    pub fn of_driver(driver: DriverKind) -> impl Iterator<Item = Self> {
        Self::ALL
            .into_iter()
            .filter(move |service| service.driver() == driver)
    }

    pub fn driver(self) -> DriverKind {
        self.preset().engine.driver()
    }

    /// A data source's settings for the service at `address`.
    pub fn connection_config(self, address: ServiceAddress<'_>) -> ConnectionConfig {
        let preset = self.preset();
        let server = ServerConfig {
            host: address.host.to_owned(),
            port: Port::Number(preset.port),
            database: address.database.to_owned(),
            user: address.user.to_owned(),
            tls: preset.tls.mode(),
            ssh: None,
            password_command: None,
            cloud_sign_in: preset.sign_in.initial(),
        };
        match preset.engine {
            ServiceEngine::Postgres => ConnectionConfig::Postgres(PostgresConfig { server }),
            ServiceEngine::Mysql => ConnectionConfig::Mysql(MysqlConfig { server }),
            ServiceEngine::SqlServer => ConnectionConfig::SqlServer(SqlServerConfig { server }),
            ServiceEngine::ClickHouse => ConnectionConfig::ClickHouse(ClickHouseConfig {
                server,
                read_only: false,
            }),
            ServiceEngine::Trino => ConnectionConfig::Trino(TrinoConfig {
                server,
                schema: String::new(),
                protocol: TrinoProtocol::Trino,
            }),
        }
    }
}

/// Redshift's port since it launched, rather than PostgreSQL's 5432.
const REDSHIFT_PORT: u16 = 5439;

impl ServiceTls {
    pub fn mode(self) -> TlsMode {
        match self {
            Self::VerifyFull => TlsMode::VerifyFull { ca_file: None },
            Self::Require => TlsMode::Require,
        }
    }
}

impl ServiceSignIn {
    /// The cloud sign-in a new data source starts with: none where a password will do.
    pub fn initial(self) -> Option<CloudSignIn> {
        match self {
            Self::Password | Self::PasswordOr(_) => None,
            Self::Only(provider) => Some(provider.default_sign_in()),
        }
    }

    pub fn takes_password(self) -> bool {
        !matches!(self, Self::Only(_))
    }
}

impl ServiceVendor {
    pub const ALL: [Self; 4] = [Self::Aws, Self::GoogleCloud, Self::Azure, Self::Independent];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Aws => "Amazon Web Services",
            Self::GoogleCloud => "Google Cloud",
            Self::Azure => "Microsoft Azure",
            Self::Independent => "Other cloud services",
        }
    }

    pub fn services(self) -> impl Iterator<Item = ManagedService> {
        ManagedService::ALL
            .into_iter()
            .filter(move |service| service.vendor() == self)
    }

    /// The engines of DbRover's own that only this cloud runs, such as DynamoDB and Athena
    /// on AWS, which its menu offers beside the managed services.
    pub fn engines(self) -> &'static [DriverKind] {
        match self {
            Self::Aws => &[DriverKind::DynamoDb, DriverKind::Athena],
            Self::GoogleCloud | Self::Azure | Self::Independent => &[],
        }
    }
}

impl fmt::Display for ManagedService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DataSource, DataSourceId, SafetyLevel};

    use DriverKind::{ClickHouse, Mysql, Postgres, SqlServer, Trino};
    use ServiceSignIn::{Only, Password, PasswordOr};
    use ServiceTls::{Require, VerifyFull};

    #[test]
    fn each_service_starts_with_its_driver_port_tls_and_sign_in() {
        let aws = PasswordOr(CloudProvider::Aws);
        let google = PasswordOr(CloudProvider::Google);
        let entra = PasswordOr(CloudProvider::Entra);
        let expected = [
            (ManagedService::RdsPostgres, Postgres, 5432, Require, aws),
            (ManagedService::RdsMysql, Mysql, 3306, Require, aws),
            (
                ManagedService::Redshift,
                Postgres,
                5439,
                VerifyFull,
                Password,
            ),
            (
                ManagedService::CloudSqlPostgres,
                Postgres,
                5432,
                Require,
                google,
            ),
            (ManagedService::CloudSqlMysql, Mysql, 3306, Require, google),
            (
                ManagedService::CloudSqlSqlServer,
                SqlServer,
                1433,
                Require,
                Password,
            ),
            (
                ManagedService::AzurePostgres,
                Postgres,
                5432,
                VerifyFull,
                entra,
            ),
            (ManagedService::AzureMysql, Mysql, 3306, VerifyFull, entra),
            (ManagedService::AzureSql, SqlServer, 1433, VerifyFull, entra),
            (
                ManagedService::SynapseDedicated,
                SqlServer,
                1433,
                VerifyFull,
                entra,
            ),
            (
                ManagedService::SynapseServerless,
                SqlServer,
                1433,
                VerifyFull,
                entra,
            ),
            (
                ManagedService::Fabric,
                SqlServer,
                1433,
                VerifyFull,
                Only(CloudProvider::Entra),
            ),
            (ManagedService::Neon, Postgres, 5432, VerifyFull, Password),
            (ManagedService::Supabase, Postgres, 5432, Require, Password),
            (
                ManagedService::ClickHouseCloud,
                ClickHouse,
                8443,
                VerifyFull,
                Password,
            ),
            (
                ManagedService::StarburstGalaxy,
                Trino,
                443,
                VerifyFull,
                Password,
            ),
        ];
        assert_eq!(expected.len(), ManagedService::ALL.len());
        for (service, driver, port, tls, sign_in) in expected {
            let preset = service.preset();
            assert_eq!(
                (service.driver(), preset.port, preset.tls, preset.sign_in),
                (driver, port, tls, sign_in),
                "{service}"
            );
        }
    }

    #[test]
    fn every_cloud_sign_in_a_service_takes_is_one_its_driver_offers() {
        for service in ManagedService::ALL {
            let preset = service.preset();
            if let ServiceSignIn::PasswordOr(provider) | ServiceSignIn::Only(provider) =
                preset.sign_in
            {
                assert!(
                    service.driver().cloud_providers().contains(&provider),
                    "{service}"
                );
            }
        }
    }

    #[test]
    fn recognises_each_services_host_names() {
        for (driver, host, service) in [
            (
                Postgres,
                "mydb.abc123xyz.eu-west-1.rds.amazonaws.com",
                Some(ManagedService::RdsPostgres),
            ),
            (
                Postgres,
                "mycluster.cluster-ro-abc123xyz.us-east-1.rds.amazonaws.com",
                Some(ManagedService::RdsPostgres),
            ),
            (
                Mysql,
                "mydb.abc123xyz.cn-north-1.rds.amazonaws.com.cn",
                Some(ManagedService::RdsMysql),
            ),
            (
                Postgres,
                "examplecluster.abc123xyz.us-west-2.redshift.amazonaws.com",
                Some(ManagedService::Redshift),
            ),
            (
                Postgres,
                "default-workgroup.123456789012.us-east-1.redshift-serverless.amazonaws.com",
                Some(ManagedService::Redshift),
            ),
            (
                Postgres,
                "abc123.eu-west1.sql.goog",
                Some(ManagedService::CloudSqlPostgres),
            ),
            (
                SqlServer,
                "abc123.eu-west1.sql.goog.",
                Some(ManagedService::CloudSqlSqlServer),
            ),
            (
                Postgres,
                "MyServer.Postgres.Database.Azure.com",
                Some(ManagedService::AzurePostgres),
            ),
            (
                Mysql,
                "myserver.mysql.database.azure.com",
                Some(ManagedService::AzureMysql),
            ),
            (
                SqlServer,
                "myserver.database.windows.net",
                Some(ManagedService::AzureSql),
            ),
            (
                SqlServer,
                "myworkspace.sql.azuresynapse.net",
                Some(ManagedService::SynapseDedicated),
            ),
            (
                SqlServer,
                "myworkspace-ondemand.sql.azuresynapse.net",
                Some(ManagedService::SynapseServerless),
            ),
            (
                SqlServer,
                "abc123xyz-def456.datawarehouse.fabric.microsoft.com",
                Some(ManagedService::Fabric),
            ),
            (
                Postgres,
                "ep-cool-name-123456-pooler.eu-central-1.aws.neon.tech",
                Some(ManagedService::Neon),
            ),
            (
                Postgres,
                "db.abcdefghijklmnop.supabase.co",
                Some(ManagedService::Supabase),
            ),
            (
                Postgres,
                "aws-0-eu-west-1.pooler.supabase.com",
                Some(ManagedService::Supabase),
            ),
            (
                ClickHouse,
                "abc123xyz.eu-west-1.aws.clickhouse.cloud",
                Some(ManagedService::ClickHouseCloud),
            ),
            (
                Trino,
                "mycluster-free.trino.galaxy.starburst.io",
                Some(ManagedService::StarburstGalaxy),
            ),
            (Trino, "trino.internal.example.com", None),
            // Another engine's host, a look-alike, a bare suffix and a plain server.
            (Mysql, "myserver.postgres.database.azure.com", None),
            (Postgres, "myserver.database.windows.net", None),
            (Postgres, "evil-neon.tech", None),
            (Postgres, "neon.tech", None),
            (Postgres, "localhost", None),
            (SqlServer, "10.0.0.4", None),
        ] {
            assert_eq!(ManagedService::recognise(driver, host), service, "{host}");
        }
    }

    #[test]
    fn every_service_is_in_its_vendors_group_once() {
        let grouped: Vec<ManagedService> = ServiceVendor::ALL
            .into_iter()
            .flat_map(ServiceVendor::services)
            .collect();
        assert_eq!(grouped, ManagedService::ALL);
    }

    #[test]
    fn a_services_settings_save_and_read_back() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct File {
            data_source: Vec<DataSource>,
        }

        let address = ServiceAddress {
            host: "example.invalid",
            database: "analytics",
            user: "reader",
        };
        let file = File {
            data_source: ManagedService::ALL
                .into_iter()
                .map(|service| DataSource {
                    id: DataSourceId::generate(),
                    name: service.display_name().to_owned(),
                    color: None,
                    safety: SafetyLevel::Normal,
                    project: None,
                    mcp_login: None,
                    config: service.connection_config(address),
                })
                .collect(),
        };
        let text = toml::to_string(&file).unwrap();
        assert_eq!(toml::from_str::<File>(&text).unwrap(), file);

        let fabric = ManagedService::Fabric.connection_config(address);
        let server = fabric.server().unwrap();
        assert_eq!(fabric.driver(), SqlServer);
        assert_eq!(server.host, "example.invalid");
        assert_eq!(server.tls, TlsMode::VerifyFull { ca_file: None });
        assert_eq!(
            server.cloud_sign_in.as_ref().map(CloudSignIn::provider),
            Some(CloudProvider::Entra)
        );
        assert!(
            ManagedService::RdsPostgres
                .connection_config(address)
                .server()
                .unwrap()
                .cloud_sign_in
                .is_none()
        );
    }
}
