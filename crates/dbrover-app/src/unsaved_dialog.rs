//! Asks before closing a console or a report's tab, or quitting, would lose changes not
//! written to their files.

use dbrover_store::{ConsoleKey, ReportName};
use egui::Id;

use crate::dialog::{self, DialogSize};
use crate::form;
use crate::icons;
use crate::rollback_dialog::Closing;

pub enum UnsavedChoice {
    /// Write the files, then close.
    Save,
    /// Close without writing them.
    Discard,
    Cancel,
}

/// A tab whose text isn't all written to its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsavedFile {
    Console(ConsoleKey),
    Report(ReportName),
}

pub struct UnsavedConfirmation {
    pub closing: Closing,
    /// The tabs with unsaved changes, and their titles.
    pub files: Vec<(UnsavedFile, String)>,
}

impl UnsavedConfirmation {
    pub fn show(&self, ctx: &egui::Context) -> Option<UnsavedChoice> {
        let names: Vec<&str> = self.files.iter().map(|(_, title)| title.as_str()).collect();
        let title = match names.as_slice() {
            [name] => format!("Save the changes to {name}?"),
            _ => "Save the changes to these files?".to_owned(),
        };
        let id = Id::new("unsaved_confirmation");
        let modal = dialog::modal(ctx, id, DialogSize::Confirm).show(ctx, |ui| {
            form::Header::new(icons::FLOPPY_DISK, &title).show(ui);
            if names.len() > 1 {
                ui.label(names.join(", "));
            }
            ui.label("Your changes are lost if you don't save them.");
            let (discard, choice) = form::footer_with_leading(
                ui,
                |ui| form::discard_button(ui, "Don't save").clicked(),
                |ui| {
                    if form::primary_button(ui, "Save", true).clicked() {
                        Some(UnsavedChoice::Save)
                    } else if ui.button("Cancel").clicked() {
                        Some(UnsavedChoice::Cancel)
                    } else {
                        None
                    }
                },
            );
            discard.then_some(UnsavedChoice::Discard).or(choice)
        });
        if modal.should_close() {
            return Some(UnsavedChoice::Cancel);
        }
        modal.inner
    }
}
