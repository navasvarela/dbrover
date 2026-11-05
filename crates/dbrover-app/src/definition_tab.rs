//! The source of catalog objects (a view, function, index, …), each in a dock tab: read
//! beside a console, several at once, and opened in a console to change.
//!
//! The tabs aren't restored after a restart: loading a definition needs a live connection, and
//! reconnecting at start-up could ask for passwords before the user did anything. Their keys
//! are numbers handed out while the app runs, which mean nothing to the next run, so
//! [`crate::workspace::forget_transient_tabs`] drops them from a saved layout as it loads.

use std::collections::BTreeMap;

use dbrover_core::{CatalogPath, DataSourceId};
use dbrover_engine::Event;
use dbrover_language::QueryLanguage;
use egui::{ScrollArea, TextEdit, Ui};
use serde::{Deserialize, Serialize};

use crate::console::highlight::{self, Marks};
use crate::form;

/// A definition tab's key in the dock, unique while the app runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DefinitionId(u64);

/// Every open definition, by its tab's key.
#[derive(Default)]
pub struct DefinitionTabs {
    tabs: BTreeMap<DefinitionId, DefinitionTab>,
    next: u64,
}

/// What the user asked of a definition tab this frame.
pub enum DefinitionAction {
    /// Put the text into a new console of the definition's data source, to change it there.
    OpenInConsole {
        data_source: DataSourceId,
        text: String,
    },
}

impl DefinitionTabs {
    /// The tab showing `path`'s definition, if one is open.
    pub fn find(&self, data_source: DataSourceId, path: &CatalogPath) -> Option<DefinitionId> {
        self.tabs
            .iter()
            .find(|(_, tab)| tab.data_source == data_source && tab.path == *path)
            .map(|(id, _)| *id)
    }

    /// A tab for `path`'s definition, loading until the engine answers.
    pub fn open(&mut self, opened: OpenedDefinition) -> DefinitionId {
        let id = DefinitionId(self.next);
        self.next += 1;
        self.tabs.insert(
            id,
            DefinitionTab {
                data_source: opened.data_source,
                path: opened.path,
                title: opened.title,
                language: opened.language,
                content: Content::Loading,
            },
        );
        id
    }

    pub fn close(&mut self, id: DefinitionId) {
        self.tabs.remove(&id);
    }

    /// The tabs showing `data_source`'s objects, to close with it.
    pub fn of(&self, data_source: DataSourceId) -> Vec<DefinitionId> {
        self.tabs
            .iter()
            .filter(|(_, tab)| tab.data_source == data_source)
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn title(&self, id: DefinitionId) -> Option<&str> {
        self.tabs.get(&id).map(|tab| tab.title.as_str())
    }

    pub fn apply(&mut self, event: &Event) {
        if let Event::DefinitionLoaded { id, path, result } = event {
            for tab in self.tabs.values_mut() {
                if tab.data_source == *id && tab.path == *path {
                    tab.content = match result {
                        Ok(text) => Content::Loaded(text.clone()),
                        Err(error) => Content::Failed(error.clone()),
                    };
                }
            }
        }
    }

    pub fn show(&self, ui: &mut Ui, id: DefinitionId) -> Option<DefinitionAction> {
        self.tabs.get(&id)?.show(ui)
    }
}

/// What a new definition tab shows.
pub struct OpenedDefinition {
    pub data_source: DataSourceId,
    pub path: CatalogPath,
    /// The object's name, the tab's title.
    pub title: String,
    /// How to colour the text: the data source's query language.
    pub language: QueryLanguage,
}

struct DefinitionTab {
    data_source: DataSourceId,
    path: CatalogPath,
    title: String,
    language: QueryLanguage,
    content: Content,
}

enum Content {
    Loading,
    Loaded(String),
    Failed(String),
}

impl DefinitionTab {
    fn show(&self, ui: &mut Ui) -> Option<DefinitionAction> {
        match &self.content {
            Content::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading…");
                });
                None
            }
            Content::Failed(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
                None
            }
            Content::Loaded(text) => {
                let action = self.toolbar(ui, text);
                ui.separator();
                self.text(ui, text);
                action
            }
        }
    }

    /// **Open in console**, the tab's main action, and **Copy**.
    fn toolbar(&self, ui: &mut Ui, text: &str) -> Option<DefinitionAction> {
        ui.horizontal(|ui| {
            let open = form::primary_button(ui, "Open in console", true)
                .on_hover_text("Put it in a new console, to change and run it there");
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(text.to_owned());
            }
            open.clicked().then(|| DefinitionAction::OpenInConsole {
                data_source: self.data_source,
                text: text.to_owned(),
            })
        })
        .inner
    }

    /// The text, coloured as a console colours it, selectable but not editable.
    fn text(&self, ui: &mut Ui, text: &str) {
        let marks = Marks::default();
        let mut layouter = highlight::layouter(self.language, &marks);
        ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            // A `&str` buffer makes the editor read-only but still selectable.
            ui.add(
                TextEdit::multiline(&mut &*text)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .layouter(&mut layouter),
            );
        });
    }
}
