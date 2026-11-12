//! A run's manifest, `manifest.json` in its work folder (`docs/reports-at-scale.md`, RS5c):
//! what the run is, and each unit it finished, written as each one ends. A run that fails, is
//! stopped or is killed after its finished units read more than `resumable_above` keeps its
//! work folder, marked kept in the manifest, so a later run can carry on from it.
//!
//! The worker writes the manifest before the run starts ([`Manifest::start`]); [`crate::run`]
//! records each unit in a manifest it finds there, and runs as before without one.

use std::io;
use std::path::{Path, PathBuf};

use dbrover_core::DataSource;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{Logic, Plan, PlannedSource, TypedValues, WrittenFile};

/// The manifest's file, in the run's work folder.
pub const MANIFEST_FILE: &str = "manifest.json";

/// The manifest's format, for a later DbRover that changes it: one it doesn't know isn't
/// resumed.
const FORMAT: u32 = 1;

/// What a run is, and what it has finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub run: RunIdentity,
    /// When the run started, as RFC 3339.
    pub started_at: String,
    /// When the work folder was kept, as RFC 3339; `None` while the run goes on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_at: Option<String>,
    /// A partitioned run's finished ranges, in their order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<RangeKept>,
}

/// What a run is: what a run resuming it must be too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunIdentity {
    /// The report's file.
    pub report: PathBuf,
    /// The Git blob id of the file's text that ran (VC1e).
    pub version: String,
    /// The parameters' values, as typed.
    pub values: TypedValues,
    /// The DbRover that ran it, as `0.33.0`.
    pub dbrover: String,
    /// Each data source a source reads, in the file's order.
    pub data_sources: Vec<DataSourceIdentity>,
}

/// Where a data source goes, never its secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSourceIdentity {
    /// The source's id in the report.
    pub source: String,
    /// The data source's id in `data_sources.toml`.
    pub id: String,
    /// Its connection as logs describe it: the engine, and its host, port, database and user
    /// where it has them.
    pub target: String,
}

/// A finished range of a partitioned run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeKept {
    pub number: u64,
    pub from: String,
    pub to: String,
    /// The bytes its sources read, which `resumable_above` counts.
    pub read: u64,
    /// The rows and bytes it gave the outputs.
    pub rows: u64,
    pub bytes: u64,
    /// The range's own files, put in place when it ended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<WrittenFile>,
}

impl RunIdentity {
    /// The identity of a run of `plan`, read from `report` at `version`, with `values`.
    pub fn of(plan: &Plan, report: &Path, version: String, values: &TypedValues) -> Self {
        Self {
            report: report.to_owned(),
            version,
            values: values.clone(),
            dbrover: env!("CARGO_PKG_VERSION").to_owned(),
            data_sources: data_sources(plan),
        }
    }
}

/// Each source's data source, in the file's order.
fn data_sources(plan: &Plan) -> Vec<DataSourceIdentity> {
    let identity = |source: &str, data_source: &DataSource| DataSourceIdentity {
        source: source.to_owned(),
        id: data_source.id.to_string(),
        target: format!("{:?} {}", data_source.config.driver(), data_source.config),
    };
    match &plan.logic {
        Logic::Direct(source) => vec![identity(&source.id, &source.data_source)],
        Logic::Combine(combined) => combined
            .sources
            .iter()
            .map(|source| match source {
                PlannedSource::Query(query) => identity(&query.id, &query.data_source),
                PlannedSource::Attached(attached) => identity(&attached.id, &attached.data_source),
            })
            .collect(),
    }
}

impl Manifest {
    /// A manifest for a run starting now, with nothing finished.
    pub fn start(run: RunIdentity) -> Self {
        Self {
            format: FORMAT,
            run,
            started_at: Timestamp::now().to_string(),
            kept_at: None,
            ranges: Vec::new(),
        }
    }

    /// The manifest in the work folder `work`; `None` when it has none.
    ///
    /// # Errors
    ///
    /// When it can't be read, isn't a manifest, or is of a format this DbRover doesn't know.
    pub fn read(work: &Path) -> io::Result<Option<Self>> {
        let text = match std::fs::read_to_string(work.join(MANIFEST_FILE)) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let manifest: Self = serde_json::from_str(&text).map_err(io::Error::other)?;
        if manifest.format != FORMAT {
            return Err(io::Error::other(format!(
                "the manifest is of format {}, which this DbRover doesn't read",
                manifest.format
            )));
        }
        Ok(Some(manifest))
    }

    /// Writes the manifest in `work`, whole or not at all: a new file renamed over the old,
    /// so a worker killed while writing it leaves the last one.
    ///
    /// # Errors
    ///
    /// When it can't be written.
    pub fn write(&self, work: &Path) -> io::Result<()> {
        std::fs::create_dir_all(work)?;
        let text = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let partial = work.join(format!("{MANIFEST_FILE}.partial"));
        {
            let mut file = std::fs::File::create(&partial)?;
            io::Write::write_all(&mut file, &text)?;
            file.sync_all()?;
        }
        std::fs::rename(&partial, work.join(MANIFEST_FILE))
    }

