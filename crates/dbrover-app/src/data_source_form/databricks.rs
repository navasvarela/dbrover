//! A Databricks data source's fields (C6): the workspace, how it signs in, the SQL warehouse
//! statements run on, and the catalog and schema they start in. The secret typed here, a
//! personal access token or a service principal's client secret, is the data source's
//! password, kept in the keychain.

use dbrover_core::{CostGuard, DatabricksConfig, DatabricksSignIn};

use super::FormError;

/// How the data source signs in, as the dialog offers it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DatabricksSignInChoice {
    #[default]
    PersonalAccessToken,
    ServicePrincipal,
    DatabricksCli,
    AzureCli,
}

impl DatabricksSignInChoice {
    /// What the secret row is called for this way of signing in, where it has one.
    pub fn secret_label(self) -> Option<&'static str> {
        match self {
            Self::PersonalAccessToken => Some("Token"),
            Self::ServicePrincipal => Some("Client secret"),
            Self::DatabricksCli | Self::AzureCli => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DatabricksFields {
    pub host: String,
    pub sign_in: DatabricksSignInChoice,
    /// Kept while another way of signing in is chosen, so switching back doesn't lose it.
    pub client_id: String,
    pub tenant: String,
    /// The token or secret as typed; empty keeps the saved one.
    pub secret: String,
    pub warehouse: String,
    pub catalog: String,
    pub schema: String,
    pub cost_guard: CostGuard,
}

impl DatabricksFields {
    pub fn from_config(config: &DatabricksConfig) -> Self {
        let (sign_in, client_id, tenant) = match &config.sign_in {
            DatabricksSignIn::PersonalAccessToken => (
                DatabricksSignInChoice::PersonalAccessToken,
                String::new(),
                String::new(),
            ),
            DatabricksSignIn::ServicePrincipal { client_id } => (
                DatabricksSignInChoice::ServicePrincipal,
                client_id.clone(),
                String::new(),
            ),
            DatabricksSignIn::DatabricksCli => (
                DatabricksSignInChoice::DatabricksCli,
                String::new(),
                String::new(),
            ),
            DatabricksSignIn::AzureCli { tenant } => (
                DatabricksSignInChoice::AzureCli,
                String::new(),
                tenant.clone().unwrap_or_default(),
            ),
        };
        Self {
            host: config.host.clone(),
            sign_in,
            client_id,
            tenant,
            secret: String::new(),
            warehouse: config.warehouse.clone(),
            catalog: config.catalog.clone(),
            schema: config.schema.clone(),
            cost_guard: config.cost_guard,
        }
    }

    /// The secret typed in the form, where this way of signing in keeps one.
    pub fn typed_secret(&self) -> Option<&str> {
        let kept = self.sign_in.secret_label().is_some();
        (kept && !self.secret.is_empty()).then_some(self.secret.as_str())
    }

    /// # Errors
    ///
    /// [`FormError::HostMissing`] and [`FormError::WarehouseMissing`] without them, and
    /// [`FormError::ClientIdMissing`] for a service principal without its client ID.
    pub fn to_config(&self) -> Result<DatabricksConfig, FormError> {
        let required = |text: &str, missing| match text.trim() {
            "" => Err(missing),
            text => Ok(text.to_owned()),
        };
        let sign_in = match self.sign_in {
            DatabricksSignInChoice::PersonalAccessToken => DatabricksSignIn::PersonalAccessToken,
            DatabricksSignInChoice::ServicePrincipal => DatabricksSignIn::ServicePrincipal {
                client_id: required(&self.client_id, FormError::ClientIdMissing)?,
            },
            DatabricksSignInChoice::DatabricksCli => DatabricksSignIn::DatabricksCli,
            DatabricksSignInChoice::AzureCli => DatabricksSignIn::AzureCli {
                tenant: Some(self.tenant.trim().to_owned()).filter(|tenant| !tenant.is_empty()),
            },
        };
        Ok(DatabricksConfig {
            host: required(&self.host, FormError::HostMissing)?,
            sign_in,
            password_command: None,
            warehouse: warehouse_id(&required(&self.warehouse, FormError::WarehouseMissing)?),
            catalog: self.catalog.trim().to_owned(),
            schema: self.schema.trim().to_owned(),
            cost_guard: self.cost_guard,
        })
    }
}

/// The warehouse's id from what was typed: the id, or the HTTP path Databricks's connection
/// details show (`/sql/1.0/warehouses/<id>`), which is what users copy.
fn warehouse_id(typed: &str) -> String {
    let typed = typed.trim_end_matches('/');
    typed
        .rsplit_once("/warehouses/")
        .map_or(typed, |(_, id)| id)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> DatabricksFields {
        DatabricksFields {
            host: " dbc-a1b2c3d4-e5f6.cloud.databricks.com ".into(),
            warehouse: "abc123def456".into(),
            catalog: "main".into(),
            ..DatabricksFields::default()
        }
    }

    #[test]
    fn each_way_of_signing_in_saves_and_reads_back() {
        for (choice, client_id, tenant) in [
            (DatabricksSignInChoice::PersonalAccessToken, "", ""),
            (DatabricksSignInChoice::ServicePrincipal, "a1b2-client", ""),
            (DatabricksSignInChoice::DatabricksCli, "", ""),
            (
                DatabricksSignInChoice::AzureCli,
                "",
                "contoso.onmicrosoft.com",
            ),
            (DatabricksSignInChoice::AzureCli, "", ""),
        ] {
            let typed = DatabricksFields {
                sign_in: choice,
                client_id: client_id.into(),
                tenant: tenant.into(),
                ..fields()
            };
            let config = typed.to_config().unwrap();
            assert_eq!(config.host, "dbc-a1b2c3d4-e5f6.cloud.databricks.com");
            assert_eq!(
                DatabricksFields::from_config(&config).to_config().unwrap(),
                config,
                "{choice:?}"
            );
        }
    }

    #[test]
    fn a_warehouses_http_path_saves_as_its_id() {
        let pasted = DatabricksFields {
            warehouse: " /sql/1.0/warehouses/abc123def456 ".into(),
            ..fields()
        };
        assert_eq!(pasted.to_config().unwrap().warehouse, "abc123def456");
    }

    #[test]
    fn what_is_missing_is_named() {
        let cases = [
            (
                DatabricksFields {
                    host: " ".into(),
                    ..fields()
                },
                FormError::HostMissing,
            ),
            (
                DatabricksFields {
                    warehouse: String::new(),
                    ..fields()
                },
                FormError::WarehouseMissing,
            ),
            (
                DatabricksFields {
                    sign_in: DatabricksSignInChoice::ServicePrincipal,
                    ..fields()
                },
                FormError::ClientIdMissing,
            ),
        ];
        for (typed, missing) in cases {
            assert_eq!(typed.to_config(), Err(missing));
        }
    }

    #[test]
    fn only_a_token_or_a_client_secret_is_kept() {
        let token = DatabricksFields {
            secret: "dapi-typed".into(),
            ..fields()
        };
        assert_eq!(token.typed_secret(), Some("dapi-typed"));
        let cli = DatabricksFields {
            sign_in: DatabricksSignInChoice::DatabricksCli,
            ..token
        };
        assert_eq!(cli.typed_secret(), None);
    }
}
