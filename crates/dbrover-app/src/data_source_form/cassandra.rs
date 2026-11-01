//! A Cassandra or ScyllaDB data source's fields: the shared server form, with the keyspace a
//! console starts in in the database's place, the datacenter to route requests to, and how
//! long a statement may run.

use std::num::NonZeroU32;

use dbrover_core::CassandraConfig;

use super::server::{DatabaseField, ServerDefaults, TlsOffer, UserField};
use super::{FormError, ServerFields};

/// CQL over TLS is a port of its own on most servers, 9142. The user may be left out, for a
/// cluster that doesn't ask for a password; `cassandra` is the superuser Cassandra starts with.
pub const CASSANDRA_DEFAULTS: ServerDefaults = ServerDefaults {
    host: "localhost",
    port: CassandraConfig::DEFAULT_PORT,
    database: "",
    database_field: DatabaseField::Keyspace,
    user: "cassandra",
    user_field: UserField::Optional,
    tls: TlsOffer::OwnPort(CassandraConfig::DEFAULT_TLS_PORT),
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CassandraFields {
    pub server: ServerFields,
    pub local_datacenter: String,
    /// Seconds, or empty for the server's own timeouts.
    pub statement_timeout: String,
}

impl CassandraFields {
    pub fn new() -> Self {
        Self {
            server: ServerFields::new(CASSANDRA_DEFAULTS),
            local_datacenter: String::new(),
            statement_timeout: String::new(),
        }
    }

    pub fn from_config(config: &CassandraConfig) -> Self {
        Self {
            server: ServerFields::from_config(&config.server, CASSANDRA_DEFAULTS),
            local_datacenter: config.local_datacenter.clone(),
            statement_timeout: config
                .statement_timeout
                .map(|seconds| seconds.to_string())
                .unwrap_or_default(),
        }
    }

    pub fn to_config(&self) -> Result<CassandraConfig, FormError> {
        Ok(CassandraConfig {
            server: self.server.to_config()?,
            local_datacenter: self.local_datacenter.trim().to_owned(),
            statement_timeout: parse_timeout(&self.statement_timeout)?,
        })
    }
}

fn parse_timeout(text: &str) -> Result<Option<NonZeroU32>, FormError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<NonZeroU32>()
        .map(Some)
        .map_err(|_| FormError::InvalidStatementTimeout)
}

#[cfg(test)]
mod tests {
    use super::super::TlsChoice;
    use super::*;

    #[test]
    fn keeps_an_optional_keyspace_datacenter_and_timeout() {
        let mut fields = CassandraFields::new();
        assert_eq!(fields.server.port, "9042");
        let config = fields.to_config().unwrap();
        assert_eq!(
            (
                config.server.database.as_str(),
                config.local_datacenter.as_str(),
                config.statement_timeout
            ),
            ("", "", None)
        );
        fields.server.database = "shop".to_owned();
        fields.local_datacenter = " dc1 ".to_owned();
        fields.statement_timeout = "30".to_owned();
        let config = fields.to_config().unwrap();
        assert_eq!(config.server.database, "shop");
        assert_eq!(config.local_datacenter, "dc1");
        assert_eq!(config.statement_timeout, NonZeroU32::new(30));
        let mut round_trip = fields.clone();
        round_trip.local_datacenter = "dc1".to_owned();
        assert_eq!(CassandraFields::from_config(&config), round_trip);
        for wrong in ["0", "-1", "soon"] {
            fields.statement_timeout = wrong.to_owned();
            assert_eq!(
                fields.to_config(),
                Err(FormError::InvalidStatementTimeout),
                "{wrong}"
            );
        }
    }

    #[test]
    fn tls_moves_to_its_own_port() {
        let mut fields = CassandraFields::new();
        fields.server.choose_tls(TlsChoice::VerifyFull);
        assert_eq!(fields.server.port, "9142");
    }
}
