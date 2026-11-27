//! **Saved dumps…** on a data source (W17h): its saved dumps, each with its schedule, whether
//! the schedule runs on this machine, how its last run went, **Run now** and **Delete…**.

use dbrover_core::DataSourceId;
use dbrover_engine::{Command, Event};
use dbrover_store::{SavedDump, ScheduleState};
use egui::{Id, RichText, ScrollArea, Ui};

use crate::dialog::{self, DialogSize};
use crate::form;
use crate::icons;
use crate::theme::Palette;

pub enum SavedDumpsAction {
    Send(Box<Command>),
    Close,
}

pub struct SavedDumpsDialog {
    data_source: DataSourceId,
    name: String,
    dumps: Option<Vec<(SavedDump, Option<ScheduleState>)>>,
    /// The saved dump a **Delete…** asks about.
    deleting: Option<String>,
}

impl SavedDumpsDialog {
    /// The dialog for `data_source`, and the command that reads the saved dumps.
    pub fn open(data_source: DataSourceId, name: &str) -> (Self, Command) {
        let dialog = Self {
            data_source,
            name: name.to_owned(),
            dumps: None,
            deleting: None,
        };
        (dialog, Command::LoadSavedDumps)
    }

    pub fn apply(&mut self, event: &Event) {
        if let Event::SavedDumpsLoaded { dumps, states } = event {
            self.dumps = Some(
                dumps
                    .iter()
                    .filter(|dump| dump.data_source == self.data_source)
                    .map(|dump| {
                        let state = states.iter().find(|state| state.id == dump.id).cloned();
                        (dump.clone(), state)
                    })
                    .collect(),
            );
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<SavedDumpsAction> {
        let modal = dialog::modal(ctx, Id::new("saved_dumps"), DialogSize::Wide)
            .show(ctx, |ui| self.contents(ui));
        if modal.should_close() {
            return Some(SavedDumpsAction::Close);
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut Ui) -> Option<SavedDumpsAction> {
        let palette = Palette::for_visuals(ui.visuals());
        form::Header::new(
            icons::DOWNLOAD_SIMPLE,
            &format!("Saved dumps of {}", self.name),
        )
        .show(ui);
        let mut action = None;
        match &self.dumps {
            None => {
                ui.spinner();
            }
            Some(dumps) if dumps.is_empty() => {
                form::description_label(
                    ui,
                    "None yet. Dump… → Save… keeps a dump's settings under a name, to run again.",
                );
            }
            Some(dumps) => {
                ScrollArea::vertical()
                    .max_height(dialog::LIST_HEIGHT)
                    .show(ui, |ui| {
                        for (dump, state) in dumps {
                            if let Some(chosen) =
                                row(ui, dump, state.as_ref(), &palette, &mut self.deleting)
                            {
                                action = Some(chosen);
                            }
                            ui.separator();
                        }
                    });
            }
        }
        let close = form::footer(ui, |ui| form::primary_button(ui, "Close", true).clicked());
        if close {
            return Some(SavedDumpsAction::Close);
        }
        action
    }
}

/// One saved dump: its name, schedule, folder and last run, and what can be done with it.
fn row(
    ui: &mut Ui,
    dump: &SavedDump,
    state: Option<&ScheduleState>,
    palette: &Palette,
    deleting: &mut Option<String>,
) -> Option<SavedDumpsAction> {
    let mut action = None;
    ui.label(RichText::new(&dump.name).strong());
    let when = dump.schedule.map_or_else(
        || "By hand".to_owned(),
        |schedule| capitalised(&schedule.to_string()),
    );
    form::description_label(
        ui,
        &format!(
            "{when}, to {}, keeping {}. dbrover dump run {}",
            dump.folder.display(),
            match dump.keep {
                0 => "every file".to_owned(),
                keep => format!("the newest {keep}"),
            },
            dump.id
        ),
    );
    if let Some(outcome) = state.and_then(|state| {
        let at = state.last_run?;
        Some(format!(
            "Last ran {}: {}",
            at.strftime("%Y-%m-%d %H:%M UTC"),
            state.last_outcome.as_deref()?
        ))
    }) {
        let failed = state.is_some_and(|state| state.last_run != state.last_ok);
        let color = if failed {
            palette.error
        } else {
            palette.text_weak
        };
        ui.label(RichText::new(outcome).small().color(color));
    }
    ui.horizontal(|ui| {
        if dump.schedule.is_some() {
            let mut on = state.is_some_and(|state| state.on);
            let label = format!("Run {}'s schedule here", dump.name);
            if form::toggle(ui, &mut on, &label).changed() {
                action = Some(SavedDumpsAction::Send(Box::new(Command::SetDumpSchedule {
                    id: dump.id.clone(),
                    on,
                })));
            }
            ui.label("On here");
        }
        if ui.button("Run now").clicked() {
            let run = Command::RunSavedDump(dump.id.clone());
            action = Some(SavedDumpsAction::Send(Box::new(run)));
        }
        if deleting.as_deref() == Some(dump.id.as_str()) {
            ui.label("Delete it? Its files stay.");
            if form::danger_button(ui, "Delete", true).clicked() {
                *deleting = None;
                let delete = Command::DeleteSavedDump(dump.id.clone());
                action = Some(SavedDumpsAction::Send(Box::new(delete)));
            }
            if ui.button("Keep").clicked() {
                *deleting = None;
            }
        } else if ui.button("Delete…").clicked() {
            *deleting = Some(dump.id.clone());
        }
    });
    action
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
