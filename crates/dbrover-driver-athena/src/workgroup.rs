//! What a workgroup says about the queries run in it: where their results go, and which
//! engine runs them.

use aws_sdk_athena::Client;
use aws_sdk_athena::types::WorkGroupConfiguration;
use dbrover_core::ByteCount;

use crate::errors::Refused;

/// A workgroup, as `GetWorkGroup` describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Workgroup {
    /// Where the workgroup puts results, if it names a place.
    pub output_location: Option<String>,
    /// Whether the workgroup's settings override a query's own, its output location among
    /// them.
    pub enforced: bool,
    /// Whether Athena keeps the results itself, so no location is needed.
    pub managed_results: bool,
    /// `Athena engine version 3`, as the workgroup names the engine its queries run on.
    pub engine: Option<String>,
    /// The most a query may scan before Athena cancels it, when the workgroup sets a limit.
    pub cutoff: Option<ByteCount>,
}

/// Why the data source's queries would have nowhere to put their results.
#[derive(Debug, thiserror::Error)]
#[error(
    "the workgroup names no location for query results; set an output location \
     (s3://bucket/prefix/) on the data source, or one in the workgroup's settings if it \
     enforces them"
)]
pub(crate) struct NoOutputLocation;

impl Workgroup {
    /// Reads the workgroup named `name`.
    ///
    /// # Errors
    ///
    /// What Athena or the SDK said, when the workgroup can't be read.
    pub(crate) async fn read(client: &Client, name: &str) -> Result<Self, Refused> {
        let output = client.get_work_group().work_group(name).send().await?;
        let configuration = output
            .work_group()
            .and_then(|workgroup| workgroup.configuration());
        Ok(configuration.map(Self::from).unwrap_or_default())
    }

    /// The output location to send with each query: none when the workgroup's own is used,
    /// because it enforces its settings or the data source names none.
    ///
    /// # Errors
    ///
    /// [`NoOutputLocation`] when the workgroup doesn't keep results and names no location,
    /// and the data source names none or the workgroup overrides it.
    pub(crate) fn output_location_for(
        &self,
        configured: &str,
    ) -> Result<Option<String>, NoOutputLocation> {
        let configured = configured.trim();
        if !configured.is_empty() && !self.enforced {
            return Ok(Some(configured.to_owned()));
        }
        if self.output_location.is_some() || self.managed_results {
            return Ok(None);
        }
        Err(NoOutputLocation)
    }

    /// The server's version as Test connection shows it: the engine, the region, and the
    /// workgroup's limit a query when it has one.
    pub(crate) fn describe_version(&self, region: &str) -> String {
        let parts: Vec<String> = [
            self.engine.clone(),
            (!region.is_empty()).then(|| region.to_owned()),
            self.cutoff
                .map(|cutoff| format!("the workgroup stops queries past {cutoff}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        parts.join(", ")
    }

    /// The limit DbRover keeps a query to: the data source's, when the workgroup has none
    /// of its own; Athena enforces the workgroup's, exactly.
    pub(crate) fn limit_for(&self, configured: Option<ByteCount>) -> Option<ByteCount> {
        match self.cutoff {
            Some(_) => None,
            None => configured,
        }
    }
}

impl From<&WorkGroupConfiguration> for Workgroup {
    fn from(configuration: &WorkGroupConfiguration) -> Self {
        Self {
            output_location: configuration
                .result_configuration()
                .and_then(|results| results.output_location())
                .map(str::to_owned),
            enforced: configuration
                .enforce_work_group_configuration()
                .unwrap_or(false),
            managed_results: configuration
                .managed_query_results_configuration()
                .is_some_and(|managed| managed.enabled()),
            engine: configuration
                .engine_version()
                .and_then(|version| version.effective_engine_version())
                .map(str::to_owned),
            cutoff: configuration
                .bytes_scanned_cutoff_per_query()
                .and_then(|bytes| u64::try_from(bytes).ok())
                .map(ByteCount),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_a_location_only_where_the_workgroup_takes_one() {
        let named = Workgroup {
            output_location: Some("s3://wg/".to_owned()),
            ..Workgroup::default()
        };
        let enforced = Workgroup {
            enforced: true,
            ..named.clone()
        };
        let managed = Workgroup {
            managed_results: true,
            ..Workgroup::default()
        };
        let bare = Workgroup::default();
        let cases = [
            (&named, "s3://mine/", Ok(Some("s3://mine/"))),
            (&named, "", Ok(None)),
            (&enforced, "s3://mine/", Ok(None)),
            (&managed, "", Ok(None)),
            (&bare, "  s3://mine/ ", Ok(Some("s3://mine/"))),
            (&bare, "", Err(())),
            // A workgroup that enforces its settings without naming a location: Athena would
            // ignore the data source's.
            (
                &Workgroup {
                    enforced: true,
                    ..Workgroup::default()
                },
                "s3://mine/",
                Err(()),
            ),
        ];
        for (workgroup, configured, expected) in cases {
            let found = workgroup.output_location_for(configured);
            assert_eq!(
                found.as_ref().map(Option::as_deref).map_err(drop),
                expected,
                "{workgroup:?} {configured:?}"
            );
        }
    }
}
