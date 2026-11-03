//! Asks whether to run a data source's password command (W9) the first time it would run, or
//! the first time since it changed, showing it in full. Opened by
//! [`Event::PasswordCommandUntrusted`]; trusting records it in `trusted_commands.toml`.

use dbrover_core::{DataSource, DataSourceId, PasswordCommand};
use dbrover_engine::Event;
use dbrover_store::TrustedCommand;
use egui::{Id, RichText, ScrollArea, TextEdit};

use crate::dialog::{self, DialogSize};
use crate::form;
use crate::icons;
use crate::theme::Palette;

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
        let id = Id::new("password_command_prompt");
        let modal = dialog::modal(ctx, id, DialogSize::Form).show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(PasswordCommandAction::Cancel);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut egui::Ui) -> Option<PasswordCommandAction> {
        let palette = &Palette::for_visuals(ui.visuals());
        form::Header::new(icons::TERMINAL_WINDOW, "Run this command for a password?")
            .subtitle(&self.name)
            .show(ui);
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
                        .desired_width(f32::INFINITY),
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
        let (run, cancel) = form::footer(ui, |ui| {
            let run = form::primary_button(ui, "Run and connect", !self.trusting).clicked();
            (run, ui.button("Cancel").clicked())
        });
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
