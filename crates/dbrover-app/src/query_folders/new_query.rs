//! **New query here** on a query folder: asks for a name, and has the engine create an empty
//! `.sql` file there, which then opens like any other.

use std::path::PathBuf;

use dbrover_core::DataSourceId;
use dbrover_engine::Event;
use egui::{Id, TextEdit, Ui};

use crate::dialog::{self, DialogSize};
use crate::form;
use crate::icons;
use crate::theme::Palette;

const FIELD_WIDTH: f32 = 280.0;
const SQL_SUFFIX: &str = ".sql";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewQueryAction {
    Create(PathBuf),
    Close,
}

/// The file the dialog created, and the data source it opens on: the focused console's when
/// the dialog was opened, or `None` to ask as **File › Open…** does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedQuery {
    pub path: PathBuf,
    pub data_source: Option<DataSourceId>,
}

/// Stays open until the engine has created the file, so a failure (a file of that name made
/// meanwhile) shows where the name was typed.
pub struct NewQueryDialog {
    folder: PathBuf,
    data_source: Option<DataSourceId>,
    name: String,
    creating: Option<PathBuf>,
    error: Option<String>,
    focus_requested: bool,
}

impl NewQueryDialog {
    pub fn new(folder: PathBuf, data_source: Option<DataSourceId>) -> Self {
        Self {
            folder,
            data_source,
            name: String::new(),
            creating: None,
            error: None,
            focus_requested: false,
        }
    }

    /// The file created, once the engine says so; the dialog is then done.
    pub fn apply(&mut self, event: &Event) -> Option<CreatedQuery> {
        let Event::SqlFileCreated { path, result } = event else {
            return None;
        };
        if self.creating.as_ref() != Some(path) {
            return None;
        }
        self.creating = None;
        match result {
            Ok(()) => Some(CreatedQuery {
                path: path.clone(),
                data_source: self.data_source,
            }),
            Err(error) => {
                self.error = Some(error.clone());
                None
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<NewQueryAction> {
        let id = Id::new("new_query_dialog");
        let modal = dialog::modal(ctx, id, DialogSize::Form).show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(NewQueryAction::Close);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut Ui) -> Option<NewQueryAction> {
        let palette = Palette::for_visuals(ui.visuals());
        form::Header::new(icons::FILE_PLUS, "New query").show(ui);
        let mut submitted = false;
        ui.horizontal(|ui| {
            let label = ui.label("File name");
            let field = ui
                .add(TextEdit::singleline(&mut self.name).desired_width(FIELD_WIDTH))
                .labelled_by(label.id);
            ui.weak(SQL_SUFFIX);
            if !std::mem::replace(&mut self.focus_requested, true) {
                field.request_focus();
            }
            submitted = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        });
        ui.weak(format!("In {}", self.folder.display()));
        let file_name = file_name(&self.name);
        match (&file_name, &self.error) {
            (Err(NameProblem::Empty), _) => {}
            (Err(problem), _) => {
                ui.colored_label(palette.error, problem.message());
            }
            (Ok(_), Some(error)) => {
                ui.colored_label(palette.error, format!("Could not create it: {error}"));
            }
            (Ok(_), None) => {}
        }
        let creatable = file_name.is_ok() && self.creating.is_none();
        form::footer(ui, |ui| {
            let confirmed = form::primary_button(ui, "Create", creatable).clicked();
            if ui.button("Cancel").clicked() {
                return Some(NewQueryAction::Close);
            }
            if !(creatable && (confirmed || submitted)) {
                return None;
            }
            let path = self.folder.join(file_name.ok()?);
            self.creating = Some(path.clone());
            self.error = None;
            Some(NewQueryAction::Create(path))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameProblem {
    Empty,
    /// A separator or `..` would put the file somewhere other than the folder asked for.
    NotHere,
}

impl NameProblem {
    fn message(self) -> &'static str {
        match self {
            Self::Empty => "Type the file's name.",
            Self::NotHere => "A file name has no folder in it, and doesn't start with a dot.",
        }
    }
}

/// The file's name from what was typed, ending in `.sql` whether or not it was typed.
fn file_name(typed: &str) -> Result<String, NameProblem> {
    let typed = typed.trim();
    let stem = match typed.len().checked_sub(SQL_SUFFIX.len()) {
        Some(end)
            if typed.is_char_boundary(end) && typed[end..].eq_ignore_ascii_case(SQL_SUFFIX) =>
        {
            &typed[..end]
        }
        _ => typed,
    };
    if stem.is_empty() {
        return Err(NameProblem::Empty);
    }
    if stem.starts_with('.') || stem.contains(['/', '\\']) {
        return Err(NameProblem::NotHere);
    }
    Ok(format!("{stem}{SQL_SUFFIX}"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn names_the_file_with_sql_at_its_end_and_nowhere_but_here() {
        let cases = [
            ("refunds", Ok("refunds.sql")),
            (" refunds.sql ", Ok("refunds.sql")),
            ("Refunds.SQL", Ok("Refunds.sql")),
            ("monthly report", Ok("monthly report.sql")),
            ("ünïcode", Ok("ünïcode.sql")),
            ("", Err(NameProblem::Empty)),
            (".sql", Err(NameProblem::Empty)),
            ("../refunds", Err(NameProblem::NotHere)),
            ("admin/refunds", Err(NameProblem::NotHere)),
            ("admin\\refunds", Err(NameProblem::NotHere)),
            (".hidden", Err(NameProblem::NotHere)),
        ];
        for (typed, expected) in cases {
            assert_eq!(file_name(typed), expected.map(str::to_owned), "{typed:?}");
        }
    }

    #[test]
    fn is_done_only_once_its_own_file_is_created() {
        let source = DataSourceId::generate();
        let mut dialog = NewQueryDialog::new("/repo/queries".into(), Some(source));
        let path = PathBuf::from("/repo/queries/refunds.sql");
        dialog.creating = Some(path.clone());
        let created = |path: &Path, result: Result<(), String>| Event::SqlFileCreated {
            path: path.to_owned(),
            result,
        };

        assert_eq!(
            dialog.apply(&created(Path::new("/other.sql"), Ok(()))),
            None
        );
        assert_eq!(
            dialog.apply(&created(&path, Err("refunds.sql is already there".into()))),
            None
        );
        assert!(dialog.error.is_some());

        dialog.creating = Some(path.clone());
        assert_eq!(
            dialog.apply(&created(&path, Ok(()))),
            Some(CreatedQuery {
                path,
                data_source: Some(source),
            })
        );
    }
}
