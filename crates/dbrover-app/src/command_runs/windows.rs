//! What command runs draw: the question before a command's first run, the windows a finished
//! program opens, and the status bar's note of what is running.

use dbrover_engine::{Command, ProgramFailure};
use egui::{Align, Grid, Id, Layout, RichText, Window};

use super::{
    CommandRuns, OutputWindow, Pending, Question, command_text, output_text, stdin_text,
    timeout_text,
};
use crate::console::cell_viewer;
use crate::form;
use crate::theme::Palette;
use crate::widgets;

/// Says so when a program wrote more than the engine keeps of it.
pub(super) const CUT_SHORT: &str = "the program wrote more than 4 MB and was stopped; this is \
     what it wrote first";

const WINDOW_SIZE: [f32; 2] = [520.0, 360.0];

impl CommandRuns {
    /// Draws the question and the output windows, and returns what to send the engine.
    pub fn show(&mut self, ctx: &egui::Context) -> Vec<Command> {
        let answer = self.asking.as_ref().and_then(|question| question.show(ctx));
        self.windows.retain(|window| window.show(ctx));
        answer.map(|run| self.answer(run)).unwrap_or_default()
    }

    /// The status bar's note: each running program with **Stop**, or how the last one ended.
    /// Returns the stops asked for.
    pub fn status(&mut self, ui: &mut egui::Ui, palette: &Palette) -> Vec<Command> {
        let mut stops = Vec::new();
        for (&program, run) in &self.running {
            ui.label(
                RichText::new(format!("Running {}…", run.name))
                    .small()
                    .color(palette.text_weak),
            );
            if ui.link(RichText::new("Stop").small()).clicked() {
                stops.push(Command::StopProgram(program));
            }
        }
        if let Some(notice) = &self.notice
            && ui
                .link(RichText::new(notice).small().color(palette.text_weak))
                .on_hover_text("Dismiss")
                .clicked()
        {
            self.notice = None;
        }
        stops
    }
}

/// What the question shows of a program: whose it is, its command in full, and what goes in
/// and out of it.
pub struct ProgramFacts<'a> {
    pub name: &'a str,
    /// The file it comes from: `actions.toml` or `exports.toml`.
    pub file: &'a str,
    pub command: &'a [String],
    pub stdin: &'a str,
    pub output: &'a str,
    /// How the timeout reads: "after 60s", or "after 60s without progress".
    pub stopped: String,
}

/// The heart of the question, for every program asked about: what it runs and with what.
pub fn program_facts(ui: &mut egui::Ui, facts: &ProgramFacts<'_>) {
    ui.heading("Run a program?");
    ui.label(format!(
        "\u{201c}{}\u{201d} runs this command from {}:",
        facts.name, facts.file
    ));
    ui.label(RichText::new(command_text(facts.command)).monospace());
    Grid::new("command_question_details")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("On stdin it gets");
            ui.label(facts.stdin);
            ui.end_row();
            ui.label("Its output is");
            ui.label(facts.output);
            ui.end_row();
            ui.label("It is stopped");
            ui.label(&facts.stopped);
            ui.end_row();
        });
    ui.weak("DbRover won't ask again until the command changes.");
}

impl Question {
    fn facts(&self) -> ProgramFacts<'_> {
        match &self.run {
            Pending::Action(request) => {
                let command = &request.command;
                ProgramFacts {
                    name: &self.name,
                    file: "actions.toml",
                    command: command.command.source(),
                    stdin: stdin_text(command.stdin),
                    output: output_text(command.output),
                    stopped: format!("after {}", timeout_text(command.timeout)),
                }
            }
            Pending::Copy(request) => {
                let program = &request.spec.program;
                ProgramFacts {
                    name: &self.name,
                    file: "exports.toml",
                    command: program.command.source(),
                    stdin: "the selected cells, as JSON Lines",
                    output: "copied to the clipboard",
                    stopped: format!("after {}", timeout_text(program.timeout)),
                }
            }
        }
    }

    /// `Some(true)` to run, `Some(false)` to cancel, `None` while unanswered.
    fn show(&self, ctx: &egui::Context) -> Option<bool> {
        let modal = widgets::dialog(ctx, Id::new("command_question")).show(ctx, |ui| {
            program_facts(ui, &self.facts());
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if form::primary_button(ui, "Run", true).clicked() {
                    Some(true)
                } else if ui.button("Cancel").clicked() {
                    Some(false)
                } else {
                    None
                }
            })
            .inner
        });
        if modal.should_close() {
            return Some(false);
        }
        modal.inner
    }
}

impl OutputWindow {
    /// Draws the window. `false` once the user closed it.
    fn show(&self, ctx: &egui::Context) -> bool {
        let mut open = true;
        match self {
            Self::Output {
                program,
                name,
                output,
            } => {
                Window::new(format!("Output of {name}"))
                    .id(Id::new(("program_output", program.0)))
                    .open(&mut open)
                    .default_size(WINDOW_SIZE)
                    .show(ctx, |ui| {
                        if output.truncated {
                            ui.label(CUT_SHORT);
                        }
                        if output.stdout.is_empty() {
                            ui.weak("The program wrote nothing.");
                        } else {
                            cell_viewer::text_view(ui, &output.stdout);
                        }
                    });
            }
            Self::Failure {
                program,
                name,
                failure,
            } => {
                Window::new(format!("{name} failed"))
                    .id(Id::new(("program_failure", program.0)))
                    .open(&mut open)
                    .default_size(WINDOW_SIZE)
                    .show(ctx, |ui| {
                        ui.label(failure.to_string());
                        if let ProgramFailure::Exited { stderr, .. } = failure
                            && !stderr.is_empty()
                        {
                            ui.strong("It wrote to stderr:");
                            cell_viewer::text_view(ui, stderr);
                        }
                    });
            }
        }
        open
    }
}
