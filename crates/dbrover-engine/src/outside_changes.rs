//! What other programs changed in the files the engine keeps in memory, found when a file is
//! read again before a save or when the window comes back to the front (VC1b in
//! `docs/version-control.md`).

use std::collections::{HashMap, hash_map::Entry};
use std::error::Error;

use dbrover_store::{FileVersion, Snapshot};

use crate::Document;
use crate::config_files::blocking;

/// When a change made by another program was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Noticed {
    /// Before writing the file, which wasn't written.
    Saving,
    /// When the window came back to the front.
    Looking,
}

/// The entries of a file of entries with ids that another program added, changed or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntryChanges<T, Id> {
    /// Added or changed, as the file holds them now.
    pub saved: Vec<T>,
    pub removed: Vec<Id>,
}

impl<T: Clone, Id: PartialEq> EntryChanges<T, Id> {
    pub(crate) fn is_empty(&self) -> bool {
        self.saved.is_empty() && self.removed.is_empty()
    }

    /// The same changes as of `now`, the entries after a save applied its own change on top:
    /// the events for the outside changes are sent after the save's own, and must not undo
    /// it.
    pub(crate) fn as_of(&mut self, now: &[T], id: impl Fn(&T) -> Id) {
        self.saved = self
            .saved
            .iter()
            .filter_map(|saved| now.iter().find(|entry| id(entry) == id(saved)).cloned())
            .collect();
        self.removed
            .retain(|removed| !now.iter().any(|entry| id(entry) == *removed));
    }
}

/// How `after` differs from `before`, entry by entry, matched by `id`.
pub(crate) fn entry_changes<T, Id>(
    before: &[T],
    after: &[T],
    id: impl Fn(&T) -> Id,
) -> EntryChanges<T, Id>
where
    T: Clone + PartialEq,
    Id: PartialEq,
{
    let saved = after
        .iter()
        .filter(|entry| !before.iter().any(|old| old == *entry))
        .cloned()
        .collect();
    let removed = before
        .iter()
        .map(&id)
        .filter(|old| !after.iter().any(|entry| id(entry) == *old))
        .collect();
    EntryChanges { saved, removed }
}

