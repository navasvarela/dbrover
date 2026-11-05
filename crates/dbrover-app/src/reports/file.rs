//! A report's file as its tab edits it (`docs/report-editor.md`, RP8e): the editor's text,
//! what the file held when last read or written, a save in flight, and the plan of the text
//! as typed.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::SystemTime;

use dbrover_core::DataSource;
use dbrover_engine::{Command, ReportSaveFailure};
use dbrover_export::CustomFormat;
use dbrover_report::{LimitDefaults, Plan, ReportProblem, plan_text};
use dbrover_store::{ReportFiles, ReportName};

use crate::code_editor::CodeEditor;

/// The text a report's tab edits, and how it stands against its file.
#[derive(Debug)]
pub struct ReportFile {
    pub editor: CodeEditor,
    /// What the file holds, as last read or written. The text is unsaved while it differs, so
    /// undoing back to it counts as saved.
    saved_text: String,
    /// The file's modification time when last read or written, which a save checks.
    read_at: Option<SystemTime>,
    /// The text of the save in flight.
    saving: Option<String>,
    /// Why the last save failed, until one succeeds.
    pub save_error: Option<SaveError>,
    /// Run once the save in flight is written: **Save and run**.
    run_after_save: bool,
    /// The plan of the text, and the fingerprint of what it was planned from.
    checked: Option<(u64, Result<Plan, Vec<ReportProblem>>)>,
    /// The last plan the text had without problems, which the parameters keep showing
    /// (greyed) while a typo is being fixed.
    last_plan: Option<Plan>,
}

/// Why a save failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// Another program changed the file since it was read; saving again replaces its change.
    ChangedOnDisk,
    Failed(String),
}

/// What a save's answer leads to.
#[derive(Debug, PartialEq, Eq)]
pub enum Saved {
    /// Written; the report's listing holds this text now.
    Written {
        text: String,
        then_run: bool,
    },
    NotWritten,
}

/// What the plan is checked against besides the text.
pub struct PlanInputs<'a> {
    pub data_sources: &'a [DataSource],
    pub presets: &'a [CustomFormat],
    pub defaults: &'a LimitDefaults,
}

impl ReportFile {
    /// A file just read: `text`, modified at `modified`.
    pub fn new(text: String, modified: Option<SystemTime>) -> Self {
        Self {
            editor: CodeEditor::new(text.clone()),
            saved_text: text,
            read_at: modified,
            saving: None,
            save_error: None,
            run_after_save: false,
            checked: None,
            last_plan: None,
        }
    }

    pub fn is_unsaved(&self) -> bool {
        self.editor.text != self.saved_text
    }

    pub fn is_saving(&self) -> bool {
        self.saving.is_some()
    }

    /// The save to send for the text as it is, unless one is in flight. After the file
    /// changed on disk, it saves over the change: the user saw it, and saved again.
    pub fn save(&mut self, files: &ReportFiles, name: &ReportName) -> Option<Command> {
        if self.saving.is_some() {
            return None;
        }
        let read_at = match self.save_error {
            Some(SaveError::ChangedOnDisk) => None,
            _ => self.read_at,
        };
        self.saving = Some(self.editor.text.clone());
        Some(Command::SaveReport {
            files: files.clone(),
            name: name.clone(),
            text: self.editor.text.clone(),
            read_at,
        })
    }

    /// Saves, then runs once the file is written. `None` when a save is already in flight,
    /// which then runs too.
    pub fn save_and_run(&mut self, files: &ReportFiles, name: &ReportName) -> Option<Command> {
        self.run_after_save = true;
        self.save(files, name)
    }

    /// Takes in a save's answer.
    pub fn saved(&mut self, result: &Result<Option<SystemTime>, ReportSaveFailure>) -> Saved {
        let Some(text) = self.saving.take() else {
            return Saved::NotWritten;
        };
        let then_run = std::mem::take(&mut self.run_after_save);
        match result {
            Ok(modified) => {
                self.saved_text.clone_from(&text);
                self.read_at = *modified;
                self.save_error = None;
                Saved::Written { text, then_run }
            }
            Err(ReportSaveFailure::ChangedOnDisk) => {
                self.save_error = Some(SaveError::ChangedOnDisk);
                Saved::NotWritten
            }
            Err(ReportSaveFailure::Failed(error)) => {
                self.save_error = Some(SaveError::Failed(error.clone()));
                Saved::NotWritten
            }
        }
    }

