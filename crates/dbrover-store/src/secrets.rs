use std::sync::Arc;

use dbrover_core::{DataSourceId, Password};

use crate::paths::APP_NAME;

/// Where data source passwords are kept. An abstraction so tests and other platforms can
/// swap the OS keychain for something else.
pub trait SecretStore: Send + Sync + 'static {
    /// The saved secret, or `None` if there isn't one.
    fn get(&self, id: DataSourceId, secret: Secret) -> Result<Option<Password>, SecretError>;

    fn set(&self, id: DataSourceId, secret: Secret, value: &Password) -> Result<(), SecretError>;

    /// Removes the saved secret. Succeeds if there was none.
    fn delete(&self, id: DataSourceId, secret: Secret) -> Result<(), SecretError>;
}

/// The secrets a data source can have. Each is an entry of its own, so none replaces another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Secret {
    /// The password the data source signs in to its database with.
    Database,
    /// The password of its MCP login (`DataSource::mcp_login`).
    McpLogin,
    /// What its SSH login needs: the SSH user's password, or the key file's passphrase.
    Ssh,
}

impl Secret {
    pub const ALL: [Self; 3] = [Self::Database, Self::McpLogin, Self::Ssh];
}

#[derive(Debug, thiserror::Error)]
#[error("could not access the OS keychain")]
pub struct SecretError(#[source] keyring::Error);

impl SecretError {
    /// A store that can't be used, for [`SecretStore`]s other than the OS keychain (such as
    /// tests standing in for a locked one).
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self(keyring::Error::PlatformFailure(reason.into().into()))
    }
}

/// The platform credential store, where the app keeps passwords.
pub fn os_keychain() -> Arc<dyn SecretStore> {
    Arc::new(Keychain { service: APP_NAME })
}

/// The platform credential store: Keychain on macOS, Credential Manager on Windows,
/// Secret Service on Linux. Entries are named `<service>` / `<data source id>`, then `/mcp` for
/// an MCP login's password and `/ssh` for the SSH login's secret. The service is the app's
/// name.
#[derive(Debug, Clone, Copy)]
struct Keychain {
    service: &'static str,
}

impl Keychain {
    fn entry(&self, id: DataSourceId, secret: Secret) -> Result<keyring::Entry, SecretError> {
        let account = match secret {
            Secret::Database => id.to_string(),
            Secret::McpLogin => format!("{id}/mcp"),
            Secret::Ssh => format!("{id}/ssh"),
        };
        keyring::Entry::new(self.service, &account).map_err(SecretError)
    }
}

impl SecretStore for Keychain {
    fn get(&self, id: DataSourceId, secret: Secret) -> Result<Option<Password>, SecretError> {
        match self.entry(id, secret)?.get_password() {
            Ok(password) => Ok(Some(Password::new(password))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretError(error)),
        }
    }

    fn set(&self, id: DataSourceId, secret: Secret, value: &Password) -> Result<(), SecretError> {
        self.entry(id, secret)?
            .set_password(value.expose())
            .map_err(SecretError)
    }

    fn delete(&self, id: DataSourceId, secret: Secret) -> Result<(), SecretError> {
        match self.entry(id, secret)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SecretError(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "uses the real OS keychain"]
    fn keeps_each_secret_apart_in_the_os_keychain() {
        let id = DataSourceId::generate();
        let value = |secret: Secret| Password::new(format!("DbRover keychain test, {secret:?}"));

        let keychain = Keychain { service: APP_NAME };
        for secret in Secret::ALL {
            keychain.set(id, secret, &value(secret)).unwrap();
        }
        let read_back: Vec<_> = Secret::ALL
            .iter()
            .map(|secret| keychain.get(id, *secret))
            .collect();
        let deleted: Vec<_> = Secret::ALL
            .iter()
            .map(|secret| keychain.delete(id, *secret))
            .collect();

        for (secret, read) in Secret::ALL.into_iter().zip(read_back) {
            assert_eq!(read.unwrap(), Some(value(secret)));
        }
        for result in deleted {
            result.unwrap();
        }
        assert_eq!(keychain.get(id, Secret::Ssh).unwrap(), None);
        keychain.delete(id, Secret::Ssh).unwrap();
    }
}
