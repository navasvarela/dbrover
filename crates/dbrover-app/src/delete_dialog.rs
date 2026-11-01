use dbrover_core::{DataSource, DataSourceId};
use egui::Id;

use crate::form;
use crate::widgets;

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
        let modal = widgets::dialog(ctx, Id::new("delete_data_source")).show(ctx, |ui| {
            ui.heading("Delete data source");
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
