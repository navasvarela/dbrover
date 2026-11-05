//! A report's file as its tab edits it (`docs/report-editor.md`, RP8e): the editor's text,
//! what the file held when last read or written, a save in flight, and the plan of the text
//! as typed.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant, SystemTime};

use dbrover_core::DataSource;
use dbrover_engine::{Command, ReportSaveFailure};
use dbrover_export::CustomFormat;
use dbrover_report::{LimitDefaults, Plan, ReportProblem, format_report, plan_text};
use dbrover_sql::FormatOptions;
use dbrover_store::{ReportFiles, ReportName, ReportRun};

use super::problems::{ProblemInputs, TextProblem, text_problems};
use crate::code_editor::CodeEditor;
use crate::console::LintOptions;

/// How long typing must pause before the text is checked again: the console's, long enough
/// not to flag every half-typed key, short enough to feel live.
const DEBOUNCE: Duration = Duration::from_millis(300);

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
    /// The text's plan and problems, and the fingerprint of what they were found from.
    checked: Option<(u64, Checked)>,
    /// The fingerprint of a text not checked yet, and when it was first seen.
    changed: Option<(u64, Instant)>,
    /// The last plan the text had without problems, which the parameters keep showing
    /// (greyed) while a typo is being fixed.
    last_plan: Option<Plan>,
    /// What the last **Format** left as written, or why it formatted nothing, with the text it
    /// is about: shown until the text changes.
    format_note: Option<(String, String)>,
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

/// What the text is checked against besides itself.
pub struct CheckInputs<'a> {
    pub data_sources: &'a [DataSource],
    pub presets: &'a [CustomFormat],
    pub defaults: &'a LimitDefaults,
    pub lint: &'a LintOptions,
    /// The report's newest runs, newest first, which the hints come from.
    pub runs: &'a [ReportRun],
}

/// The text's plan and every problem it has.
#[derive(Debug, Clone)]
pub struct Checked {
    pub plan: Result<Plan, Vec<ReportProblem>>,
    pub problems: Vec<TextProblem>,
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
            changed: None,
            last_plan: None,
            format_note: None,
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

    /// The text's plan and problems against `inputs`, found again once typing has paused
    /// for [`DEBOUNCE`] or the inputs change; until then the last ones, and how long to wait
    /// before drawing again to check.
    pub fn check(
        &mut self,
        inputs: &CheckInputs<'_>,
        now: Instant,
    ) -> (&Checked, Option<Duration>) {
        let fingerprint = fingerprint(&self.editor.text, inputs);
        let current = self
            .checked
            .as_ref()
            .is_some_and(|(checked, _)| *checked == fingerprint);
        let mut wait = None;
        if !current {
            let since = match self.changed {
                Some((changed, since)) if changed == fingerprint => since,
                _ => now,
            };
            self.changed = Some((fingerprint, since));
            let paused = now.saturating_duration_since(since);
            if self.checked.is_none() || paused >= DEBOUNCE {
                self.checked = Some((fingerprint, self.found(inputs)));
                self.changed = None;
            } else {
                wait = Some(DEBOUNCE - paused);
            }
        }
        let text = &self.editor.text;
        let (_, checked) = self
            .checked
            .get_or_insert_with(|| (fingerprint, found(text, inputs)));
        (checked, wait)
    }

    fn found(&mut self, inputs: &CheckInputs<'_>) -> Checked {
        let checked = found(&self.editor.text, inputs);
        if let Ok(plan) = &checked.plan {
            self.last_plan = Some(plan.clone());
        }
        checked
    }

    /// Formats the text as one undo step, unsaved until saved. What wasn't formatted, and why,
    /// is the format note.
    pub fn format(&mut self, data_sources: &[DataSource], options: &FormatOptions) {
        let note = match format_report(&self.editor.text, data_sources, options) {
            Ok(formatted) => {
                if formatted.text != self.editor.text {
                    self.editor.replace_keeping_cursor(formatted.text);
                }
                (!formatted.left_as_written.is_empty())
                    .then(|| formatted.left_as_written.join("; "))
            }
            Err(error) => Some(error.to_string()),
        };
        self.format_note = note.map(|note| (self.editor.text.clone(), note));
    }

    /// What the last **Format** said, while the text is what it formatted.
    pub fn format_note(&self) -> Option<&str> {
        self.format_note
            .as_ref()
            .filter(|(text, _)| *text == self.editor.text)
            .map(|(_, note)| note.as_str())
    }

    /// The last plan the text had without problems.
    pub fn last_plan(&self) -> Option<&Plan> {
        self.last_plan.as_ref()
    }
}

/// The text's plan and problems.
fn found(text: &str, inputs: &CheckInputs<'_>) -> Checked {
    let plan = plan_text(text, inputs.data_sources, inputs.presets, inputs.defaults);
    let problems = text_problems(
        text,
        &plan,
        &ProblemInputs {
            data_sources: inputs.data_sources,
            lint: inputs.lint,
            runs: inputs.runs,
        },
    );
    Checked { plan, problems }
}

/// A hash of what the check depends on: the text, the names, ids and kinds of the data
/// sources and presets it may name, the limits' defaults, the lint rules, and the newest run.
fn fingerprint(text: &str, inputs: &CheckInputs<'_>) -> u64 {
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
    format!("{:?}", inputs.lint).hash(&mut hasher);
    inputs.runs.len().hash(&mut hasher);
    if let Some(newest) = inputs.runs.first() {
        newest.started_at.to_string().hash(&mut hasher);
    }
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
    fn checks_again_once_typing_pauses() {
        let lint = LintOptions::default();
        let defaults = LimitDefaults::default();
        let inputs = CheckInputs {
            data_sources: &[],
            presets: &[],
            defaults: &defaults,
            lint: &lint,
            runs: &[],
        };
        let start = Instant::now();
        let mut file = ReportFile::new("[report]\nname = \"R\"\n".to_owned(), None);
        let (first, wait) = file.check(&inputs, start);
        let first_problems = first.problems.len();
        assert_eq!(wait, None, "the first check is at once");

        file.editor.text.push_str("oops = \n");
        let (stale, wait) = file.check(&inputs, start + Duration::from_millis(100));
        assert_eq!(stale.problems.len(), first_problems, "still typing");
        assert_eq!(wait, Some(DEBOUNCE));
        let (fresh, wait) = file.check(&inputs, start + Duration::from_millis(100) + DEBOUNCE);
        assert_eq!(wait, None);
        assert!(
            fresh
                .problems
                .iter()
                .any(|problem| problem.message.starts_with("line 3")),
            "{:?}",
            fresh.problems
        );
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
