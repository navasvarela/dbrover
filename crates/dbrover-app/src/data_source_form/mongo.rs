//! A MongoDB data source's fields: the shared server form with the authentication database, or
//! a connection string pasted whole, whose user and password move to their own fields.

use dbrover_core::{MongoConfig, MongoUri, ServerConfig, has_placeholder};

use super::server::{DatabaseField, ServerDefaults, TlsOffer, UserField};
use super::{FormError, ServerFields};

/// Consoles start in `test`, as the shell's do. The user may be left out for a server without
/// access control. TLS is all or nothing on a port, so there is no `prefer`.
pub const MONGO_DEFAULTS: ServerDefaults = ServerDefaults {
    host: "localhost",
    port: MongoConfig::DEFAULT_PORT,
    database: MongoConfig::DEFAULT_DATABASE,
    database_field: DatabaseField::Name,
    user: "",
    user_field: UserField::Optional,
    tls: TlsOffer::OwnPort(MongoConfig::DEFAULT_PORT),
    cloud: &[],
};

/// How a MongoDB data source says where the server is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MongoTarget {
    /// The host, port, TLS and SSH rows every server has.
    Server,
    /// A `mongodb://` or `mongodb+srv://` string, as Atlas hands out.
    ConnectionString,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MongoFields {
    pub server: ServerFields,
    pub target: MongoTarget,
    /// Empty signs in against `admin`.
    pub auth_database: String,
    /// Kept while the server form is chosen, so switching back doesn't lose it.
    pub connection_string: String,
}

impl MongoFields {
    pub fn new() -> Self {
        Self {
            server: ServerFields::new(MONGO_DEFAULTS),
            target: MongoTarget::Server,
            auth_database: String::new(),
            connection_string: String::new(),
        }
    }

    pub fn from_config(config: &MongoConfig) -> Self {
        Self {
            server: ServerFields::from_config(&config.server, MONGO_DEFAULTS),
            target: match config.connection_string {
                Some(_) => MongoTarget::ConnectionString,
                None => MongoTarget::Server,
            },
            auth_database: config.auth_database.clone(),
            connection_string: config.connection_string.clone().unwrap_or_default(),
        }
    }

    /// Moves a pasted string's user and password into their own fields, and its database into
    /// the Database field while that still holds the default, so the string is kept without
    /// them and the password goes to the keychain. Nothing changes for a string without them.
    pub fn take_credentials(&mut self) {
        let Some(uri) = MongoUri::parse(&self.connection_string) else {
            return;
        };
        if let Some(database) = &uri.database
            && self.server.database == MONGO_DEFAULTS.database
        {
            database.clone_into(&mut self.server.database);
        }
        if uri.user.is_empty() && uri.password.is_none() {
            return;
        }
        if !uri.user.is_empty() {
            self.server.user = uri.user;
        }
        if let Some(password) = uri.password {
            self.server.password = password;
        }
        self.connection_string = uri.without_credentials;
    }

    pub fn to_config(&self) -> Result<MongoConfig, FormError> {
        let auth_database = self.auth_database.trim().to_owned();
        let MongoTarget::ConnectionString = self.target else {
            return Ok(MongoConfig {
                server: self.server.to_config()?,
                auth_database,
                connection_string: None,
            });
        };
        let string = self.connection_string.trim();
        let host = if has_placeholder(string) {
            "connection string".to_owned()
        } else {
            MongoUri::parse(string)
                .ok_or(FormError::InvalidConnectionString)?
                .host
        };
        // The string says where the server is and how to encrypt; the form's host only names
        // the data source, and its tunnel and TLS aren't used.
        let mut fields = self.server.clone();
        fields.host = host;
        fields.port = MongoConfig::DEFAULT_PORT.to_string();
        fields.ssh.enabled = false;
        let server: ServerConfig = fields.to_config()?;
        Ok(MongoConfig {
            server: ServerConfig {
                tls: dbrover_core::TlsMode::Disable,
                ..server
            },
            auth_database,
            connection_string: Some(string.to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_string_keeps_its_credentials_apart() {
        let mut fields = MongoFields::new();
        fields.target = MongoTarget::ConnectionString;
        "mongodb+srv://app:s%40cret@cluster0.ab1cd.mongodb.net/shop?retryWrites=true"
            .clone_into(&mut fields.connection_string);
        fields.take_credentials();
        assert_eq!(
            (
                fields.server.user.as_str(),
                fields.server.password.as_str(),
                fields.server.database.as_str(),
                fields.connection_string.as_str(),
            ),
            (
                "app",
                "s@cret",
                "shop",
                "mongodb+srv://cluster0.ab1cd.mongodb.net/shop?retryWrites=true"
            )
        );
        let config = fields.to_config().unwrap();
        assert_eq!(config.server.host, "cluster0.ab1cd.mongodb.net");
        assert_eq!(config.server.user, "app");
        assert_eq!(
            config.connection_string.as_deref(),
            Some("mongodb+srv://cluster0.ab1cd.mongodb.net/shop?retryWrites=true")
        );
    }

    #[test]
    fn the_server_form_needs_no_user_and_refuses_a_string_that_is_not_one() {
        let config = MongoFields::new().to_config().unwrap();
        assert_eq!(
            (
                config.server.port.clone(),
                config.server.database.as_str(),
                config.server.user.as_str(),
                config.connection_string,
            ),
            (27017.into(), "test", "", None)
        );
        let mut fields = MongoFields::new();
        fields.target = MongoTarget::ConnectionString;
        "localhost:27017".clone_into(&mut fields.connection_string);
        assert_eq!(fields.to_config(), Err(FormError::InvalidConnectionString));
    }
}
