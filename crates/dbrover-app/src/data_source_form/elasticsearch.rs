//! An Elasticsearch or OpenSearch data source's fields: the shared server form, without a
//! database, signing in with a user and password or with an API key in the password's place.

use dbrover_core::{ElasticsearchAuth, ElasticsearchConfig};

use super::server::{DatabaseField, ServerDefaults, TlsOffer, UserField};
use super::{FormError, ServerFields};

/// The REST API's port, plain or over TLS: a node serves one or the other on it, so there is no
/// `prefer`. The user may be left out, for a node without security or an API key.
pub const ELASTICSEARCH_DEFAULTS: ServerDefaults = ServerDefaults {
    host: "localhost",
    port: ElasticsearchConfig::DEFAULT_PORT,
    database: "",
    database_field: DatabaseField::Absent,
    user: "elastic",
    user_field: UserField::Optional,
    tls: TlsOffer::OwnPort(ElasticsearchConfig::DEFAULT_PORT),
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElasticsearchFields {
    pub server: ServerFields,
    pub auth: ElasticsearchAuth,
}

impl ElasticsearchFields {
    pub fn new() -> Self {
        Self {
            server: ServerFields::new(ELASTICSEARCH_DEFAULTS),
            auth: ElasticsearchAuth::Password,
        }
    }

    pub fn from_config(config: &ElasticsearchConfig) -> Self {
        Self {
            server: ServerFields::from_config(&config.server, ELASTICSEARCH_DEFAULTS),
            auth: config.auth,
        }
    }

    /// The configuration, without a user when an API key signs in: the key says who it is.
    pub fn to_config(&self) -> Result<ElasticsearchConfig, FormError> {
        let mut server = self.server.to_config()?;
        if self.auth == ElasticsearchAuth::ApiKey {
            server.user.clear();
        }
        Ok(ElasticsearchConfig {
            server,
            auth: self.auth,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_api_key_signs_in_without_a_user() {
        let mut fields = ElasticsearchFields::new();
        assert_eq!(fields.server.port, "9200");
        let config = fields.to_config().unwrap();
        assert_eq!(
            (config.server.user.as_str(), config.server.database.as_str()),
            ("elastic", "")
        );
        fields.auth = ElasticsearchAuth::ApiKey;
        let config = fields.to_config().unwrap();
        assert_eq!(config.server.user, "");
        assert_eq!(config.auth, ElasticsearchAuth::ApiKey);
        assert_eq!(ElasticsearchFields::from_config(&config), {
            let mut expected = fields.clone();
            expected.server.user.clear();
            expected
        });
    }
}
