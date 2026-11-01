//! ER diagrams (ER1 in `docs/diagrams.md`): the tabs that show a data source's tables as boxes
//! and their foreign keys as lines, the dialog that picks a schema's tables for one, and the
//! files they are kept in.

mod dialog;
mod geometry;
mod model;
mod view;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use dbrover_core::DataSourceId;
use dbrover_engine::{Command, Event, SessionState};
use dbrover_store::{DiagramFile, DiagramFolder, DiagramKey, DiagramName, SavedDiagram};
use egui::{Rect, Ui};

pub use dialog::{DiagramDialog, DiagramDialogAction};
#[cfg(test)]
pub use model::TableState;
pub use model::{Diagram, Growth};

/// How long a diagram must stay as it is before it is saved, so dragging a table writes its
/// file once, when the drag is over, as a console is saved once typing pauses.
const SAVE_DELAY: Duration = Duration::from_secs(1);

/// What a diagram's tab asks of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagramAction {
    /// **Delete diagram…**, confirmed: remove its file and close its tab.
    Delete(DiagramKey),
}

/// What a diagram's tab asks of the collection, which knows its key.
enum TabAction {
    Delete,
}

/// A diagram's tab: the diagram, the part of it the tab shows, and how its file stands.
pub struct DiagramTab {
    diagram: Diagram,
    /// What the `Scene` shows, in the diagram's coordinates; `Rect::ZERO` until the diagram
    /// is first shown whole.
    scene_rect: Rect,
    /// Show the whole diagram on the next frame.
    fit: bool,
    /// What the diagram's file holds, as last saved or read.
    saved: Option<DiagramFile>,
    /// How the diagram was when it was last seen to change, and when.
    changed: Option<(DiagramFile, Instant)>,
    /// Why the last save failed, until one succeeds.
    save_error: Option<String>,
    confirming_delete: bool,
    /// From the diagram's coordinates to the screen's, as last drawn. Widgets in a `Scene`
    /// report their bounds to accessibility in the scene's own coordinates, so tests need it
    /// to point at a table.
    #[cfg(test)]
    to_screen: egui::emath::TSTransform,
}

/// The diagram files the engine read at start-up, and those saved since.
#[derive(Debug, Default)]
pub struct SavedDiagrams(BTreeMap<DiagramKey, DiagramFile>);

impl SavedDiagrams {
    /// The saved diagrams of the data source, by key.
    pub fn of(&self, data_source: DataSourceId) -> impl Iterator<Item = &DiagramKey> {
        self.0
            .iter()
            .filter(move |(_, file)| file.data_source == data_source)
            .map(|(key, _)| key)
    }
}

/// The diagrams with an open tab, and the saved ones.
#[derive(Default)]
pub struct Diagrams {
    open: BTreeMap<DiagramKey, DiagramTab>,
    saved: SavedDiagrams,
    /// Saves of diagrams whose tab closed before their last change was saved.
    closing_saves: Vec<Command>,
}

impl Diagrams {
    /// Starts showing a new `diagram` under `key`, and returns its first batch of tables to
    /// describe. It is saved once it has stayed as it is for a moment.
    pub fn open(&mut self, key: DiagramKey, diagram: Diagram) -> Option<Command> {
        self.show_diagram(key, diagram, None)
    }

    /// Opens the saved diagram `key` again, and returns its first batch of tables to describe;
    /// `None` if there is no such diagram, or it is open already.
    pub fn reopen(&mut self, key: &DiagramKey) -> Option<Command> {
        if self.open.contains_key(key) {
            return None;
        }
        let file = self.saved.0.get(key)?.clone();
        let diagram = Diagram::from_file(&file);
        self.show_diagram(key.clone(), diagram, Some(file))
    }

    pub fn is_open(&self, key: &DiagramKey) -> bool {
        self.open.contains_key(key)
    }

    /// The data source the saved diagram `key` draws.
    pub fn data_source_of(&self, key: &DiagramKey) -> Option<DataSourceId> {
        self.saved.0.get(key).map(|file| file.data_source)
    }

    fn show_diagram(
        &mut self,
        key: DiagramKey,
        mut diagram: Diagram,
        saved: Option<DiagramFile>,
    ) -> Option<Command> {
        let first = diagram.next_batch();
        let tab = DiagramTab {
            diagram,
            scene_rect: Rect::ZERO,
            fit: true,
            saved,
            changed: None,
            save_error: None,
            confirming_delete: false,
            #[cfg(test)]
            to_screen: egui::emath::TSTransform::IDENTITY,
        };
        self.open.insert(key, tab);
        first
    }

