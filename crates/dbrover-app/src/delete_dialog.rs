use dbrover_core::{DataSource, DataSourceId};
use egui::Id;

use crate::dialog::{self, DialogSize};
use crate::{form, icons};

pub enum DeleteChoice {
    Confirmed(DataSourceId),
    Cancelled,
}

/// Asks before deleting a data source, since that also removes its saved password.
pub struct DeleteConfirmation {
    id: DataSourceId,
    name: String,
}

impl DeleteConfirmation {
    pub fn new(data_source: &DataSource) -> Self {
        Self {
            id: data_source.id,
            name: data_source.name.clone(),
        }
    }

    pub fn show(&self, ctx: &egui::Context) -> Option<DeleteChoice> {
        let id = Id::new("delete_data_source");
        let modal = dialog::modal(ctx, id, DialogSize::Confirm).show(ctx, |ui| {
            form::Header::new(icons::TRASH, "Delete data source")
                .subtitle(&self.name)
                .danger()
                .show(ui);
            ui.label(format!(
                "Delete \"{}\"? Its saved password is removed from the keychain too.",
                self.name
            ));
            form::footer(ui, |ui| {
                if form::danger_button(ui, "Delete", true).clicked() {
                    Some(DeleteChoice::Confirmed(self.id))
                } else if ui.button("Cancel").clicked() {
                    Some(DeleteChoice::Cancelled)
                } else {
                    None
                }
            })
        });
        if modal.should_close() {
            return Some(DeleteChoice::Cancelled);
        }
        modal.inner
    }
}
