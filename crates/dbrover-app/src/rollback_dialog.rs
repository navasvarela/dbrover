//! Asks before an action that closes connections with uncommitted transactions, which the
//! database then rolls back.

use dbrover_core::DataSourceId;
use dbrover_store::{ConsoleKey, ReportName};
use egui::Id;

use crate::dialog::{self, DialogSize};
use crate::form;
use crate::icons;

/// An action that closes console connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Closing {
    Console(ConsoleKey),
    /// A report's tab, which has no connection: only its unsaved text is asked about.
    Report(ReportName),
    /// Disconnecting a data source closes its consoles' connections too.
    Disconnect(DataSourceId),
    Quit,
}

impl Closing {
    /// Whether the action closes this console's connection.
    pub fn affects(&self, console: &ConsoleKey) -> bool {
        match self {
            Self::Console(key) => key == console,
            Self::Report(_) => false,
            Self::Disconnect(data_source) => console.data_source == *data_source,
            Self::Quit => true,
        }
    }

    fn verb(&self) -> &'static str {
        match self {
            Self::Console(_) | Self::Report(_) => "close",
            Self::Disconnect(_) => "disconnect",
            Self::Quit => "quit",
        }
    }
}

pub enum RollbackChoice {
    Proceed,
    Cancel,
}

pub struct RollbackConfirmation {
    pub closing: Closing,
    /// Titles of the consoles whose transactions would be rolled back.
    consoles: Vec<String>,
}

impl RollbackConfirmation {
    pub fn new(closing: Closing, consoles: Vec<String>) -> Self {
        Self { closing, consoles }
    }

    pub fn show(&self, ctx: &egui::Context) -> Option<RollbackChoice> {
        let verb = self.closing.verb();
        let id = Id::new("rollback_confirmation");
        let modal = dialog::modal(ctx, id, DialogSize::Confirm).show(ctx, |ui| {
            form::Header::new(
                icons::ARROW_COUNTER_CLOCKWISE,
                "Roll back open transactions?",
            )
            .danger()
            .show(ui);
            ui.label(format!(
                "These consoles have uncommitted changes: {}. If you {verb}, they are rolled back.",
                self.consoles.join(", ")
            ));
            form::footer(ui, |ui| {
                if form::danger_button(ui, &format!("Roll back and {verb}"), true).clicked() {
                    Some(RollbackChoice::Proceed)
                } else if ui.button("Cancel").clicked() {
                    Some(RollbackChoice::Cancel)
                } else {
                    None
                }
            })
        });
        if modal.should_close() {
            return Some(RollbackChoice::Cancel);
        }
        modal.inner
    }
}
