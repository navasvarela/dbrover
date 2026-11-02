//! Signing in to cloud providers with the sign-in already on the machine, never storing a
//! cloud secret: the groundwork C0 of `docs/cloud-data.md` plans for every cloud data source.
//!
//! AWS came first, for DynamoDB (`docs/nosql.md`, N5), through AWS's own SDK. Google Cloud
//! signs in with Application Default Credentials over a small HTTPS client of DbRover's own,
//! and Microsoft Entra ID through the Azure CLIs' sign-in. Both hand out [`AccessToken`]s,
//! which a [`TokenCache`] keeps until they are about to expire.

pub mod aws;
pub mod entra;
pub mod google;
mod http;
pub mod rds;
pub mod token;

use std::error::Error;

pub use http::HttpError;
#[cfg(any(test, feature = "stand-in"))]
pub use http::stand_in;
pub use token::{AccessToken, TokenCache};

/// `error`'s message followed by each of its causes', as AWS's SDK and hyper nest them: the
/// message alone often says only that a provider or a request failed.
pub(crate) fn with_causes(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}
