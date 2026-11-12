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
    /// The sources loaded and typed in the workspace, in the order they finished (RS5e).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceKept>,
}

/// A source an unpartitioned run loaded into its workspace and typed: its table is in the
/// workspace's file, which the work folder keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceKept {
    pub id: String,
    /// The rows and bytes it read, which its caps and `resumable_above` count.
    pub rows: u64,
    pub read: u64,
    /// Its table's columns as typed, which a lookup of a later round reads.
    pub columns: Vec<KeptColumn>,
}

/// A loaded table's column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptColumn {
    pub name: String,
    /// Its DuckDB type, as `DECIMAL(18,2)`.
    pub duckdb_type: String,
}

/// What a run is: what a run resuming it must be too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunIdentity {
    /// The report's file.
    pub report: PathBuf,
    /// The folder its files go in, where a partitioned run's finished ranges' files are.
    pub folder: PathBuf,
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
    /// The identity of a run of `plan`, read from `report` at `version`, with `values`, its
    /// files going in `folder`.
    pub fn of(
        plan: &Plan,
        report: &Path,
        folder: &Path,
        version: String,
        values: &TypedValues,
    ) -> Self {
        // Canonical, so the app and the command line, which may name them another way, name
        // the same report and folder alike.
        let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
        Self {
            report: canonical(report),
            folder: canonical(folder),
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

impl RunIdentity {
    /// What differs between this run and `kept`, the run it would resume, a sentence each:
    /// nothing when it may resume it.
    pub fn changes_since(&self, kept: &Self) -> Vec<String> {
        let mut changes = Vec::new();
        if self.report != kept.report {
            changes.push(format!(
                "it ran {}, not {}",
                kept.report.display(),
                self.report.display()
            ));
        }
        if self.folder != kept.folder {
            changes.push(format!(
                "it wrote its files in {}, not {}",
                kept.folder.display(),
                self.folder.display()
            ));
        }
        if self.version != kept.version {
            changes.push(format!(
                "the report's file changed since it ran: its version was {}, and is {} now",
                kept.version, self.version
            ));
        }
        if self.dbrover != kept.dbrover {
            changes.push(format!(
                "it ran on DbRover {}, and this is {}",
                kept.dbrover, self.dbrover
            ));
        }
        changes.extend(value_changes(&kept.values, &self.values));
        if self.data_sources != kept.data_sources {
            changes.extend(data_source_changes(&kept.data_sources, &self.data_sources));
        }
        changes
    }
}

/// How the parameters' values `now` differ from those a run had `then`.
fn value_changes(then: &TypedValues, now: &TypedValues) -> Vec<String> {
    let names: std::collections::BTreeSet<&String> = then.keys().chain(now.keys()).collect();
    names
        .into_iter()
        .filter_map(|name| match (then.get(name), now.get(name)) {
            (Some(was), Some(is)) if was != is => {
                Some(format!("{name} was {was} when it ran, and is {is} now"))
            }
            (Some(was), None) => Some(format!("{name} was {was} when it ran, and isn't given now")),
            (None, Some(is)) => Some(format!("{name} wasn't given when it ran, and is {is} now")),
            _ => None,
        })
        .collect()
}

/// How the data sources `now` differ from those a run read `then`.
fn data_source_changes(then: &[DataSourceIdentity], now: &[DataSourceIdentity]) -> Vec<String> {
    let mut changes: Vec<String> = now
        .iter()
        .filter_map(|source| {
            let was = then.iter().find(|was| was.source == source.source);
            match was {
                None => Some(format!("the source {} is new", source.source)),
                Some(was) if was.target != source.target => Some(format!(
                    "the source {} read {} when it ran, and reads {} now",
                    source.source, was.target, source.target
                )),
                Some(was) if was.id != source.id => Some(format!(
                    "the source {} read the data source {} when it ran, and reads another now, \
                     {}",
                    source.source, was.id, source.id
                )),
                Some(_) => None,
            }
        })
        .collect();
    changes.extend(
        then.iter()
            .filter(|was| !now.iter().any(|source| source.source == was.source))
            .map(|was| format!("the source {} is gone", was.source)),
    );
    if changes.is_empty() {
        changes.push("its sources are in another order".to_owned());
    }
    changes
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
            sources: Vec::new(),
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
        let ranges: u64 = self.ranges.iter().map(|range| range.read).sum();
        let sources: u64 = self.sources.iter().map(|source| source.read).sum();
        ranges + sources
    }

    /// Whether anything finished, so a run resuming it has something to skip.
    pub fn has_finished_units(&self) -> bool {
        !self.ranges.is_empty() || !self.sources.is_empty()
    }

    /// When the folder was kept.
    pub fn kept_at(&self) -> Option<Timestamp> {
        self.kept_at.as_deref()?.parse().ok()
    }

    /// Marks the folder kept, now.
    pub fn mark_kept(&mut self) {
        self.kept_at = Some(Timestamp::now().to_string());
    }

    /// The manifest of the kept run in `work`, ready for `identity`'s run to carry on from:
    /// no longer marked kept, since the run going on now writes in its folder.
    ///
    /// # Errors
    ///
    /// Every reason it can't, a sentence each: the folder holds no kept run, or what differs.
    pub fn resume(work: &Path, identity: &RunIdentity) -> Result<Self, Vec<String>> {
        let folder = work.display();
        let mut manifest = match Self::read(work) {
            Ok(Some(manifest)) if manifest.kept_at.is_some() => manifest,
            Ok(_) => return Err(vec![format!("{folder} holds no run to resume")]),
            Err(error) => return Err(vec![format!("{folder}: {error}")]),
        };
        let changes = identity.changes_since(&manifest.run);
        if !changes.is_empty() {
            return Err(changes);
        }
        manifest.kept_at = None;
        manifest
            .write(work)
            .map_err(|error| vec![format!("{folder}: {error}")])?;
        Ok(manifest)
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
    /// The sources loaded in its workspace, which a run resuming it doesn't read again.
    #[serde(default)]
    pub sources: u64,
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
            sources: manifest.sources.len() as u64,
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
            folder: PathBuf::from("/out"),
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
