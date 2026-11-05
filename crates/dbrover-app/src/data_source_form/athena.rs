//! An Athena data source's fields: the AWS sign-in as DynamoDB's (a profile and a region, each
//! empty for AWS's own choice, or a local emulator at an endpoint), then where its queries run.
//! Nothing secret is typed here.

use dbrover_core::{AthenaConfig, AwsSignInKind, CostGuard};

use super::FormError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AthenaFields {
    pub sign_in: AwsSignInKind,
    pub profile: String,
    pub region: String,
    pub endpoint: String,
    pub workgroup: String,
    pub catalog: String,
    pub database: String,
    pub output_location: String,
    pub cost_guard: CostGuard,
}

/// A new data source's fields: names left empty show Athena's defaults as their hints, and
/// saving fills them in.
impl Default for AthenaFields {
    fn default() -> Self {
        Self {
            workgroup: String::new(),
            catalog: String::new(),
            database: String::new(),
            ..Self::from_config(&AthenaConfig::default())
        }
    }
}

impl AthenaFields {
    pub fn from_config(config: &AthenaConfig) -> Self {
        Self {
            sign_in: config.sign_in,
            profile: config.profile.clone(),
            region: config.region.clone(),
            endpoint: config.endpoint.clone(),
            workgroup: config.workgroup.clone(),
            catalog: config.catalog.clone(),
            database: config.database.clone(),
            output_location: config.output_location.clone(),
            cost_guard: config.cost_guard,
        }
    }

    /// The configuration, with each empty name back at Athena's default and no profile for an
    /// emulator, which signs in without one.
    ///
    /// # Errors
    ///
    /// [`FormError::EndpointMissing`] for an emulator without its endpoint, and
    /// [`FormError::OutputLocationNotS3`] for an output location that isn't in S3.
    pub fn to_config(&self) -> Result<AthenaConfig, FormError> {
        let endpoint = self.endpoint.trim().to_owned();
        let profile = match self.sign_in {
            AwsSignInKind::Profile => self.profile.trim().to_owned(),
            AwsSignInKind::LocalEmulator if endpoint.is_empty() => {
                return Err(FormError::EndpointMissing);
            }
            AwsSignInKind::LocalEmulator => String::new(),
        };
        let output_location = self.output_location.trim().to_owned();
        if !output_location.is_empty()
            && !output_location.starts_with("s3://")
            && !output_location.starts_with("${")
        {
            return Err(FormError::OutputLocationNotS3);
        }
        let or_default = |typed: &str, default: &str| match typed.trim() {
            "" => default.to_owned(),
            typed => typed.to_owned(),
        };
        Ok(AthenaConfig {
            sign_in: self.sign_in,
            profile,
            region: self.region.trim().to_owned(),
            workgroup: or_default(&self.workgroup, AthenaConfig::DEFAULT_WORKGROUP),
            catalog: or_default(&self.catalog, AthenaConfig::DEFAULT_CATALOG),
            database: or_default(&self.database, AthenaConfig::DEFAULT_DATABASE),
            output_location,
            endpoint,
            cost_guard: self.cost_guard,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_names_are_athenas_defaults_and_results_go_to_s3() {
        let mut fields = AthenaFields {
            profile: " analytics ".into(),
            workgroup: String::new(),
            database: " sales ".into(),
            output_location: "/tmp/results".into(),
            ..AthenaFields::default()
        };
        assert_eq!(fields.to_config(), Err(FormError::OutputLocationNotS3));
        fields.output_location = "s3://results/athena/".into();
        let config = fields.to_config().unwrap();
        assert_eq!(
            (
                config.profile.as_str(),
                config.workgroup.as_str(),
                config.catalog.as_str(),
                config.database.as_str()
            ),
            ("analytics", "primary", "AwsDataCatalog", "sales")
        );
        assert_eq!(AthenaFields::from_config(&config).database, "sales");

        fields.sign_in = AwsSignInKind::LocalEmulator;
        assert_eq!(fields.to_config(), Err(FormError::EndpointMissing));
        fields.endpoint = "http://localhost:4566".into();
        assert_eq!(fields.to_config().unwrap().profile, "");
    }
}
