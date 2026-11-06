//! Managed services in the data source form (C1f, `docs/cloud-data.md`): a service's preset
//! fills in what it decides (port, TLS, sign-in), and the host typed names the service.

use dbrover_core::{ManagedService, ServicePreset, ServiceSignIn, ServiceTls};

use super::{DataSourceForm, DriverFields, FormError, ServerFields, TlsChoice};

impl DataSourceForm {
    /// A new data source's form, filled in for `service`.
    pub fn for_service(service: ManagedService) -> Self {
        let mut form = Self::new(service.driver());
        form.apply_service(service);
        form
    }

    /// The services of the form's engine, empty for engines with none.
    pub fn services(&self) -> impl Iterator<Item = ManagedService> + use<> {
        ManagedService::of_driver(self.driver())
    }

    /// The service the data source is on: the one its host names, else the one last chosen
    /// while the host doesn't name another.
    pub fn service(&self) -> Option<ManagedService> {
        let server = self.fields.server()?;
        ManagedService::recognise(self.driver(), &server.host).or(server.chosen_service)
    }

    /// Fills in `service`'s port, TLS mode and sign-in, and its database and user where the
    /// form still has the engine's own; what the user typed is kept.
    pub fn apply_service(&mut self, service: ManagedService) {
        if service.driver() != self.driver() {
            return;
        }
        if let Some(server) = self.fields.server_mut() {
            server.apply_preset(&service.preset());
            server.chosen_service = Some(service);
        }
    }

    /// Forgets the service last chosen, so the form names one only if its host does.
    pub fn forget_service(&mut self) {
        if let Some(server) = self.fields.server_mut() {
            server.chosen_service = None;
        }
    }

    /// Refuses what the service can't take: Fabric signs in only with Microsoft Entra ID.
    pub(super) fn check_service(&self) -> Result<(), FormError> {
        let Some(server) = self.fields.server() else {
            return Ok(());
        };
        match self.service().map(|service| service.preset().sign_in) {
            Some(ServiceSignIn::Only(provider)) if server.cloud_provider != Some(provider) => {
                Err(FormError::CloudSignInRequired(provider))
            }
            _ => Ok(()),
        }
    }
}

impl DriverFields {
    fn server_mut(&mut self) -> Option<&mut ServerFields> {
        match self {
            Self::Postgres(fields) => Some(&mut fields.server),
            Self::Mysql(fields) => Some(&mut fields.server),
            Self::SqlServer(fields) => Some(&mut fields.server),
            Self::ClickHouse(fields) => Some(&mut fields.server),
            Self::Redis(fields) => Some(&mut fields.server),
            Self::Mongo(fields) => Some(&mut fields.server),
            Self::Elasticsearch(fields) => Some(&mut fields.server),
            Self::Cassandra(fields) => Some(&mut fields.server),
            Self::Trino(fields) => Some(&mut fields.server),
            Self::Sqlite(_) | Self::DuckDb(_) | Self::DynamoDb(_) | Self::Athena(_) => None,
        }
    }
}

impl ServerFields {
    fn apply_preset(&mut self, preset: &ServicePreset) {
        let defaults = self.defaults;
        let untouched = |value: &str, default: &str| value.trim() == default;
        if untouched(&self.host, defaults.host) {
            self.host.clear();
        }
        if untouched(&self.database, defaults.database) {
            preset.database.clone_into(&mut self.database);
        }
        if untouched(&self.user, defaults.user) {
            preset.user.clone_into(&mut self.user);
        }
        self.port = preset.port.to_string();
        self.tls = match preset.tls {
            ServiceTls::VerifyFull => TlsChoice::VerifyFull,
            ServiceTls::Require => TlsChoice::Require,
        };
        self.cloud_provider = match preset.sign_in {
            ServiceSignIn::Only(provider) => Some(provider),
            ServiceSignIn::PasswordOr(provider) => {
                self.cloud_provider.filter(|chosen| *chosen == provider)
            }
            ServiceSignIn::Password => None,
        };
    }

    /// The Host field's hint: an example of the service's host names, or the engine's own.
    pub fn host_hint(&self) -> &'static str {
        self.chosen_service
            .map_or(self.defaults.host, |service| service.preset().host_example)
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{
        CloudProvider, CloudSignIn, ConnectionConfig, DataSourceId, DriverKind, Port, ServerConfig,
        TlsMode,
    };

