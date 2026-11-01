//! Signing in to cloud providers with the sign-in already on the machine, never storing a
//! cloud secret: the groundwork C0 of `docs/cloud-data.md` plans for every cloud data source.
//!
//! AWS comes first, for DynamoDB (`docs/nosql.md`, N5). Google Cloud and Microsoft Entra ID
//! join it as modules of their own when their milestones come.

pub mod aws;
