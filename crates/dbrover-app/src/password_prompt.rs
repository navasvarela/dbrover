//! Asks for a data source's password, or its SSH login's password or key passphrase, when
//! connecting finds none: nothing is saved, or the keychain can't be read. Opened by
//! [`Event::PasswordNeeded`].

use dbrover_core::{DataSource, DataSourceId, Password, SshLogin};
use dbrover_engine::{Event, PasswordKept, PasswordNeeded};
use dbrover_store::Secret;
use egui::{Align, Id, Layout, Modal, RichText, TextEdit};

use crate::form;
use crate::theme::Palette;

const FIELD_WIDTH: f32 = 260.0;

pub enum PromptAction {
    /// Give the engine the password, then connect again once it has it.
    Provide {
        password: Password,
        save: bool,
    },
    Cancel,
}

pub struct PasswordPrompt {
    data_source: DataSourceId,
    secret: Secret,
    name: String,
    /// Where the connection goes, so the user knows which password is meant.
    target: String,
    /// What is asked for, as the field's label and in the sentence above it.
    label: &'static str,
    wanted: String,
    reason: PasswordNeeded,
    password: String,
    /// Ticked to start with: a password typed once should not be asked for again.
    save: bool,
    submitted: bool,
    focus_requested: bool,
}

impl PasswordPrompt {
    pub fn new(data_source: &DataSource, secret: Secret, reason: PasswordNeeded) -> Self {
        let ssh = data_source
            .config
            .server()
            .and_then(|server| server.ssh.as_ref());
        let (label, wanted) = match (secret, ssh.map(|ssh| (ssh, &ssh.login))) {
            (Secret::Ssh, Some((ssh, SshLogin::KeyFile { key_file }))) => (
                "Passphrase",
                format!(
                    "the passphrase of the SSH key {} for {ssh}",
                    key_file.display()
                ),
            ),
            (Secret::Ssh, Some((ssh, _))) => ("SSH password", format!("the SSH password of {ssh}")),
            _ => ("Password", "the password".to_owned()),
        };
        Self {
            data_source: data_source.id,
            secret,
            name: data_source.name.clone(),
            target: data_source.config.to_string(),
            label,
            wanted,
            reason,
            password: String::new(),
            save: true,
            submitted: false,
            focus_requested: false,
        }
    }

    pub fn data_source(&self) -> DataSourceId {
        self.data_source
    }

    pub fn secret(&self) -> Secret {
        self.secret
    }

    /// Whether `event` answers this prompt's password, meaning the prompt is done. A password
    /// the keychain refused is still kept for the session, so that isn't a reason to stay.
    pub fn apply(&self, event: &Event) -> Option<PasswordKept> {
        match event {
            Event::PasswordProvided {
                data_source,
                secret,
                kept,
            } if *data_source == self.data_source && *secret == self.secret => Some(kept.clone()),
            _ => None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<PromptAction> {
        let modal = Modal::new(Id::new("password_prompt")).show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(PromptAction::Cancel);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut egui::Ui) -> Option<PromptAction> {
        let palette = &Palette::for_visuals(ui.visuals());
        ui.heading(format!("Connect to {}", self.name));
        ui.label(RichText::new(&self.target).color(palette.text_weak));
        ui.label(format!("Enter {}: {}.", self.wanted, self.reason));
        ui.add_space(ui.spacing().item_spacing.y);
        let entered = ui
            .horizontal(|ui| {
                let label = ui.label(self.label);
                let field = ui
                    .add(
                        TextEdit::singleline(&mut self.password)
                            .password(true)
                            .desired_width(FIELD_WIDTH),
                    )
                    .labelled_by(label.id);
                if !std::mem::replace(&mut self.focus_requested, true) {
                    field.request_focus();
                }
                field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
            })
            .inner;
        ui.checkbox(&mut self.save, "Save to keychain")
            .on_hover_text("Otherwise the password is kept only until DbRover quits");
        ui.separator();
        let (connect, cancel) = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                let connect = form::primary_button(ui, "Connect", !self.submitted).clicked();
                (connect, ui.button("Cancel").clicked())
            })
            .inner;
        if cancel {
            return Some(PromptAction::Cancel);
        }
        if (connect || entered) && !self.submitted {
            self.submitted = true;
            return Some(PromptAction::Provide {
                password: Password::new(self.password.clone()),
                save: self.save,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::{
        ConnectionConfig, DataSourceId, PostgresConfig, SafetyLevel, ServerConfig, SshConfig,
        TlsMode,
    };

    use super::*;

    fn behind_ssh(login: SshLogin) -> DataSource {
        DataSource {
            id: DataSourceId::generate(),
            name: "staging".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Postgres(PostgresConfig {
                server: ServerConfig {
                    host: "db".to_owned(),
                    port: 5432.into(),
                    database: "app".to_owned(),
                    user: "juan".to_owned(),
                    tls: TlsMode::Prefer,
                    ssh: Some(SshConfig {
                        host: "bastion".to_owned(),
                        port: 22.into(),
                        user: "deploy".to_owned(),
                        login,
                    }),
                    password_command: None,
                },
            }),
        }
    }

    #[test]
    fn names_the_secret_it_asks_for() {
        let key = SshLogin::KeyFile {
            key_file: "/home/juan/.ssh/id_ed25519".into(),
        };
        let cases = [
            (
                Secret::Database,
                SshLogin::Password,
                "Password",
                "the password",
            ),
            (
                Secret::Ssh,
                SshLogin::Password,
                "SSH password",
                "the SSH password of deploy@bastion:22",
            ),
            (
                Secret::Ssh,
                key,
                "Passphrase",
                "the passphrase of the SSH key /home/juan/.ssh/id_ed25519 for deploy@bastion:22",
            ),
        ];
        for (secret, login, label, wanted) in cases {
            let prompt = PasswordPrompt::new(&behind_ssh(login), secret, PasswordNeeded::NoneSaved);
            assert_eq!((prompt.label, prompt.wanted.as_str()), (label, wanted));
        }
    }
}
