//! The review panel: the SQL a helper will run, highlighted and read-only, with Execute, Open
//! in console and Copy, and the database's error when it fails.

use dbrover_core::SafetyLevel;
use dbrover_engine::DdlFailure;
use dbrover_language::QueryLanguage;
use dbrover_sql::DdlError;
use dbrover_store::redact_secrets;
use egui::{Align, Id, Layout, ScrollArea, TextEdit, Ui};

use super::Target;
use crate::console::highlight::{self, Marks};
use crate::theme::Palette;
use crate::{icons, widgets};

/// The SQL's share of the window: enough for a table of a dozen columns before it scrolls.
const SQL_HEIGHT: f32 = 220.0;

pub(super) enum ReviewAction {
    Execute,
    OpenInConsole,
    Cancel,
}

#[derive(Default)]
pub(super) struct Review {
    state: State,
    /// Execute was clicked on a careful data source, which asks first.
    confirming: bool,
}

#[derive(Default)]
enum State {
    #[default]
    Ready,
    /// Sent; waiting for the engine's answer.
    Running,
    Failed {
        message: String,
        /// Statements that committed before the failure, where DDL isn't transactional.
        applied: usize,
    },
}

/// Why Copy and Open in console are off while the SQL holds a password: the redacted text
/// would set a password nobody knows, and the real one would leave the verifier on the
/// clipboard.
const HOLDS_PASSWORD: &str = "The SQL holds a password, so it runs from here only: what it \
     sends stays out of the clipboard and the console";

/// The SQL as the panel shows it: the script with its secrets redacted, as the history
/// records it.
struct Shown {
    text: String,
    holds_password: bool,
}

impl Shown {
    fn new(script: &str) -> Self {
        let text = redact_secrets(script);
        Self {
            holds_password: text != script,
            text: text.into_owned(),
        }
    }
}

/// The statements as one script, each ended with `;`, as the panel shows them and a console
/// receives them.
pub(super) fn script(statements: &[String]) -> String {
    statements
        .iter()
        .map(|statement| format!("{statement};\n"))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Review {
    pub(super) fn is_running(&self) -> bool {
        matches!(self.state, State::Running)
    }

    pub(super) fn failed(&mut self, failure: &DdlFailure) {
        if self.is_running() {
            self.state = State::Failed {
                message: failure.message.clone(),
                applied: failure.applied,
            };
        }
    }

    /// The statements never reached the database, e.g. because it couldn't be connected to.
    pub(super) fn not_sent(&mut self, message: &str) {
        if self.is_running() {
            self.state = State::Failed {
                message: message.to_owned(),
                applied: 0,
            };
        }
    }

    /// `confirmed` is whether the destruction, if any, has been confirmed.
    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        target: &Target,
        statements: &Result<Vec<String>, DdlError>,
        confirmed: bool,
    ) -> Option<ReviewAction> {
        let palette = &Palette::for_visuals(ui.visuals());
        ui.strong("SQL");
        let shown = match statements {
            Ok(statements) => {
                let shown = Shown::new(&script(statements));
                sql(ui, target, &shown.text);
                notes(ui, target, statements.len(), shown.holds_password);
                Some(shown)
            }
            Err(error) => {
                ui.colored_label(palette.warning, error.to_string());
                None
            }
        };
        if let State::Failed { message, applied } = &self.state {
            ui.colored_label(palette.error, message);
            if *applied > 0 {
                ui.colored_label(palette.warning, applied_note(*applied));
            }
        }
        ui.separator();
        let action = self.buttons(ui, target, shown.as_ref(), confirmed);
        let action = match action {
            Some(ReviewAction::Execute) if target.safety == SafetyLevel::Careful => {
                self.confirming = true;
                None
            }
            action => action,
        };
        let action = action.or_else(|| self.careful_confirmation(ui.ctx(), target));
        if matches!(action, Some(ReviewAction::Execute)) {
            self.state = State::Running;
        }
        action
    }

    fn buttons(
        &self,
        ui: &mut Ui,
        target: &Target,
        shown: Option<&Shown>,
        confirmed: bool,
    ) -> Option<ReviewAction> {
        let running = self.is_running();
        let writable = target.safety != SafetyLevel::ReadOnly;
        let ready = shown.is_some() && !running;
        let shareable = shown.is_some_and(|shown| !shown.holds_password);
        // Inside a row, so the buttons take one line's height rather than all that is left.
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let execute = ui
                    .add_enabled(
                        ready && confirmed && writable,
                        egui::Button::new(widgets::icon_text(
                            ui,
                            icons::PLAY,
                            egui::Color32::PLACEHOLDER,
                            "Execute",
                        )),
                    )
                    .on_disabled_hover_text(execute_disabled_reason(target, confirmed, running));
                let execute = widgets::name(execute, egui::WidgetType::Button, "Execute", ready);
                if running {
                    ui.spinner();
                }
                let console = ui
                    .add_enabled(ready && shareable, egui::Button::new("Open in console"))
                    .on_disabled_hover_text(HOLDS_PASSWORD);
                let copy = ui
                    .add_enabled(shareable, egui::Button::new("Copy"))
                    .on_disabled_hover_text(HOLDS_PASSWORD);
                let cancel = ui.add_enabled(!running, egui::Button::new("Cancel"));
                if copy.clicked()
                    && let Some(shown) = shown
                {
                    ui.ctx().copy_text(shown.text.clone());
                }
                if execute.clicked() {
                    Some(ReviewAction::Execute)
                } else if console.clicked() {
                    Some(ReviewAction::OpenInConsole)
                } else if cancel.clicked() {
                    Some(ReviewAction::Cancel)
                } else {
                    None
                }
            })
            .inner
        })
        .inner
    }

    /// On a careful data source, Execute asks once more, naming the data source.
    fn careful_confirmation(
        &mut self,
        ctx: &egui::Context,
        target: &Target,
    ) -> Option<ReviewAction> {
        if !self.confirming {
            return None;
        }
        let modal = widgets::dialog(ctx, Id::new("object_dialog_careful")).show(ctx, |ui| {
            ui.heading(format!("Run on \u{201c}{}\u{201d}?", target.name));
            ui.label(format!(
                "\u{201c}{}\u{201d} is careful, and this changes the schema.",
                target.name
            ));
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Run").clicked() {
                    Some(true)
                } else if ui.button("Cancel").clicked() {
                    Some(false)
                } else {
                    None
                }
            })
            .inner
        });
        let choice = if modal.should_close() {
            Some(false)
        } else {
            modal.inner
        };
        match choice {
            Some(run) => {
                self.confirming = false;
                run.then_some(ReviewAction::Execute)
            }
            None => None,
        }
    }
}

