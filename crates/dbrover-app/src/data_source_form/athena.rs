//! An Athena data source's fields: the AWS sign-in as DynamoDB's (a profile and a region, each
//! empty for AWS's own choice, or a local emulator at an endpoint), then where its queries run.
//! Nothing secret is typed here.

use dbrover_core::{AthenaConfig, AwsSignInKind, ByteCount, CostGuard};

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
    /// The most a query may scan, in gigabytes as Athena's prices count them (10⁹ bytes);
    /// empty for no limit. Saved as the cost guard's `ask_above_bytes`.
    pub limit_gb: String,
    pub cost_guard: CostGuard,
}

/// Bytes in a gigabyte, decimal, as cloud bills count them.
const GIGABYTE: u64 = 1_000_000_000;

/// The digits a gigabyte has after its point, to the byte.
const GIGABYTE_DIGITS: usize = 9;

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
            limit_gb: config
                .cost_guard
                .ask_above_bytes
                .map(gigabytes)
                .unwrap_or_default(),
            cost_guard: config.cost_guard,
        }
    }

    /// The configuration, with each empty name back at Athena's default and no profile for an
    /// emulator, which signs in without one.
    ///
    /// # Errors
    ///
    /// [`FormError::EndpointMissing`] for an emulator without its endpoint,
    /// [`FormError::OutputLocationNotS3`] for an output location that isn't in S3, and
    /// [`FormError::InvalidByteLimit`] for a limit that isn't a number of gigabytes.
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
        let limit = match self.limit_gb.trim() {
            "" => None,
            typed => Some(parse_gigabytes(typed).ok_or(FormError::InvalidByteLimit)?),
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
            cost_guard: CostGuard {
                ask_above_bytes: limit,
                ..self.cost_guard
            },
        })
    }
}

/// `1.5` for 1,500,000,000 bytes: as few digits as say it exactly.
fn gigabytes(bytes: ByteCount) -> String {
    let whole = bytes.0 / GIGABYTE;
    let fraction = bytes.0 % GIGABYTE;
    if fraction == 0 {
        return whole.to_string();
    }
    let digits = format!("{fraction:0width$}", width = GIGABYTE_DIGITS);
    format!("{whole}.{}", digits.trim_end_matches('0'))
}

/// A number of gigabytes, such as `10` or `0.5`, read exactly; `None` for anything else,
/// zero, or more than a byte's precision.
fn parse_gigabytes(text: &str) -> Option<ByteCount> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |part: &str| part.chars().all(|c| c.is_ascii_digit());
    if whole.is_empty() && fraction.is_empty()
        || !digits(whole)
        || !digits(fraction)
        || fraction.len() > GIGABYTE_DIGITS
    {
        return None;
    }
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let fraction: u64 = format!("{fraction:0<width$}", width = GIGABYTE_DIGITS)
        .parse()
        .ok()?;
    let bytes = whole.checked_mul(GIGABYTE)?.checked_add(fraction)?;
    (bytes > 0).then_some(ByteCount(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gigabytes_are_read_and_written_to_the_byte() {
        for (text, bytes) in [
            ("10", Some(10_000_000_000)),
            ("0.5", Some(500_000_000)),
            (".25", Some(250_000_000)),
            ("1.000000001", Some(1_000_000_001)),
            ("1.0000000001", None),
            ("0", None),
            ("-1", None),
            ("1e3", None),
            (".", None),
        ] {
            assert_eq!(parse_gigabytes(text), bytes.map(ByteCount), "{text}");
        }
        assert_eq!(gigabytes(ByteCount(10_000_000_000)), "10");
        assert_eq!(gigabytes(ByteCount(1_000_000_001)), "1.000000001");
    }

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

        fields.limit_gb = "ten".into();
        assert_eq!(fields.to_config(), Err(FormError::InvalidByteLimit));
        fields.limit_gb = " 1.5 ".into();
        let config = fields.to_config().unwrap();
        assert_eq!(
            config.cost_guard.ask_above_bytes,
            Some(ByteCount(1_500_000_000))
        );
        assert_eq!(AthenaFields::from_config(&config).limit_gb, "1.5");
        fields.limit_gb.clear();

        fields.sign_in = AwsSignInKind::LocalEmulator;
        assert_eq!(fields.to_config(), Err(FormError::EndpointMissing));
        fields.endpoint = "http://localhost:4566".into();
        assert_eq!(fields.to_config().unwrap().profile, "");
    }
}
