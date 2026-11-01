//! Asks whether to run a data source's password command (W9) the first time it would run, or
//! the first time since it changed, showing it in full. Opened by
//! [`Event::PasswordCommandUntrusted`]; trusting records it in `trusted_commands.toml`.

use dbrover_core::{DataSource, DataSourceId, PasswordCommand};
use dbrover_engine::Event;
use dbrover_store::TrustedCommand;
use egui::{Align, Id, Layout, Modal, RichText, ScrollArea, TextEdit};

use crate::form;
use crate::theme::Palette;

const DIALOG_WIDTH: f32 = 460.0;
/// Rows of the command shown before it scrolls: a cloud CLI's token command, one item per
/// line, fits.
const COMMAND_ROWS: usize = 12;

pub enum PasswordCommandAction {
    Trust(PasswordCommand),
    Cancel,
}

pub struct PasswordCommandPrompt {
    data_source: DataSourceId,
    name: String,
    command: PasswordCommand,
    trusting: bool,
}

impl PasswordCommandPrompt {
    pub fn new(data_source: &DataSource, command: PasswordCommand) -> Self {
        Self {
            data_source: data_source.id,
            name: data_source.name.clone(),
            command,
            trusting: false,
        }
    }

    pub fn data_source(&self) -> DataSourceId {
        self.data_source
    }

    /// Whether `event` says the command is now trusted, meaning the prompt is done. One that
    /// couldn't be saved is still trusted until DbRover quits, and the engine logs why.
    pub fn apply(&self, event: &Event) -> bool {
        let Event::TrustedCommandsLoaded(trusted) = event else {
            return false;
        };
        trusted.contains(&TrustedCommand::password(self.data_source, &self.command))
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<PasswordCommandAction> {
        let modal =
            Modal::new(Id::new("password_command_prompt")).show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(PasswordCommandAction::Cancel);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut egui::Ui) -> Option<PasswordCommandAction> {
        ui.set_width(DIALOG_WIDTH);
        let palette = &Palette::for_visuals(ui.visuals());
        ui.heading("Run this command for a password?");
        ui.label(RichText::new(&self.name).color(palette.text_weak));
        ui.add_space(ui.spacing().item_spacing.y);
        ui.label(
            "This data source gets its password by running a program, which DbRover hasn't \
             run for it before. It runs with your permissions, directly rather than through a \
             shell, each item below one argument:",
        );
        // Each item on a line of its own, so an argument with spaces in it reads as one and
        // nothing is hidden in a long line.
        let mut shown = self.command.source().join("\n");
        ScrollArea::vertical()
            .max_height(ui.text_style_height(&egui::TextStyle::Monospace) * COMMAND_ROWS as f32)
            .show(ui, |ui| {
                ui.add(
                    TextEdit::multiline(&mut shown)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(self.command.source().len().min(COMMAND_ROWS))
                        .desired_width(DIALOG_WIDTH),
                );
            });
        ui.label(
            RichText::new(
                "{host}, {port}, {user} and {database} are filled in from the data source. \
                 What it prints is the password. Once you say yes, it runs on every \
                 connection without asking, until the command changes.",
            )
            .small()
            .color(palette.text_weak),
        );
        ui.separator();
        let (run, cancel) = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                let run = form::primary_button(ui, "Run and connect", !self.trusting).clicked();
                (run, ui.button("Cancel").clicked())
            })
            .inner;
        if cancel {
            return Some(PasswordCommandAction::Cancel);
        }
        if run && !self.trusting {
            self.trusting = true;
            return Some(PasswordCommandAction::Trust(self.command.clone()));
        }
        None
    }
}