    /// The bytes the finished units read: what `resumable_above` is held against.
    pub fn finished_bytes(&self) -> u64 {
        self.ranges.iter().map(|range| range.read).sum()
    }

    /// Whether anything finished, so a run resuming it has something to skip.
    pub fn has_finished_units(&self) -> bool {
        !self.ranges.is_empty()
    }

    /// When the folder was kept.
    pub fn kept_at(&self) -> Option<Timestamp> {
        self.kept_at.as_deref()?.parse().ok()
    }

    /// Marks the folder kept, now.
    pub fn mark_kept(&mut self) {
        self.kept_at = Some(Timestamp::now().to_string());
    }
}

/// Keeps the work folder `work` when its manifest's finished units read more than `above`, or
/// whatever they read when `always`, marking it kept; the folder's name when it is kept.
///
/// # Errors
///
/// When the manifest can't be read or written: the caller removes the folder then.
pub fn keep_work_folder(work: &Path, above: u64, always: bool) -> io::Result<Option<WorkKept>> {
    let Some(mut manifest) = Manifest::read(work)? else {
        return Ok(None);
    };
    if !manifest.has_finished_units() || !(always || manifest.finished_bytes() > above) {
        return Ok(None);
    }
    manifest.mark_kept();
    manifest.write(work)?;
    Ok(Some(WorkKept::of(work, &manifest)))
}

/// A run's work folder kept so the run can resume, as [`crate::RunEvent::WorkKept`] says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkKept {
    /// The folder's name, in the folder runs' work folders go in.
    pub folder: String,
    /// The finished ranges a run resuming it skips.
    pub ranges: u64,
    /// The bytes they read.
    pub read: u64,
}

impl WorkKept {
    pub(crate) fn of(work: &Path, manifest: &Manifest) -> Self {
        Self {
            folder: work
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            ranges: manifest.ranges.len() as u64,
            read: manifest.finished_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest::start(RunIdentity {
            report: PathBuf::from("/reports/events.toml"),
            version: "a1b2".to_owned(),
            values: TypedValues::from([("since".to_owned(), "2025-01-01".to_owned())]),
            dbrover: "0.33.0".to_owned(),
            data_sources: vec![DataSourceIdentity {
                source: "events".to_owned(),
                id: "generated".to_owned(),
                target: "DuckDb :memory:".to_owned(),
            }],
        })
    }

    fn range(number: u64, read: u64) -> RangeKept {
        RangeKept {
            number,
            from: format!("2025-0{number}-01"),
            to: format!("2025-0{}-01", number + 1),
            read,
            rows: 10,
            bytes: 100,
            files: Vec::new(),
        }
    }

    #[test]
    fn writes_and_reads_a_manifest_whole() {
        let work = tempfile::tempdir().unwrap();
        assert_eq!(Manifest::read(work.path()).unwrap(), None);
        let mut written = manifest();
        written.ranges.push(range(1, 500));
        written.write(work.path()).unwrap();
        written.ranges.push(range(2, 700));
        written.write(work.path()).unwrap();
        let read = Manifest::read(work.path()).unwrap().unwrap();
        assert_eq!(read, written);
        assert_eq!(read.finished_bytes(), 1_200);
        assert!(!work.path().join("manifest.json.partial").exists());
    }

    #[test]
    fn refuses_a_format_it_does_not_know() {
        let work = tempfile::tempdir().unwrap();
        let mut later = manifest();
        later.format = FORMAT + 1;
        later.write(work.path()).unwrap();
        let error = Manifest::read(work.path()).unwrap_err();
        assert!(error.to_string().contains("format 2"), "{error}");
    }

    #[test]
    fn keeps_a_folder_past_the_threshold_or_when_asked() {
        let cases = [
            // (ranges' bytes read, threshold, always, kept)
            (&[][..], 0, true, false),
            (&[400, 400][..], 1_000, false, false),
            (&[600, 600][..], 1_000, false, true),
            (&[400][..], 1_000, true, true),
        ];
        for (reads, above, always, kept) in cases {
            let work = tempfile::tempdir().unwrap();
            let mut written = manifest();
            written.ranges = reads
                .iter()
                .zip(1..)
                .map(|(read, number)| range(number, *read))
                .collect();
            written.write(work.path()).unwrap();
            let outcome = keep_work_folder(work.path(), above, always).unwrap();
            assert_eq!(outcome.is_some(), kept, "{reads:?} above {above}");
            let read = Manifest::read(work.path()).unwrap().unwrap();
            assert_eq!(read.kept_at().is_some(), kept);
            if let Some(outcome) = outcome {
                assert_eq!(outcome.ranges, reads.len() as u64);
                assert_eq!(outcome.read, reads.iter().sum::<u64>());
            }
        }
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(keep_work_folder(empty.path(), 0, true).unwrap(), None);
    }
}
