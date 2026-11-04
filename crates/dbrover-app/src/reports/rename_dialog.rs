//! **Rename…** on a report: its file's new name, which its runs in the history follow
//! (`docs/reporting.md`, "Finishing RP4").

use dbrover_engine::Event;
use dbrover_store::ReportName;
use egui::{Id, TextEdit, Ui};

use crate::dialog::{self, DialogSize};
use crate::form;
use crate::icons;
use crate::theme::Palette;

const FIELD_WIDTH: f32 = 280.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameAction {
    Rename { from: ReportName, to: ReportName },
    Close,
}

/// Asks for a report's new file name, and stays open until the engine has renamed it, so a
/// failure shows where the name was typed.
pub struct RenameReportDialog {
    from: ReportName,
    name: String,
    error: Option<String>,
    focus_requested: bool,
}

impl RenameReportDialog {
    pub fn new(from: ReportName) -> Self {
        Self {
            name: from.to_string(),
            from,
            error: None,
            focus_requested: false,
        }
    }

    /// Updates the dialog from an engine event. Returns `true` once its report is renamed,
    /// meaning the dialog is done.
    pub fn apply(&mut self, event: &Event) -> bool {
        match event {
            Event::ReportRenamed { from, result } if *from == self.from => match result {
                Ok(_) => true,
                Err(error) => {
                    self.error = Some(error.clone());
                    false
                }
            },
            _ => false,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<RenameAction> {
        let id = Id::new("rename_report_dialog");
        let modal = dialog::modal(ctx, id, DialogSize::Form).show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(RenameAction::Close);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut Ui) -> Option<RenameAction> {
        let palette = Palette::for_visuals(ui.visuals());
        form::Header::new(icons::PENCIL_SIMPLE, "Rename report").show(ui);
        let mut submitted = false;
        ui.horizontal(|ui| {
            let label = ui.label("File name");
            let field = ui
                .add(TextEdit::singleline(&mut self.name).desired_width(FIELD_WIDTH))
                .labelled_by(label.id);
            ui.weak(".toml");
            if !std::mem::replace(&mut self.focus_requested, true) {
                field.request_focus();
            }
            submitted = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        });
        ui.weak("Its runs move with it. Renamed outside DbRover, a report starts its runs afresh.");
        let to = self.new_name();
        match &to {
            Err(problem) => {
                ui.colored_label(palette.error, problem);
            }
            Ok(_) => {
                if let Some(error) = &self.error {
                    ui.colored_label(palette.error, format!("Could not rename: {error}"));
                }
            }
        }
        let to = to.ok().flatten();
        form::footer(ui, |ui| {
            let confirmed = form::primary_button(ui, "Rename", to.is_some()).clicked();
            if ui.button("Cancel").clicked() {
                return Some(RenameAction::Close);
            }
            let to = to.filter(|_| confirmed || submitted)?;
            Some(RenameAction::Rename {
                from: self.from.clone(),
                to,
            })
        })
    }

    /// The name typed, if it can be one; `None` while it is the report's own.
    fn new_name(&self) -> Result<Option<ReportName>, String> {
        let typed = self.name.trim();
        let typed = typed.strip_suffix(".toml").unwrap_or(typed);
        let name = ReportName::new(typed).ok_or_else(|| {
            "A file name has no folder in it, and doesn't start with a dot.".to_owned()
        })?;
        Ok((name != self.from).then_some(name))
    }
}
