//! Asks whether to trust an SSH server DbRover hasn't connected to before, showing its host
//! key's fingerprint. Opened by [`Event::HostKeyUnknown`]; trusting adds the key to
//! `~/.ssh/known_hosts`, as `ssh` does when asked the same question.

use dbrover_core::{DataSource, DataSourceId};
use dbrover_engine::{Event, HostKey};
use egui::{Align, Id, Layout, Modal, RichText, TextEdit};

use crate::form;
use crate::theme::Palette;

const DIALOG_WIDTH: f32 = 460.0;

pub enum HostKeyAction {
    Trust(HostKey),
    Cancel,
}

pub struct HostKeyPrompt {
    data_source: DataSourceId,
    name: String,
    key: HostKey,
    state: State,
}

enum State {
    Asking,
    Trusting,
    Failed(String),
}

impl HostKeyPrompt {
    pub fn new(data_source: &DataSource, key: HostKey) -> Self {
        Self {
            data_source: data_source.id,
            name: data_source.name.clone(),
            key,
            state: State::Asking,
        }
    }

    pub fn data_source(&self) -> DataSourceId {
        self.data_source
    }

    /// Whether `event` says the key is now trusted, meaning the prompt is done. A failure to
    /// write `known_hosts` keeps it open, showing why.
    pub fn apply(&mut self, event: &Event) -> bool {
        match event {
            Event::HostKeyTrusted {
                data_source,
                result,
            } if *data_source == self.data_source => match result {
                Ok(()) => true,
                Err(error) => {
                    self.state = State::Failed(error.clone());
                    false
                }
            },
            _ => false,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<HostKeyAction> {
        let modal = Modal::new(Id::new("host_key_prompt")).show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(HostKeyAction::Cancel);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut egui::Ui) -> Option<HostKeyAction> {
        ui.set_width(DIALOG_WIDTH);
        let palette = &Palette::for_visuals(ui.visuals());
        ui.heading(format!("Trust the SSH server {}?", self.key.server()));
        ui.label(RichText::new(&self.name).color(palette.text_weak));
        ui.add_space(ui.spacing().item_spacing.y);
        ui.label(
            "DbRover hasn't connected to this SSH server before, so it can't tell whether the \
             server is who it claims to be.",
        );
        ui.label(format!(
            "Its {} host key's fingerprint is:",
            self.key.algorithm()
        ));
        let mut fingerprint = self.key.fingerprint();
        ui.add(
            TextEdit::singleline(&mut fingerprint)
                .font(egui::TextStyle::Monospace)
                .desired_width(DIALOG_WIDTH),
        );
        ui.label(
            RichText::new(
                "Compare it with the one the server's administrator gives you, from \
                 ssh-keygen -lf on the server's host key. Trusting adds it to \
                 ~/.ssh/known_hosts, and a different key is refused from then on.",
            )
            .small()
            .color(palette.text_weak),
        );
        if let State::Failed(error) = &self.state {
            ui.colored_label(palette.error, format!("Could not trust it: {error}"));
        }
        ui.separator();
        let trusting = matches!(self.state, State::Trusting);
        let (trust, cancel) = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                let trust = form::primary_button(ui, "Trust and connect", !trusting).clicked();
                (trust, ui.button("Cancel").clicked())
            })
            .inner;
        if cancel {
            return Some(HostKeyAction::Cancel);
        }
        if trust && !trusting {
            self.state = State::Trusting;
            return Some(HostKeyAction::Trust(self.key.clone()));
        }
        None
    }
}
