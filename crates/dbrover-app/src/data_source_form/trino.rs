//! A Trino, Starburst or Presto data source's fields: the shared server form, with the catalog
//! a console starts in in the database's place, then its schema and the protocol's spelling.

use dbrover_core::{TrinoConfig, TrinoProtocol};

use super::server::{DatabaseField, ServerDefaults, TlsOffer, UserField};
use super::{FormError, ServerFields};

/// HTTPS is a port of its own, 443 behind most load balancers and on Starburst Galaxy. Trino
/// asks for a user on every request, whatever signs in.
pub const TRINO_DEFAULTS: ServerDefaults = ServerDefaults {
    host: "localhost",
    port: TrinoConfig::DEFAULT_PORT,
    database: "",
    database_field: DatabaseField::Catalog,
    user: "trino",
    user_field: UserField::Required,
    tls: TlsOffer::OwnPort(TrinoConfig::DEFAULT_TLS_PORT),
    cloud: &[],
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrinoFields {
    pub server: ServerFields,
    pub schema: String,
    pub protocol: TrinoProtocol,
}

impl TrinoFields {
    pub fn new() -> Self {
        Self {
            server: ServerFields::new(TRINO_DEFAULTS),
            schema: String::new(),
            protocol: TrinoProtocol::default(),
        }
    }

    pub fn from_config(config: &TrinoConfig) -> Self {
        Self {
            server: ServerFields::from_config(&config.server, TRINO_DEFAULTS),
            schema: config.schema.clone(),
            protocol: config.protocol,
        }
    }

    pub fn to_config(&self) -> Result<TrinoConfig, FormError> {
        Ok(TrinoConfig {
            server: self.server.to_config()?,
            schema: self.schema.trim().to_owned(),
            protocol: self.protocol,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_and_schema_may_be_left_empty() {
        let mut fields = TrinoFields::new();
        assert_eq!(fields.server.port, "8080");
        let config = fields.to_config().unwrap();
        assert_eq!(
            (config.server.database.as_str(), config.schema.as_str()),
            ("", "")
        );
        fields.server.database = " memory ".into();
        fields.schema = " default ".into();
        fields.protocol = TrinoProtocol::Presto;
        let config = fields.to_config().unwrap();
        assert_eq!(
            (config.server.database.as_str(), config.schema.as_str()),
            ("memory", "default")
        );
        assert_eq!(TrinoFields::from_config(&config), fields_after(&config));
    }

    fn fields_after(config: &TrinoConfig) -> TrinoFields {
        TrinoFields::from_config(config)
    }
}
