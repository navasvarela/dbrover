//! Signing in to cloud providers with the sign-in already on the machine, never storing a
//! cloud secret: the groundwork C0 of `docs/cloud-data.md` plans for every cloud data source.
//!
//! AWS came first, for DynamoDB (`docs/nosql.md`, N5), through AWS's own SDK. Google Cloud
//! signs in with Application Default Credentials over a small HTTPS client of DbRover's own,
//! and Microsoft Entra ID through the Azure CLIs' sign-in. Both hand out [`AccessToken`]s,
//! which a [`TokenCache`] keeps until they are about to expire. Snowflake's key pair makes its
//! tokens on the machine, as JWTs signed with the user's key, and keeps them the same way, as
//! Databricks's service principals keep the tokens their workspace hands out.

pub mod aws;
pub mod databricks;
pub mod entra;
pub mod google;
mod http;
mod program;
pub mod rds;
pub mod snowflake;
pub mod storage;
pub mod token;

use std::error::Error;

#[cfg(any(test, feature = "stand-in"))]
pub use http::stand_in;
pub use http::{Answer, HttpError, Https};
pub use program::ProgramError;
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
