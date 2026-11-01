//! ER diagrams (ER1 in `docs/diagrams.md`): the tabs that show a data source's tables as boxes
//! and their foreign keys as lines, and the dialog that picks a schema's tables for one.

mod dialog;
mod geometry;
mod model;
mod view;

use std::collections::BTreeMap;

use dbrover_core::DataSourceId;
use dbrover_engine::{Command, Event, SessionState};
use dbrover_store::{DiagramFolder, DiagramKey, DiagramName};
use egui::{Rect, Ui};

pub use dialog::{DiagramDialog, DiagramDialogAction};
#[cfg(test)]
pub use model::TableState;
pub use model::{Diagram, Growth};

/// A diagram's tab: the diagram, and the part of it the tab shows.
pub struct DiagramTab {
    diagram: Diagram,
    /// What the `Scene` shows, in the diagram's coordinates; `Rect::ZERO` lets it fit the
    /// diagram on its first frame.
    scene_rect: Rect,
    /// Show the whole diagram on the next frame.
    fit: bool,
}

/// The diagrams with an open tab.
#[derive(Default)]
pub struct Diagrams {
    open: BTreeMap<DiagramKey, DiagramTab>,
}

impl Diagrams {
    /// Starts showing `diagram` under `key`, and returns its first batch of tables to describe.
    pub fn open(&mut self, key: DiagramKey, mut diagram: Diagram) -> Option<Command> {
        let first = diagram.next_batch();
        let tab = DiagramTab {
            diagram,
            scene_rect: Rect::ZERO,
            fit: false,
        };
        self.open.insert(key, tab);
        first
    }

    pub fn close(&mut self, key: &DiagramKey) {
        self.open.remove(key);
    }

    /// `name`, or the first of `name_2`, `name_3`… that no open diagram in `folder` has.
    pub fn unused_name(&self, folder: &DiagramFolder, name: &DiagramName) -> DiagramName {
        let taken = |name: &DiagramName| {
            self.open.contains_key(&DiagramKey {
                folder: folder.clone(),
                name: name.clone(),
            })
        };
        if !taken(name) {
            return name.clone();
        }
        (2..)
            .map(|number| name.numbered(number))
            .find(|numbered| !taken(numbered))
            .unwrap_or_else(|| name.clone())
    }

    /// Hands the engine's descriptions to the diagrams waiting for them, and returns the next
    /// batches they ask for.
    pub fn apply(&mut self, event: &Event) -> Vec<Command> {
        match event {
            Event::TablesDescribed {
                data_source,
                tables,
            } => self
                .of_data_source(*data_source)
                .filter_map(|diagram| {
                    diagram.described(tables);
                    diagram.next_batch()
                })
                .collect(),
            Event::SessionChanged { id, state } => {
                let reason = match state {
                    SessionState::Disconnected => "disconnected",
                    SessionState::Failed(error) => error.as_str(),
                    _ => return Vec::new(),
                };
                for diagram in self.of_data_source(*id) {
                    diagram.stop_loading(reason);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    pub fn show(&mut self, ui: &mut Ui, key: &DiagramKey) {
        match self.open.get_mut(key) {
            Some(tab) => view::show(ui, tab),
            None => {
                ui.weak("This diagram is closed.");
            }
        }
    }

    /// The open diagrams, for tests that look at what they hold.
    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = (&DiagramKey, &Diagram)> {
        self.open.iter().map(|(key, tab)| (key, &tab.diagram))
    }

    fn of_data_source(&mut self, id: DataSourceId) -> impl Iterator<Item = &mut Diagram> {
        self.open
            .values_mut()
            .map(|tab| &mut tab.diagram)
            .filter(move |diagram| diagram.data_source == id)
    }
}