/// What the engine knows of each file the app shows as one document: the version last read or
/// written, or one written by another program that the app has been told of and hasn't
/// answered yet.
#[derive(Debug, Default)]
pub(crate) struct DocumentVersions(HashMap<Document, Known>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Known {
    Read(FileVersion),
    /// Changed on disk; saves wait until the app says it has seen this version, with
    /// **Reload** or **Keep mine**.
    ChangedOnDisk(FileVersion),
}

impl DocumentVersions {
    /// `document` was read or written at `version`.
    pub(crate) fn read(&mut self, document: Document, version: FileVersion) {
        self.0.insert(document, Known::Read(version));
    }

    pub(crate) fn forget(&mut self, document: &Document) {
        self.0.remove(document);
    }

    /// The version a save of `document` may write over, or `None` while the app hasn't
    /// answered a change on disk. A document never read has no file yet.
    pub(crate) fn expected(&self, document: &Document) -> Option<FileVersion> {
        match self.0.get(document) {
            Some(Known::Read(version)) => Some(*version),
            Some(Known::ChangedOnDisk(_)) => None,
            None => Some(FileVersion::Missing),
        }
    }

    /// Notes that `document` is at `on_disk`, written by another program. `false` if the app
    /// was already told of that version.
    pub(crate) fn changed_on_disk(&mut self, document: Document, on_disk: FileVersion) -> bool {
        match self.0.entry(document) {
            Entry::Occupied(mut known) => {
                if *known.get() == Known::ChangedOnDisk(on_disk) {
                    return false;
                }
                known.insert(Known::ChangedOnDisk(on_disk));
            }
            Entry::Vacant(vacant) => {
                vacant.insert(Known::ChangedOnDisk(on_disk));
            }
        }
        true
    }

    /// The app has taken in the version on disk it was told of, by showing it or by keeping
    /// its own over it: the next save may write over that version.
    pub(crate) fn seen(&mut self, document: &Document) {
        if let Some(known) = self.0.get_mut(document)
            && let Known::ChangedOnDisk(version) = *known
        {
            *known = Known::Read(version);
        }
    }

    /// The version `document` was last seen at: read, written, or found changed on disk.
    pub(crate) fn last_seen(&self, document: &Document) -> FileVersion {
        match self.0.get(document) {
            Some(Known::Read(version) | Known::ChangedOnDisk(version)) => *version,
            None => FileVersion::Missing,
        }
    }

    /// Every document the engine has read or written.
    pub(crate) fn documents(&self) -> Vec<Document> {
        self.0.keys().cloned().collect()
    }
}

/// How a save of a document went.
#[derive(Debug)]
pub(crate) enum Saved<T> {
    Written,
    /// Not written: another program changed the file, which now holds `T`.
    ChangedOnDisk(T),
    /// Not written: the app hasn't answered an earlier change on disk yet. Saves sent before
    /// it heard of that change end here.
    Waiting,
}

/// Saves `document` with `write`, unless another program changed its file since the engine
/// last read or wrote it, as `read` finds it just before. A file deleted since is no change:
/// there is nothing on disk to lose.
///
/// # Errors
///
/// What reading or writing the file failed with, described for the UI. A file that can't be
/// read, such as one left with a merge's conflict markers, is never written over.
pub(crate) async fn save_document<T, E>(
    documents: &mut DocumentVersions,
    document: Document,
    read: impl FnOnce() -> Result<Snapshot<T>, E> + Send + 'static,
    write: impl FnOnce(&Snapshot<T>) -> Result<FileVersion, E> + Send + 'static,
) -> Result<Saved<T>, String>
where
    T: Send + 'static,
    E: Error + Send + 'static,
{
    let Some(expected) = documents.expected(&document) else {
        tracing::debug!(
            ?document,
            "not saved: waiting for the app to see a change on disk"
        );
        return Ok(Saved::Waiting);
    };
    let checked = blocking(move || {
        let snapshot = read()?;
        let changed = snapshot.version != expected && snapshot.version != FileVersion::Missing;
        if changed {
            return Ok(Err(snapshot));
        }
        write(&snapshot).map(Ok)
    })
    .await?;
    match checked {
        Ok(version) => {
            documents.read(document, version);
            Ok(Saved::Written)
        }
        Err(snapshot) => {
            tracing::info!(?document, "changed on disk; not saved");
            documents.changed_on_disk(document, snapshot.version);
            Ok(Saved::ChangedOnDisk(snapshot.value))
        }
    }
}

/// What `document`'s file holds, if another program changed it since the app last saw it and
/// the app hasn't been told yet. A file deleted since is nothing to tell, as its next save
/// writes it again.
///
/// # Errors
///
/// Why the file can't be read, described for the UI. It is left for the next save to refuse.
pub(crate) async fn look_at<T, E>(
    documents: &mut DocumentVersions,
    document: Document,
    read: impl FnOnce() -> Result<Snapshot<T>, E> + Send + 'static,
) -> Result<Option<T>, String>
where
    T: Send + 'static,
    E: Error + Send + 'static,
{
    let snapshot = blocking(read).await?;
    if snapshot.version == documents.last_seen(&document) {
        return Ok(None);
    }
    if snapshot.version == FileVersion::Missing {
        documents.read(document, FileVersion::Missing);
        return Ok(None);
    }
    Ok(documents
        .changed_on_disk(document, snapshot.version)
        .then_some(snapshot.value))
}

#[cfg(test)]
mod tests {
    use dbrover_store::BlobId;

    use super::*;

    fn blob(text: &str) -> FileVersion {
        FileVersion::Blob(BlobId::of(text.as_bytes()))
    }

    #[test]
    fn finds_entries_added_changed_and_removed() {
        let before = [(1, "one"), (2, "two"), (3, "three")];
        let after = [(1, "one"), (3, "THREE"), (4, "four")];

        let changes = entry_changes(&before, &after, |entry| entry.0);

        assert_eq!(changes.saved, [(3, "THREE"), (4, "four")]);
        assert_eq!(changes.removed, [2]);
        assert!(entry_changes(&before, &before, |entry| entry.0).is_empty());
    }

    #[test]
    fn changes_as_of_a_save_show_the_saved_entries() {
        let mut changes = entry_changes(&[(1, "one"), (2, "two")], &[(1, "ONE")], |e| e.0);

        changes.as_of(&[(1, "mine"), (2, "two again")], |e| e.0);

        assert_eq!(changes.saved, [(1, "mine")]);
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn holds_saves_until_the_app_has_seen_a_change_on_disk() {
        let mut versions = DocumentVersions::default();
        assert_eq!(
            versions.expected(&Document::Settings),
            Some(FileVersion::Missing)
        );
        versions.read(Document::Settings, blob("a"));

        assert!(versions.changed_on_disk(Document::Settings, blob("b")));
        assert!(!versions.changed_on_disk(Document::Settings, blob("b")));
        assert_eq!(versions.expected(&Document::Settings), None);
        assert_eq!(versions.last_seen(&Document::Settings), blob("b"));

        versions.seen(&Document::Settings);
        assert_eq!(versions.expected(&Document::Settings), Some(blob("b")));
    }
}
