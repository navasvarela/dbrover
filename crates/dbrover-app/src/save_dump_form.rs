//! **Save…** in the Dump dialog (W17h): the dump's settings under a name, with a schedule, kept
//! in `dumps.toml` to run again by hand, on its schedule while DbRover is open, or with
//! `dbrover dump run`.

use std::num::NonZeroU8;
use std::path::PathBuf;

use dbrover_core::{DataSourceId, DumpRequest, DumpScope};
use dbrover_store::{DEFAULT_KEEP, Day, SavedDump, Schedule, TimeOfDay};
use egui::Ui;

use crate::form::{self, Segment};
use crate::widgets;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    ByHand,
    Daily,
    Weekly,
    Hours,
}

pub(crate) struct SaveDumpForm {
    name: String,
    when: When,
    at: String,
    day: Day,
    hours: u8,
    keep: u32,
}

impl SaveDumpForm {
    pub(crate) fn new(database: &str) -> Self {
        Self {
            name: format!("{database} dump"),
            when: When::Daily,
            at: "02:00".to_owned(),
            day: Day::Monday,
            hours: 6,
            keep: DEFAULT_KEEP,
        }
    }

    /// The form's rows, in a card of their own.
    pub(crate) fn show(&mut self, ui: &mut Ui) {
        form::card(ui, |rows| {
            rows.row("Name", None, |ui| {
                widgets::name(
                    ui.text_edit_singleline(&mut self.name),
                    egui::WidgetType::TextEdit,
                    "Saved dump's name",
                    true,
                );
            });
            rows.row(
                "Runs",
                Some(
                    "A schedule runs while DbRover is open; `dbrover dump run` runs it otherwise.",
                ),
                |ui| {
                    form::segmented(
                        ui,
                        &mut self.when,
                        &[
                            Segment::new(When::ByHand, "By hand"),
                            Segment::new(When::Daily, "Daily"),
                            Segment::new(When::Weekly, "Weekly"),
                            Segment::new(When::Hours, "Every few hours"),
                        ],
                    );
                },
            );
            match self.when {
                When::ByHand => {}
                When::Daily => rows.row("At", None, |ui| at(ui, &mut self.at)),
                When::Weekly => {
                    rows.row("On", None, |ui| {
                        egui::ComboBox::from_id_salt("saved_dump_day")
                            .selected_text(self.day.to_string())
                            .show_ui(ui, |ui| {
                                for day in Day::ALL {
                                    ui.selectable_value(&mut self.day, day, day.to_string());
                                }
                            });
                    });
                    rows.row("At", None, |ui| at(ui, &mut self.at));
                }
                When::Hours => rows.row("Every", None, |ui| {
                    ui.label("hours");
                    ui.add(egui::DragValue::new(&mut self.hours).range(1..=24));
                }),
            }
            rows.row(
                "Keep",
                Some("Its newest files; older ones it wrote are removed after a run that works. 0 keeps every one."),
                |ui| {
                    ui.add(egui::DragValue::new(&mut self.keep).range(0..=365));
                },
            );
        });
    }

    /// What is wrong with the form, if anything, as a sentence.
    pub(crate) fn problem(&self) -> Option<&'static str> {
        if id_of(&self.name).is_empty() {
            return Some("Give it a name");
        }
        let timed = matches!(self.when, When::Daily | When::Weekly);
        (timed && TimeOfDay::try_from(self.at.trim().to_owned()).is_err())
            .then_some("Write the time as HH:MM")
    }

    /// The saved dump of `request`, from `data_source`.
    pub(crate) fn saved(
        &self,
        data_source: DataSourceId,
        request: &DumpRequest,
    ) -> Option<SavedDump> {
        let at = TimeOfDay::try_from(self.at.trim().to_owned()).ok();
        let schedule = match self.when {
            When::ByHand => None,
            When::Daily => Some(Schedule::Day { at: at? }),
            When::Weekly => Some(Schedule::Week {
                on: self.day,
                at: at?,
            }),
            When::Hours => Some(Schedule::Hours {
                hours: NonZeroU8::new(self.hours)?,
            }),
        };
        let (schemas, tables) = match &request.scope {
            DumpScope::Database => (Vec::new(), Vec::new()),
            DumpScope::Schemas(schemas) => (schemas.clone(), Vec::new()),
            DumpScope::Tables(tables) => (
                Vec::new(),
                tables
                    .iter()
                    .map(|table| format!("{}.{}", table.schema, table.name))
                    .collect(),
            ),
        };
        Some(SavedDump {
            id: id_of(&self.name),
            name: self.name.trim().to_owned(),
            data_source,
            database: request.database.clone(),
            schemas,
            tables,
            content: request.options.content,
            format: request.format,
            leave_out_owners: request.options.leave_out_owners,
            leave_out_privileges: request.options.leave_out_privileges,
            folder: request
                .file
                .parent()
                .map_or_else(PathBuf::new, PathBuf::from),
            keep: self.keep,
            schedule,
        })
    }
}

fn at(ui: &mut Ui, text: &mut String) {
    widgets::name(
        ui.add(egui::TextEdit::singleline(text).desired_width(60.0)),
        egui::WidgetType::TextEdit,
        "Time of day",
        true,
    );
}

/// `Nightly shop` → `nightly-shop`: the name as `dbrover dump run` takes it.
fn id_of(name: &str) -> String {
    let mut id = String::new();
    for c in name.trim().chars() {
        if c.is_alphanumeric() {
            id.extend(c.to_lowercase());
        } else if !id.ends_with('-') && !id.is_empty() {
            id.push('-');
        }
    }
    id.trim_end_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_a_saved_dump_as_the_command_line_takes_it() {
        assert_eq!(id_of("  Nightly shop (EU) "), "nightly-shop-eu");
        assert_eq!(id_of("!!"), "");
        let mut form = SaveDumpForm::new("shop");
        assert_eq!(form.problem(), None);
        form.at = "25:00".to_owned();
        assert_eq!(form.problem(), Some("Write the time as HH:MM"));
    }
}