    /// Closes the diagram's tab, keeping its file, saved with the last change if it waited to
    /// be.
    pub fn close(&mut self, key: &DiagramKey) {
        let Some(tab) = self.open.remove(key) else {
            return;
        };
        let current = tab.diagram.to_file();
        if tab.saved.as_ref() != Some(&current) {
            let closing = save(key, &current, &mut self.saved);
            self.closing_saves.push(closing);
        }
    }

    /// Forgets a deleted diagram, open or saved.
    pub fn forget(&mut self, key: &DiagramKey) {
        self.open.remove(key);
        self.saved.0.remove(key);
    }

    pub fn saved(&self) -> &SavedDiagrams {
        &self.saved
    }

    /// `name`, or the first of `name_2`, `name_3`… that no diagram in `folder` has, open or
    /// saved.
    pub fn unused_name(&self, folder: &DiagramFolder, name: &DiagramName) -> DiagramName {
        let taken = |name: &DiagramName| {
            let key = DiagramKey {
                folder: folder.clone(),
                name: name.clone(),
            };
            self.open.contains_key(&key) || self.saved.0.contains_key(&key)
        };
        if !taken(name) {
            return name.clone();
        }
        (2..)
            .map(|number| name.numbered(number))
            .find(|numbered| !taken(numbered))
            .unwrap_or_else(|| name.clone())
    }

