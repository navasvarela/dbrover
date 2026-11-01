//! Asks before closing a console, or quitting, would lose changes not written to `.sql` files.

use dbrover_store::ConsoleKey;
use egui::{Align, Id, Layout, Modal};

use crate::form;
use crate::rollback_dialog::Closing;

pub enum UnsavedChoice {
    /// Write the files, then close.
    Save,
    /// Close without writing them.
    Discard,
    Cancel,
}

pub struct UnsavedConfirmation {
    pub closing: Closing,
    /// The consoles with unsaved changes, and their titles.
    pub consoles: Vec<(ConsoleKey, String)>,
}

impl UnsavedConfirmation {
    pub fn show(&self, ctx: &egui::Context) -> Option<UnsavedChoice> {
        let names: Vec<&str> = self
            .consoles
            .iter()
            .map(|(_, title)| title.as_str())
            .collect();
        let modal = Modal::new(Id::new("unsaved_confirmation")).show(ctx, |ui| {
            ui.heading(match names.as_slice() {
                [name] => format!("Save the changes to {name}?"),
                _ => "Save the changes to these files?".to_owned(),
            });
            if names.len() > 1 {
                ui.label(names.join(", "));
            }
            ui.label("Your changes are lost if you don't save them.");
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if form::primary_button(ui, "Save", true).clicked() {
                    Some(UnsavedChoice::Save)
                } else if ui.button("Don't save").clicked() {
                    Some(UnsavedChoice::Discard)
                } else if ui.button("Cancel").clicked() {
                    Some(UnsavedChoice::Cancel)
                } else {
                    None
                }
            })
            .inner
        });
        if modal.should_close() {
            return Some(UnsavedChoice::Cancel);
        }
        modal.inner
    }
}
