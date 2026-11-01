//! A DynamoDB data source's fields: a profile, a region and an endpoint, each empty for AWS's
//! own choice, or a local emulator at an endpoint. Nothing secret is typed here.

use dbrover_core::{CostGuard, DynamoDbConfig, DynamoDbSignIn};

use super::FormError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DynamoDbFields {
    pub sign_in: DynamoDbSignIn,
    pub profile: String,
    pub region: String,
    pub endpoint: String,
    pub cost_guard: CostGuard,
}

impl DynamoDbFields {
    pub fn from_config(config: &DynamoDbConfig) -> Self {
        Self {
            sign_in: config.sign_in,
            profile: config.profile.clone(),
            region: config.region.clone(),
            endpoint: config.endpoint.clone(),
            cost_guard: config.cost_guard,
        }
    }

    /// The configuration, without a profile for an emulator, which signs in without one.
    ///
    /// # Errors
    ///
    /// [`FormError::EndpointMissing`] for an emulator without its endpoint.
    pub fn to_config(&self) -> Result<DynamoDbConfig, FormError> {
        let endpoint = self.endpoint.trim().to_owned();
        let profile = match self.sign_in {
            DynamoDbSignIn::Profile => self.profile.trim().to_owned(),
            DynamoDbSignIn::LocalEmulator if endpoint.is_empty() => {
                return Err(FormError::EndpointMissing);
            }
            DynamoDbSignIn::LocalEmulator => String::new(),
        };
        Ok(DynamoDbConfig {
            sign_in: self.sign_in,
            profile,
            region: self.region.trim().to_owned(),
            endpoint,
            cost_guard: self.cost_guard,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_emulator_needs_an_endpoint_and_no_profile() {
        let mut fields = DynamoDbFields {
            sign_in: DynamoDbSignIn::LocalEmulator,
            profile: "dev".into(),
            region: " eu-west-1 ".into(),
            endpoint: String::new(),
            cost_guard: CostGuard::Off,
        };
        assert_eq!(fields.to_config(), Err(FormError::EndpointMissing));
        fields.endpoint = "http://localhost:8000".into();
        let config = fields.to_config().unwrap();
        assert_eq!(
            (config.profile.as_str(), config.region.as_str()),
            ("", "eu-west-1")
        );
        fields.sign_in = DynamoDbSignIn::Profile;
        fields.endpoint.clear();
        let config = fields.to_config().unwrap();
        assert_eq!(config.profile, "dev");
        assert_eq!(config.cost_guard, CostGuard::Off);
        assert_eq!(DynamoDbFields::from_config(&config).profile, "dev");
    }
}
