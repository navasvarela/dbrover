//! Where a run's handed-over files go, and what tells a leftover from a run in flight
//! (`docs/reports-at-scale.md`, RS7f).
//!
//! A run owns one folder under its report's own prefix:
//!
//! ```text
//! <prefix><report>/run-<nanoseconds>-<process>/<source>/
//! ```
//!
//! The folder's name says when the run started, so the cleanup needs nothing from the
//! bucket's listing: a run removes the folders of its report that started before it, which
//! are what a killed run left, and never one beside it. A folder whose name it can't read is
//! somebody else's and is left alone.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use dbrover_driver_duckdb::BucketSignIn;

use crate::{Logic, Plan, PlannedHandoff, PlannedSource, SourceOpener};

/// What every run folder's name starts with.
const RUN: &str = "run-";

/// The folders of one report's handoffs in a bucket: the report's own prefix, and this run's
/// folder under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandoffFolders {
    report: String,
    run: String,
}

impl HandoffFolders {
    /// The folders of a run of `report` starting now.
    ///
    /// The run's folder is named to the clock's own granularity, which is a nanosecond on
    /// macOS and Linux and a 100-nanosecond FILETIME tick on Windows; two runs that start
    /// inside one tick are told apart by the process, which is why the name carries it.
    pub(crate) fn starting(report: &str) -> Self {
        Self::started(report, SystemTime::now(), std::process::id())
    }

    fn started(report: &str, at: SystemTime, process: u32) -> Self {
        let nanos = at
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        Self {
            report: slug(report),
            // The process is there so two runs of one report that start in the same
            // nanosecond, in two windows or a window and the command line, own two folders.
            run: format!("{RUN}{nanos}-{process}"),
        }
    }

    /// Where every run of this report writes, under the handoff's own `prefix`.
    pub(crate) fn report_prefix(&self, prefix: &str) -> String {
        format!("{prefix}{}/", self.report)
    }

    /// Where this run writes.
    pub(crate) fn run_prefix(&self, prefix: &str) -> String {
        format!("{}{}/", self.report_prefix(prefix), self.run)
    }

    /// Where one statement of the source `id` writes: its own folder, which is empty, since
    /// `UNLOAD` refuses a folder that isn't. A range of a partitioned run gets one of its own,
    /// because a source without a `partition` is read again for each range.
    pub(crate) fn statement_prefix(&self, prefix: &str, id: &str, part: Option<u64>) -> String {
        let source = slug(id);
        match part {
            Some(part) => format!("{}{source}/part-{part}/", self.run_prefix(prefix)),
            None => format!("{}{source}/", self.run_prefix(prefix)),
        }
    }

    /// The folders of this report that an earlier run left, out of the `folders` a listing of
    /// [`HandoffFolders::report_prefix`] gave: those whose name says they started before this
    /// run did.
    ///
    /// A folder this run owns is never one of them, and neither is a folder whose name isn't a
    /// run's — a user's own folder under the same prefix, or one a later DbRover writes.
    pub(crate) fn left_behind<'f>(&self, folders: &'f [String]) -> Vec<&'f str> {
        let Some(started) = started_at(&self.run) else {
            return Vec::new();
        };
        folders
            .iter()
            .filter(|folder| {
                let name = folder.trim_end_matches('/').rsplit('/').next();
                name.and_then(started_at)
                    .is_some_and(|other| other < started)
            })
            .map(String::as_str)
            .collect()
    }
}

/// When the run whose folder is named `name` started, in nanoseconds; `None` for a name that
/// isn't a run folder's.
fn started_at(name: &str) -> Option<u128> {
    let rest = name.strip_prefix(RUN)?;
    let nanos = rest.split_once('-').map_or(rest, |(nanos, _)| nanos);
    nanos.parse().ok()
}