    /// Takes in the file as read again, when it isn't what was last read or written: the
    /// editor shows it if nothing typed would be lost, as one undo step.
    pub fn read_again(&mut self, text: &str, modified: Option<SystemTime>) {
        if text == self.saved_text || self.is_unsaved() || self.saving.is_some() {
            return;
        }
        text.clone_into(&mut self.saved_text);
        self.read_at = modified;
        self.editor.replace_keeping_cursor(text.to_owned());
    }

    /// The text's plan against `inputs`, planned again only when the text or the inputs
    /// change.
    pub fn plan(&mut self, inputs: &PlanInputs<'_>) -> &Result<Plan, Vec<ReportProblem>> {
        let fingerprint = fingerprint(&self.editor.text, inputs);
        let stale = self
            .checked
            .as_ref()
            .is_none_or(|(checked, _)| *checked != fingerprint);
        if stale {
            self.checked = None;
        }
        let text = &self.editor.text;
        let last_plan = &mut self.last_plan;
        let (_, planned) = self.checked.get_or_insert_with(|| {
            let planned = plan_text(text, inputs.data_sources, inputs.presets, inputs.defaults);
            if let Ok(plan) = &planned {
                *last_plan = Some(plan.clone());
            }
            (fingerprint, planned)
        });
        planned
    }

    /// The last plan the text had without problems.
    pub fn last_plan(&self) -> Option<&Plan> {
        self.last_plan.as_ref()
    }
}

/// A hash of what a plan depends on: the text, and the names, ids and kinds of the data
/// sources and presets it may name, with the limits' defaults.
fn fingerprint(text: &str, inputs: &PlanInputs<'_>) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    for data_source in inputs.data_sources {
        data_source.id.to_string().hash(&mut hasher);
        data_source.name.hash(&mut hasher);
        format!("{:?}", data_source.config.driver()).hash(&mut hasher);
    }
    for preset in inputs.presets {
        format!("{preset:?}").hash(&mut hasher);
    }
    format!("{:?}", inputs.defaults).hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> ReportName {
        ReportName::new("sales").unwrap()
    }

    #[test]
    fn saves_and_counts_undo_back_to_the_file_as_saved() {
        let files = ReportFiles::new("/tmp/reports");
        let mut file = ReportFile::new("a".to_owned(), None);
        assert!(!file.is_unsaved());
        file.editor.text.push('b');
        assert!(file.is_unsaved());

        let save = file.save(&files, &name());
        assert!(matches!(save, Some(Command::SaveReport { ref text, .. }) if text == "ab"));
        assert!(file.save(&files, &name()).is_none(), "one save at a time");
        assert_eq!(
            file.saved(&Ok(None)),
            Saved::Written {
                text: "ab".to_owned(),
                then_run: false
            }
        );
        assert!(!file.is_unsaved());

        file.editor.text.push('c');
        file.editor.text.pop();
        assert!(!file.is_unsaved(), "back to what was saved");
    }

    #[test]
    fn runs_after_saving_and_saves_over_a_change_once_told_of_it() {
        let files = ReportFiles::new("/tmp/reports");
        let read_at = Some(SystemTime::UNIX_EPOCH);
        let mut file = ReportFile::new("a".to_owned(), read_at);
        file.editor.text.push('b');

        let save = file.save_and_run(&files, &name());
        assert!(matches!(
            save,
            Some(Command::SaveReport {
                read_at: Some(_),
                ..
            })
        ));
        assert_eq!(
            file.saved(&Err(ReportSaveFailure::ChangedOnDisk)),
            Saved::NotWritten
        );
        assert_eq!(file.save_error, Some(SaveError::ChangedOnDisk));

        let again = file.save_and_run(&files, &name());
        assert!(matches!(
            again,
            Some(Command::SaveReport { read_at: None, .. })
        ));
        assert!(matches!(
            file.saved(&Ok(read_at)),
            Saved::Written { then_run: true, .. }
        ));
    }

    #[test]
    fn shows_the_file_read_again_only_when_nothing_typed_is_lost() {
        let mut file = ReportFile::new("a".to_owned(), None);
        file.read_again("theirs", None);
        assert_eq!(file.editor.text, "theirs");
        assert!(!file.is_unsaved());

        file.editor.text.push('!');
        file.read_again("theirs again", None);
        assert_eq!(file.editor.text, "theirs!");
    }
}
