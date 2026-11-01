//! Asks for the values of a run's `:name` parameters before it runs, starting from what the
//! console ran them with last.

use dbrover_core::Cell;
use dbrover_sql::ParameterValues;
use dbrover_store::ConsoleKey;
use egui::{Align, Id, Key, Layout, TextEdit, vec2};

use crate::console::SubmittedRun;
use crate::form;
use crate::widgets;

/// Room for a date and time, or a short sentence, in each value field.
const VALUE_WIDTH: f32 = 260.0;

pub enum ParametersChoice {
    Run,
    Cancel,
}

pub struct ParametersDialog {
    pub console: ConsoleKey,
    run: SubmittedRun,
    fields: Vec<Field>,
    /// Set until the first field has been given the keyboard focus.
    focus_first: bool,
}

/// One parameter's value as being typed.
struct Field {
    name: String,
    text: String,
    null: bool,
}

impl ParametersDialog {
    /// Asks for `names`, in order, starting from the values `remembered`.
    pub fn new(
        console: ConsoleKey,
        run: SubmittedRun,
        names: Vec<String>,
        remembered: &ParameterValues,
    ) -> Self {
        let fields = names
            .into_iter()
            .map(|name| {
                let (text, null) = match remembered.get(&name) {
                    Some(Some(text)) => (text.clone(), false),
                    Some(None) => (String::new(), true),
                    None => (String::new(), false),
                };
                Field { name, text, null }
            })
            .collect();
        Self {
            console,
            run,
            fields,
            focus_first: true,
        }
    }

    /// The values as typed: an empty field is an empty string, not `NULL`.
    pub fn values(&self) -> ParameterValues {
        self.fields
            .iter()
            .map(|field| {
                let value: Cell = (!field.null).then(|| field.text.clone());
                (field.name.clone(), value)
            })
            .collect()
    }

    /// The run, with the values typed.
    pub fn into_run(self) -> (ConsoleKey, SubmittedRun) {
        let values = self.values();
        let mut run = self.run;
        run.parameters = values;
        (self.console, run)
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<ParametersChoice> {
        let modal = widgets::dialog(ctx, Id::new("parameters_dialog")).show(ctx, |ui| {
            ui.heading("Parameter values");
            ui.label("Each value is sent as text, which the database converts as it needs.");
            ui.add_space(4.0);
            egui::Grid::new("parameter_fields")
                .num_columns(3)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    for (index, field) in self.fields.iter_mut().enumerate() {
                        let label = ui.label(format!(":{}", field.name));
                        // Sized explicitly: in a grid, a text field's desired width is
                        // squeezed to the column, which starts out a few characters wide.
                        let size = vec2(VALUE_WIDTH, ui.spacing().interact_size.y);
                        let edit = ui
                            .add_enabled_ui(!field.null, |ui| {
                                ui.add_sized(
                                    size,
                                    TextEdit::singleline(&mut field.text)
                                        .hint_text(if field.null { "NULL" } else { "" }),
                                )
                            })
                            .inner
                            .labelled_by(label.id);
                        if index == 0 && std::mem::take(&mut self.focus_first) {
                            edit.request_focus();
                        }
                        ui.checkbox(&mut field.null, "NULL");
                        ui.end_row();
                    }
                });
            ui.separator();
            let enter = ui.input(|input| input.key_pressed(Key::Enter));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if form::primary_button(ui, "Run", true).clicked() || enter {
                    Some(ParametersChoice::Run)
                } else if ui.button("Cancel").clicked() {
                    Some(ParametersChoice::Cancel)
                } else {
                    None
                }
            })
            .inner
        });
        if modal.should_close() {
            return Some(ParametersChoice::Cancel);
        }
        modal.inner
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::DataSourceId;
    use dbrover_engine::{RunOrigin, RunPurpose};
    use dbrover_store::ConsoleName;

    use super::*;

    #[test]
    fn starts_from_the_values_remembered_and_tells_null_from_empty() {
        let remembered: ParameterValues = [
            ("since", Some("2026-01-01".to_owned())),
            ("note", None),
            ("unused", Some("x".to_owned())),
        ]
        .into_iter()
        .collect();
        let key = ConsoleKey {
            data_source: DataSourceId::generate(),
            name: ConsoleName::numbered(1),
        };
        let run = SubmittedRun {
            statements: Vec::new(),
            purpose: RunPurpose::Run,
            parameters: ParameterValues::default(),
            origin: RunOrigin::Console,
            destination: crate::console::RunDestination::Grid,
            scans: dbrover_engine::ScanApproval::Ask,
        };
        let names = ["since", "note", "new"].map(str::to_owned).to_vec();

        let dialog = ParametersDialog::new(key.clone(), run, names, &remembered);

        let expected: ParameterValues = [
            ("since", Some("2026-01-01".to_owned())),
            ("note", None),
            ("new", Some(String::new())),
        ]
        .into_iter()
        .collect();
        assert_eq!(dialog.values(), expected);
        let (console, run) = dialog.into_run();
        assert_eq!(console, key);
        assert_eq!(run.parameters, expected);
    }
}