fn execute_disabled_reason(target: &Target, confirmed: bool, running: bool) -> String {
    if target.safety == SafetyLevel::ReadOnly {
        format!(
            "\u{201c}{}\u{201d} is read-only: open the SQL in a console or copy it instead",
            target.name
        )
    } else if running {
        "Running…".to_owned()
    } else if !confirmed {
        "Confirm above first".to_owned()
    } else {
        "Fill in the form first".to_owned()
    }
}

/// The SQL, highlighted as the console highlights it, selectable but not editable.
fn sql(ui: &mut Ui, target: &Target, text: &str) {
    let marks = Marks::default();
    let mut layouter = highlight::layouter(QueryLanguage::Sql(target.dialect), &marks);
    ScrollArea::vertical()
        .id_salt("object_dialog_sql")
        .max_height(SQL_HEIGHT)
        .show(ui, |ui| {
            // A `&str` buffer makes the editor read-only but still selectable.
            ui.add(
                TextEdit::multiline(&mut &*text)
                    .id_salt("object_dialog_sql_text")
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .layouter(&mut layouter),
            );
        });
}

/// What the user should know before Execute: that a read-only data source refuses it, where
/// DDL isn't transactional, that a failure part way keeps what ran before it, and how a
/// password is sent.
fn notes(ui: &mut Ui, target: &Target, statements: usize, holds_password: bool) {
    let palette = &Palette::for_visuals(ui.visuals());
    if holds_password {
        ui.horizontal_wrapped(|ui| {
            ui.label(widgets::glyph(icons::LOCK_SIMPLE).color(palette.success));
            ui.label(
                "The password is hashed here into a SCRAM-SHA-256 verifier, which is sent in \
                 its place, and redacted here, in the history and in the log.",
            );
        });
    }
    if target.safety == SafetyLevel::ReadOnly {
        ui.horizontal_wrapped(|ui| {
            ui.label(widgets::glyph(icons::LOCK_SIMPLE).color(palette.warning));
            ui.label(format!(
                "\u{201c}{}\u{201d} is read-only, so this can't run here.",
                target.name
            ));
        });
    }
    if statements > 1 && !target.dialect.capabilities().transactional_ddl {
        ui.weak(format!(
            "{} commits each statement as it runs: if one fails, the ones before it stay \
             applied.",
            target.dialect.display_name()
        ));
    }
}

fn applied_note(applied: usize) -> String {
    match applied {
        1 => "The first statement ran and stays applied.".to_owned(),
        n => format!("The first {n} statements ran and stay applied."),
    }
}