    use super::*;

    fn server(form: &DataSourceForm) -> ServerConfig {
        let data_source = form.to_data_source(DataSourceId::generate()).unwrap();
        data_source.config.server().unwrap().clone()
    }

    #[test]
    fn each_service_fills_in_its_engine_port_tls_and_sign_in() {
        for service in ManagedService::ALL {
            let mut form = DataSourceForm::for_service(service);
            let preset = service.preset();
            assert_eq!(form.driver(), service.driver(), "{service}");
            assert_eq!(form.service(), Some(service), "{service}");
            if let Some(server) = form.fields.server_mut() {
                server.host = "example.invalid".to_owned();
                if server.user.is_empty() {
                    server.user = "reader".to_owned();
                }
                if server.database.is_empty() {
                    server.database = "analytics".to_owned();
                }
            }
            let saved = server(&form);
            assert_eq!(saved.port, Port::Number(preset.port), "{service}");
            assert_eq!(saved.tls, preset.tls.mode(), "{service}");
            assert_eq!(
                saved.cloud_sign_in.as_ref().map(CloudSignIn::provider),
                preset.sign_in.initial().as_ref().map(CloudSignIn::provider),
                "{service}"
            );
        }
    }

    #[test]
    fn keeps_what_was_typed_and_replaces_the_engines_defaults() {
        let mut form = DataSourceForm::new(DriverKind::SqlServer);
        if let Some(server) = form.fields.server_mut() {
            server.database = "sales".to_owned();
        }
        form.apply_service(ManagedService::AzureSql);
        let fields = form.fields.server().unwrap();
        assert_eq!(fields.host, "");
        assert_eq!(fields.host_hint(), "myserver.database.windows.net");
        assert_eq!(fields.database, "sales");
        assert_eq!(fields.user, "", "sa is no Azure SQL login");

        let mut clickhouse = DataSourceForm::for_service(ManagedService::ClickHouseCloud);
        let fields = clickhouse.fields.server_mut().unwrap();
        assert_eq!(
            (fields.user.as_str(), fields.port.as_str()),
            ("default", "8443")
        );
        // Another engine's service changes nothing.
        clickhouse.apply_service(ManagedService::Neon);
        assert_eq!(clickhouse.driver(), DriverKind::ClickHouse);
    }

    #[test]
    fn a_typed_host_names_its_service() {
        let mut form = DataSourceForm::new(DriverKind::Postgres);
        assert_eq!(form.service(), None);
        form.fields.server_mut().unwrap().host =
            "ep-cool-name-123456-pooler.eu-central-1.aws.neon.tech".to_owned();
        assert_eq!(form.service(), Some(ManagedService::Neon));
        assert_eq!(form.services().count(), 6);
        assert_eq!(DataSourceForm::new(DriverKind::Redis).services().count(), 0);
    }

    #[test]
    fn fabric_takes_only_an_entra_id_sign_in() {
        let mut form = DataSourceForm::for_service(ManagedService::Fabric);
        let fields = form.fields.server_mut().unwrap();
        fields.host = "abc123.datawarehouse.fabric.microsoft.com".to_owned();
        fields.database = "sales".to_owned();
        fields.user = "analyst@contoso.com".to_owned();
        assert!(matches!(
            server(&form).cloud_sign_in,
            Some(CloudSignIn::Entra { .. })
        ));

        form.fields.server_mut().unwrap().cloud_provider = None;
        assert_eq!(
            form.to_data_source(DataSourceId::generate()),
            Err(FormError::CloudSignInRequired(CloudProvider::Entra))
        );
        // Azure SQL takes a password as well.
        let mut azure = DataSourceForm::for_service(ManagedService::AzureSql);
        let fields = azure.fields.server_mut().unwrap();
        fields.host = "myserver.database.windows.net".to_owned();
        fields.user = "admin".to_owned();
        fields.database = "sales".to_owned();
        let saved = azure.to_data_source(DataSourceId::generate()).unwrap();
        assert!(matches!(
            saved.config,
            ConnectionConfig::SqlServer(ref config)
                if config.server.cloud_sign_in.is_none()
                    && config.server.tls == TlsMode::VerifyFull { ca_file: None }
        ));
    }
}
