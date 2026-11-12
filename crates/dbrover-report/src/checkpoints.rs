//! The run's manifest as the run goes (`docs/reports-at-scale.md`, RS5c to RS5e): the units an
//! earlier run finished, which this one takes rather than reading again, and each unit this
//! run finishes, recorded as it ends. Without a manifest in the work folder, as in tests that
//! call [`crate::run`] directly, nothing is recorded or taken.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use crate::{Manifest, PartRange, RangeKept, SourceKept, WorkKept, keep_work_folder};

/// The run's manifest, shared by the steps that finish units, one at a time.
pub(crate) struct Checkpoints {
    work: PathBuf,
    manifest: RefCell<Option<Manifest>>,
}

impl Checkpoints {
    /// The manifest in `work`, which the worker wrote before the run started.
    pub(crate) fn read(work: &Path) -> Self {
        let manifest = Manifest::read(work).unwrap_or_else(|error| {
            tracing::warn!("the run's manifest can't be read, so it won't resume: {error}");
            None
        });
        Self {
            work: work.to_owned(),
            manifest: RefCell::new(manifest),
        }
    }

    /// What the manifest says of `range`, when an earlier run finished it with the same
    /// bounds: a range this run doesn't read again.
    pub(crate) fn finished(&self, range: &PartRange) -> Option<RangeKept> {
        self.manifest
            .borrow()
            .as_ref()?
            .ranges
            .iter()
            .find(|kept| kept.number == range.number)
            .filter(|kept| kept.from == range.from && kept.to == range.to)
            .cloned()
    }

    /// The source `id`, when an earlier run loaded it into the workspace it kept.
    pub(crate) fn source(&self, id: &str) -> Option<SourceKept> {
        self.manifest
            .borrow()
            .as_ref()?
            .sources
            .iter()
            .find(|kept| kept.id == id)
            .cloned()
    }

    /// Whether an earlier run loaded any source into the workspace it kept.
    pub(crate) fn has_sources(&self) -> bool {
        self.manifest
            .borrow()
            .as_ref()
            .is_some_and(|manifest| !manifest.sources.is_empty())
    }

    /// Records a finished range.
    pub(crate) fn range_done(&self, range: RangeKept) {
        self.record(|manifest| manifest.ranges.push(range));
    }

    /// Records a source loaded into the workspace and typed.
    pub(crate) fn source_done(&self, source: SourceKept) {
        self.record(|manifest| manifest.sources.push(source));
    }

    /// Changes the manifest with `change` and writes it. One that can't be written fails
    /// nothing: the run goes on, and a run resuming it reads the units recorded before again.
    fn record(&self, change: impl FnOnce(&mut Manifest)) {
        let mut manifest = self.manifest.borrow_mut();
        let Some(manifest) = manifest.as_mut() else {
            return;
        };
        change(manifest);
        if let Err(error) = manifest.write(&self.work) {
            tracing::warn!("could not record a finished unit in the run's manifest: {error}");
        }
    }

    /// Keeps the work folder for a run that resumes, when the finished units read more than
    /// `above`.
    pub(crate) fn keep(&self, above: u64) -> Option<WorkKept> {
        self.manifest.borrow().as_ref()?;
        keep_work_folder(&self.work, above, false).unwrap_or_else(|error| {
            tracing::warn!("could not keep the run's work folder: {error}");
            None
        })
    }
}