    /// Takes in what the engine read or described, and returns the next batches of tables
    /// the diagrams ask for.
    pub fn apply(&mut self, event: &Event) -> Vec<Command> {
        match event {
            Event::DiagramsLoaded(saved) => {
                let files = saved
                    .iter()
                    .map(|SavedDiagram { key, file }| (key.clone(), file.clone()));
                self.saved.0.extend(files);
                Vec::new()
            }
            Event::DiagramSaveFailed { key, error } => {
                if let Some(tab) = self.open.get_mut(key) {
                    tab.save_error = Some(error.clone());
                }
                Vec::new()
            }
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

    /// Saves each diagram that has stayed changed for [`SAVE_DELAY`], and says how long until
    /// the next one is due, if one is waiting.
    pub fn due_saves(&mut self, now: Instant) -> (Vec<Command>, Option<Duration>) {
        let mut saves = std::mem::take(&mut self.closing_saves);
        let mut next_due: Option<Duration> = None;
        let mut wait_for = |wait: Duration| {
            next_due = Some(next_due.map_or(wait, |next: Duration| next.min(wait)));
        };
        for (key, tab) in &mut self.open {
            let current = tab.diagram.to_file();
            if tab.saved.as_ref() == Some(&current) {
                tab.changed = None;
                continue;
            }
            let since = match &tab.changed {
                Some((seen, since)) if *seen == current => *since,
                _ => {
                    tab.changed = Some((current, now));
                    wait_for(SAVE_DELAY);
                    continue;
                }
            };
            let waited = now.duration_since(since);
            if waited < SAVE_DELAY {
                wait_for(SAVE_DELAY - waited);
                continue;
            }
            saves.push(save(key, &current, &mut self.saved));
            tab.saved = Some(current);
            tab.changed = None;
            tab.save_error = None;
        }
        (saves, next_due)
    }

    /// Saves every open diagram that changed since it was last saved, at once: on quitting.
    pub fn unsaved(&mut self) -> Vec<Command> {
        let mut saves = std::mem::take(&mut self.closing_saves);
        for (key, tab) in &mut self.open {
            let current = tab.diagram.to_file();
            if tab.saved.as_ref() != Some(&current) {
                saves.push(save(key, &current, &mut self.saved));
                tab.saved = Some(current);
            }
        }
        saves
    }

    pub fn show(&mut self, ui: &mut Ui, key: &DiagramKey) -> Option<DiagramAction> {
        let Some(tab) = self.open.get_mut(key) else {
            // Restored from the saved layout, waiting for the engine to read its file.
            ui.weak("Loading…");
            return None;
        };
        match view::show(ui, tab, key)? {
            TabAction::Delete => Some(DiagramAction::Delete(key.clone())),
        }
    }

    /// The open diagrams, for tests that look at what they hold.
    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = (&DiagramKey, &Diagram)> {
        self.open.iter().map(|(key, tab)| (key, &tab.diagram))
    }

    /// Where the table's box was last drawn on screen, for tests that drag it.
    #[cfg(test)]
    pub fn screen_rect(&self, key: &DiagramKey, table: &str) -> Option<Rect> {
        let tab = self.open.get(key)?;
        let index = tab
            .diagram
            .tables()
            .iter()
            .position(|t| t.name.name == table)?;
        Some(tab.to_screen * tab.diagram.rect(index)?)
    }

    fn of_data_source(&mut self, id: DataSourceId) -> impl Iterator<Item = &mut Diagram> {
        self.open
            .values_mut()
            .map(|tab| &mut tab.diagram)
            .filter(move |diagram| diagram.data_source == id)
    }
}

/// The command that writes `file` as the diagram `key`, which is listed as saved from then on.
fn save(key: &DiagramKey, file: &DiagramFile, saved: &mut SavedDiagrams) -> Command {
    saved.0.insert(key.clone(), file.clone());
    Command::SaveDiagram {
        key: key.clone(),
        file: file.clone(),
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::TableName;
    use dbrover_store::PlacedTable;

    use super::*;

    fn key(name: &str) -> DiagramKey {
        DiagramKey {
            folder: DiagramFolder::NoProject,
            name: DiagramName::try_from(name.to_owned()).unwrap(),
        }
    }

    fn table(name: &str) -> TableName {
        TableName {
            schema: "main".to_owned(),
            name: name.to_owned(),
        }
    }

    fn loaded(diagrams: &mut Diagrams, name: &str, file: DiagramFile) {
        diagrams.apply(&Event::DiagramsLoaded(vec![SavedDiagram {
            key: key(name),
            file,
        }]));
    }

    #[test]
    fn saves_a_diagram_once_it_has_stayed_the_same_for_a_moment() {
        let mut diagrams = Diagrams::default();
        let start = Instant::now();
        let data_source = DataSourceId::generate();
        let diagram = Diagram::new(data_source, None, vec![table("orders")], None);
        diagrams.open(key("shop"), diagram);

        let (saves, next) = diagrams.due_saves(start);
        assert!(saves.is_empty());
        assert_eq!(next, Some(SAVE_DELAY));

        let (saves, _) = diagrams.due_saves(start + SAVE_DELAY);
        assert!(
            matches!(saves.as_slice(), [Command::SaveDiagram { key: saved, .. }] if *saved == key("shop"))
        );
        assert_eq!(diagrams.saved().of(data_source).count(), 1);

        let (saves, next) = diagrams.due_saves(start + SAVE_DELAY * 3);
        assert!(saves.is_empty() && next.is_none(), "nothing changed since");
        assert!(diagrams.unsaved().is_empty());
    }

    #[test]
    fn a_tab_closed_before_its_change_was_saved_saves_it_on_closing() {
        let mut diagrams = Diagrams::default();
        let data_source = DataSourceId::generate();
        diagrams.open(
            key("shop"),
            Diagram::new(data_source, None, vec![table("orders")], None),
        );

        diagrams.close(&key("shop"));
        let (saves, _) = diagrams.due_saves(Instant::now());

        assert!(matches!(saves.as_slice(), [Command::SaveDiagram { .. }]));
        assert_eq!(
            diagrams.saved().of(data_source).count(),
            1,
            "listed for reopening"
        );
    }

    #[test]
    fn names_a_new_diagram_apart_from_open_and_saved_ones() {
        let mut diagrams = Diagrams::default();
        let data_source = DataSourceId::generate();
        let file = DiagramFile {
            data_source,
            schema: None,
            tables: vec![],
        };
        loaded(&mut diagrams, "main", file);
        diagrams.open(key("main_2"), Diagram::new(data_source, None, vec![], None));

        let name = diagrams.unused_name(&DiagramFolder::NoProject, &key("main").name);

        assert_eq!(name.as_str(), "main_3");
    }

    #[test]
    fn reopens_a_saved_diagram_once() {
        let mut diagrams = Diagrams::default();
        let file = DiagramFile {
            data_source: DataSourceId::generate(),
            schema: Some("main".to_owned()),
            tables: vec![PlacedTable {
                schema: "main".to_owned(),
                name: "orders".to_owned(),
                x: Some(10),
                y: Some(20),
            }],
        };
        loaded(&mut diagrams, "shop", file);

        let first = diagrams.reopen(&key("shop"));

        assert!(matches!(first, Some(Command::DescribeTables { .. })));
        assert!(diagrams.reopen(&key("shop")).is_none(), "already open");
        assert!(diagrams.reopen(&key("nothing")).is_none());
        assert!(diagrams.unsaved().is_empty(), "as it was read");
    }
}