/// `name` as one part of a key: lowercase letters, digits and `-`, so a report's name reads in
/// a bucket's listing whatever it holds. Two names that slug alike share a prefix, which only
/// means each run cleans up after the other's leftovers.
fn slug(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for character in name.chars() {
        match character {
            'a'..='z' | '0'..='9' => slug.push(character),
            'A'..='Z' => slug.extend(character.to_lowercase()),
            _ if slug.ends_with('-') => {}
            _ => slug.push('-'),
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        return "report".to_owned();
    }
    slug.chars().take(64).collect()
}

/// Every handoff of `plan` that writes into a folder of its own, one per bucket and prefix:
/// what a cleanup has to reach. A handoff with no prefix reads the engine's own result file,
/// which lies where the engine puts every result, not in a folder the run owns.
fn written_in(plan: &Plan) -> Vec<&PlannedHandoff> {
    let Logic::Combine(combined) = &plan.logic else {
        // A report with no combine step has no DuckDB to read files with, which is refused
        // when the report is read.
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    combined
        .sources
        .iter()
        .filter_map(|source| match source {
            PlannedSource::Query(source) => source.handoff.as_ref(),
            PlannedSource::Attached(_) | PlannedSource::Lake(_) => None,
        })
        .filter(|handoff| handoff.prefix.is_some())
        .filter(|handoff| seen.insert((handoff.data_source.id, handoff.prefix.clone())))
        .map(Box::as_ref)
        .collect()
}

/// Removes what earlier runs of this report left in its handoff prefixes: a run killed before
/// its own delete, which nothing else would clean up (RS7f).
///
/// Best effort, and it runs before the report does: a bucket that can't be reached is the
/// run's own problem a moment later, when the handoff's statement writes in it, and that
/// failure says more than this one would. What is left behind stays for the next run, or for
/// the lifecycle rule the manual asks for.
pub(crate) async fn remove_left_behind<O: SourceOpener>(
    plan: &Plan,
    opener: &O,
    folders: &HandoffFolders,
) {
    for handoff in written_in(plan) {
        let Some(prefix) = &handoff.prefix else {
            continue;
        };
        let Some(mut sign_in) = sign_in(handoff, opener).await else {
            continue;
        };
        let report = folders.report_prefix(prefix);
        let listed = match sign_in.folders_under(&report).await {
            Ok(listed) => listed,
            Err(error) => {
                tracing::debug!("could not list {report} for leftover handoff files: {error}");
                continue;
            }
        };
        for left in folders.left_behind(&listed) {
            match sign_in.remove_under(left).await {
                Ok(0) => {}
                Ok(files) => tracing::info!("removed {files} files an earlier run left in {left}"),
                Err(error) => tracing::warn!("could not remove {left}: {error}"),
            }
        }
    }
}

/// Removes this run's own handoff folders, however the run ended (RS7f): the files each
/// statement's load already deleted are gone, and this takes what a statement that failed, or
/// a combine step that was stopped, left behind.
pub(crate) async fn remove_this_runs<O: SourceOpener>(
    plan: &Plan,
    opener: &O,
    folders: &HandoffFolders,
) {
    for handoff in written_in(plan) {
        let Some(prefix) = &handoff.prefix else {
            continue;
        };
        let Some(mut sign_in) = sign_in(handoff, opener).await else {
            continue;
        };
        let run = folders.run_prefix(prefix);
        match sign_in.remove_under(&run).await {
            Ok(0) => {}
            Ok(files) => tracing::info!("removed {files} handoff files from {run}"),
            // The files stay for the next run of this report, and for the prefix's lifecycle
            // rule; the run itself is done either way.
            Err(error) => tracing::warn!("could not remove {run}: {error}"),
        }
    }
}

/// The bucket's sign-in, which reads and deletes the files: the handoff's own data source's,
/// never the engine's. `None` when its password can't be found, which the run's own statement
/// reports properly a moment later.
async fn sign_in<O: SourceOpener>(handoff: &PlannedHandoff, opener: &O) -> Option<BucketSignIn> {
    let password = if handoff.storage.uses_password() {
        match opener.password(&handoff.data_source).await {
            Ok(password) => password,
            Err(error) => {
                tracing::debug!(
                    "could not read the password of {:?} for its handoff: {}",
                    handoff.data_source.name,
                    error.message
                );
                return None;
            }
        }
    } else {
        None
    };
    Some(BucketSignIn::new(handoff.storage.clone(), password))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A run of `report` started `nanos` after the epoch.
    ///
    /// `nanos` has to be a multiple of 100: Windows counts a `SystemTime` in FILETIME's
    /// 100-nanosecond ticks, so a time with a finer tail comes back truncated there and the
    /// folder's name loses its last digits — which is what made this test the stack's one
    /// Windows failure. The assertion keeps a later fixture from finding that out again.
    fn folders(report: &str, nanos: u64, process: u32) -> HandoffFolders {
        assert_eq!(
            nanos % 100,
            0,
            "a fixture's time must be one every platform's clock holds"
        );
        HandoffFolders::started(report, UNIX_EPOCH + Duration::from_nanos(nanos), process)
    }

    #[test]
    fn a_statements_folder_names_the_report_the_run_the_source_and_the_range() {
        let run = folders("Monthly balances", 1_700_000_000_000_000_100, 4242);

        assert_eq!(
            run.report_prefix("dbrover-tmp/"),
            "dbrover-tmp/monthly-balances/"
        );
        assert_eq!(
            run.run_prefix("dbrover-tmp/"),
            "dbrover-tmp/monthly-balances/run-1700000000000000100-4242/"
        );
        assert_eq!(
            run.statement_prefix("dbrover-tmp/", "ledger", None),
            "dbrover-tmp/monthly-balances/run-1700000000000000100-4242/ledger/"
        );
        assert_eq!(
            run.statement_prefix("dbrover-tmp/", "ledger", Some(3)),
            "dbrover-tmp/monthly-balances/run-1700000000000000100-4242/ledger/part-3/"
        );
    }

    #[test]
    fn a_name_that_holds_nothing_a_key_takes_still_names_a_folder() {
        let cases = [
            ("Q4 2026 — Profit & loss", "q4-2026-profit-loss"),
            ("../../etc", "etc"),
            ("", "report"),
            ("////", "report"),
        ];
        for (name, expected) in cases {
            assert_eq!(
                folders(name, 100, 1).report_prefix(""),
                format!("{expected}/")
            );
        }
    }

    /// The cleanup's whole rule: older runs of this report go, this run and the ones beside it
    /// stay, and a folder that isn't a run's is never touched.
    #[test]
    fn only_the_folders_of_runs_that_started_earlier_are_left_behind() {
        let run = folders("shop", 2_000, 7);
        let listed = [
            "dbrover-tmp/shop/run-1000-3/".to_owned(),
            "dbrover-tmp/shop/run-1999-7/".to_owned(),
            "dbrover-tmp/shop/run-2000-7/".to_owned(),
            "dbrover-tmp/shop/run-2001-9/".to_owned(),
            "dbrover-tmp/shop/run-3000-1/".to_owned(),
            "dbrover-tmp/shop/my-own-files/".to_owned(),
            "dbrover-tmp/shop/run-not-a-number/".to_owned(),
        ];

        assert_eq!(
            run.left_behind(&listed),
            [
                "dbrover-tmp/shop/run-1000-3/",
                "dbrover-tmp/shop/run-1999-7/"
            ]
        );
    }

    #[test]
    fn two_runs_of_one_report_in_the_same_nanosecond_own_two_folders() {
        let first = folders("shop", 500, 11);
        let second = folders("shop", 500, 12);

        assert_ne!(first.run_prefix("tmp/"), second.run_prefix("tmp/"));
        // Neither is the other's leftover: they started at the same time.
        assert!(first.left_behind(&[second.run_prefix("tmp/")]).is_empty());
        assert!(second.left_behind(&[first.run_prefix("tmp/")]).is_empty());
    }
}
