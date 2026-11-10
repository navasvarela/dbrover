//! A run of a report in the settings folder's `reports/` kept in `history.sqlite`, as the app
//! keeps its own runs (`docs/reporting.md`, "Run history" in RP4 and RP6): the same row, built
//! from the same events by the app's [`RunRecord`], so the report's tab lists it beside the
//! others.

use std::num::NonZeroU32;
use std::path::Path;

use dbrover_core::describe_error;
use dbrover_report::{TimedEvent, TypedValues};
use dbrover_store::{AppPaths, HistoryFile, ReportName, RunOrigin};

use crate::reports::RunRecord;

/// The run so far, and where it will be kept.
pub struct RunHistory {
    report: ReportName,
    record: RunRecord,
    file: HistoryFile,
    kept: NonZeroU32,
    /// What started the run: the command, or an MCP client.
    origin: RunOrigin,
}

impl RunHistory {
    /// A run of `report` starting now with `values`, keeping the newest `kept` runs of it; `None`
    /// for a report file outside `reports/`, which the app can't list, and whose runs would
    /// otherwise mix with those of a report there of the same name.
    pub fn start(
        paths: &AppPaths,
        report: &Path,
        values: TypedValues,
        kept: NonZeroU32,
        origin: RunOrigin,
    ) -> Option<Self> {
        let report = listed_name(paths.report_files().dir(), report)?;
        Some(Self {
            report,
            record: RunRecord::new(values),
            file: HistoryFile::new(paths.history_file()),
            kept,
            origin,
        })
    }

    pub fn event(&mut self, event: &TimedEvent) {
        self.record.apply(event.clone());
    }

    /// Writes the run, once an event has ended it.
    ///
    /// # Errors
    ///
    /// Why the history can't be written, or that the run hasn't ended.
    pub fn keep(self) -> Result<(), String> {
        let run = self
            .record
            .to_report_run(&self.report, self.origin)
            .ok_or("the run hasn't ended")?;
        self.file
            .open()
            .and_then(|history| history.record_run(&run, self.kept))
            .map_err(|error| describe_error(&error))
    }
}

/// The name the app lists the report file `report` by, if it is in `reports`: its file name
/// without `.toml`.
pub(super) fn listed_name(reports: &Path, report: &Path) -> Option<ReportName> {
    let folder = report.parent()?.canonicalize().ok()?;
    if folder != reports.canonicalize().ok()? || report.extension()? != "toml" {
        return None;
    }
    ReportName::new(report.file_stem()?.to_str()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_only_a_report_in_the_reports_folder() {
        let settings = tempfile::tempdir().unwrap();
        let reports = settings.path().join("reports");
        let elsewhere = settings.path().join("elsewhere");
        std::fs::create_dir_all(&reports).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let cases = [
            (reports.join("sales.toml"), Some("sales")),
            // Through a path that names the folder another way.
            (reports.join("../reports/sales.toml"), Some("sales")),
            (elsewhere.join("sales.toml"), None),
            (reports.join("notes.txt"), None),
            (settings.path().join("missing/sales.toml"), None),
        ];
        for (report, expected) in cases {
            assert_eq!(
                listed_name(&reports, &report),
                expected.and_then(ReportName::new),
                "{}",
                report.display()
            );
        }
    }
}
