//! The dockable panel layout: which tabs exist, where they start, and what each one draws.

use std::collections::BTreeMap;
use std::path::Path;

use dbrover_core::SourceColor;
use dbrover_store::{ConsoleKey, CustomAction, DiagramKey, Project, Snippet};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Id, Ui, WidgetText};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockState, NodeIndex, TabStyle, TabViewer};
use serde::{Deserialize, Serialize};

use crate::command_runs::CommandRuns;
use crate::console::{
    self, ConsoleAction, ConsoleContext, LintOptions, QueryConsole, RecentCompletions, TreeCatalog,
};
use crate::consoles::SavedConsoles;
use crate::custom_actions::{SubjectKind, SubjectOn};
use crate::custom_files::CustomFiles;
use crate::diagram::{DiagramAction, Diagrams};
use crate::explorer::{self, ExplorerAction, ExplorerOptions};
use crate::fonts;
use crate::icons::{self, Icon, Role};
use crate::keymap::Keymap;
use crate::log_console::LogConsole;
use crate::mcp_settings::McpDefaultsEditor;
use crate::settings::AppSettings;
use crate::settings_tab::{SettingsAction, SettingsTab, SettingsView};
use crate::state::DataSources;
use crate::theme::{self, Palette};
use crate::welcome;

/// Tab titles are a little smaller than body text, like an IDE's.
const TAB_TITLE_SIZE: f32 = 12.5;
/// The band in a console's data source colour along the top of the console.
const SOURCE_BAND_HEIGHT: f32 = 2.0;

/// Share of the window's width for the explorer, a full-height column on the left. The rest
/// is the main area, where consoles run, with the log console underneath.
///
/// egui_dock's split `fraction` is the share of the first (left or top) node, whatever its
/// docs say about "the old node".
const EXPLORER_SHARE: f32 = 0.2;
/// Share of the main area's height for the consoles; the log console gets the rest.
const CONSOLES_SHARE_OF_MAIN_AREA: f32 = 0.75;

/// Every kind of tab the dock can hold.
///
/// Saved with the layout in eframe's storage. A saved layout that names a variant which no
/// longer exists fails to load, and the app falls back to [`default_layout`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    Explorer,
    Welcome,
    LogConsole,
    /// A console, by its file. Restored at start-up once the engine has read the console
    /// files; see [`restore_consoles`].
    QueryConsole(ConsoleKey),
    /// View › Settings…, beside the consoles.
    Settings,
    /// An ER diagram, by its file.
    Diagram(DiagramKey),
}

/// The consoles whose tabs the layout holds, so they can be reopened at start-up.
pub fn console_tabs(layout: &DockState<Tab>) -> Vec<ConsoleKey> {
    layout
        .iter_all_tabs()
        .filter_map(|(_, tab)| match tab {
            Tab::QueryConsole(key) => Some(key.clone()),
            _ => None,
        })
        .collect()
}

/// Drops the console tabs `reopen` can't reopen (their file or data source is gone), so a
/// restored layout never shows an empty tab.
pub fn restore_consoles(layout: &mut DockState<Tab>, mut reopen: impl FnMut(&ConsoleKey) -> bool) {
    for key in console_tabs(layout) {
        if !reopen(&key) {
            close_console_tab(layout, &key);
        }
    }
}

pub fn close_console_tab(layout: &mut DockState<Tab>, key: &ConsoleKey) {
    if let Some(path) = layout.find_tab(&Tab::QueryConsole(key.clone())) {
        layout.remove_tab(path);
    }
}

/// Adds a console tab next to the other consoles (or the Welcome tab), and makes it the
/// active tab and its area the focused one.
pub fn open_console_tab(layout: &mut DockState<Tab>, key: ConsoleKey) {
    open_in_main_area(layout, Tab::QueryConsole(key));
}

/// Makes the console's tab the active tab and its area the focused one, if the tab exists.
pub fn focus_console_tab(layout: &mut DockState<Tab>, key: &ConsoleKey) {
    focus_tab(layout, &Tab::QueryConsole(key.clone()));
}

/// Brings a diagram's tab to the front, opening it next to the consoles if it isn't open.
pub fn open_diagram_tab(layout: &mut DockState<Tab>, key: DiagramKey) {
    let tab = Tab::Diagram(key);
    if !focus_tab(layout, &tab) {
        open_in_main_area(layout, tab);
    }
}

pub fn close_diagram_tab(layout: &mut DockState<Tab>, key: &DiagramKey) {
    if let Some(path) = layout.find_tab(&Tab::Diagram(key.clone())) {
        layout.remove_tab(path);
    }
}

/// Drops the diagram tabs `reopen` can't reopen (their file or data source is gone), so a
/// restored layout never shows an empty tab.
pub fn restore_diagrams(layout: &mut DockState<Tab>, mut reopen: impl FnMut(&DiagramKey) -> bool) {
    let keys: Vec<DiagramKey> = layout
        .iter_all_tabs()
        .filter_map(|(_, tab)| match tab {
            Tab::Diagram(key) => Some(key.clone()),
            _ => None,
        })
        .collect();
    for key in keys {
        if !reopen(&key) {
            close_diagram_tab(layout, &key);
        }
    }
}

/// Brings the Settings tab to the front, opening it beside the consoles the first time, as an
/// editor opens its settings.
pub fn open_settings_tab(layout: &mut DockState<Tab>) {
    if !focus_tab(layout, &Tab::Settings) {
        open_in_main_area(layout, Tab::Settings);
    }
}

/// Adds `tab` next to the consoles (or the Welcome or Settings tab), and makes it the active
/// tab and its area the focused one.
fn open_in_main_area(layout: &mut DockState<Tab>, tab: Tab) {
    let tree = layout.main_surface_mut();
    let in_main_area = |tab: &Tab| {
        matches!(
            tab,
            Tab::QueryConsole(_) | Tab::Welcome | Tab::Settings | Tab::Diagram(_)
        )
    };
    match tree.find_tab_from(in_main_area) {
        Some((node, _)) => tree[node].append_tab(tab.clone()),
        // The consoles' area was closed; give them a new one beside everything else, taking
        // what the main area would.
        None => {
            tree.split_right(NodeIndex::root(), EXPLORER_SHARE, vec![tab.clone()]);
        }
    }
    focus_tab(layout, &tab);
}

/// Makes `tab` the active tab and its area the focused one. Returns whether the tab exists.
fn focus_tab(layout: &mut DockState<Tab>, tab: &Tab) -> bool {
    let Some(path) = layout.find_tab(tab) else {
        return false;
    };
    // The tab was just found, so activating it can't fail.
    let _ = layout.set_active_tab(path);
    // Focused too, so menus acting on "the current console" act on this one.
    layout.set_focused_node_and_surface(path.node_path());
    true
}

pub fn default_layout() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Welcome]);
    let tree = dock.main_surface_mut();
    let [main_area, _explorer] =
        tree.split_left(NodeIndex::root(), EXPLORER_SHARE, vec![Tab::Explorer]);
    tree.split_below(
        main_area,
        CONSOLES_SHARE_OF_MAIN_AREA,
        vec![Tab::LogConsole],
    );
    dock
}

/// Draws the content of each tab.
pub struct Panels<'a> {
    pub palette: &'a Palette,
    pub log_console: &'a mut LogConsole,
    pub data_sources: &'a DataSources,
    pub projects: &'a [Project],
    pub saved_consoles: &'a SavedConsoles,
    pub explorer_options: &'a mut ExplorerOptions,
    /// From `actions.toml`, for the explorer's and the grid's menus.
    pub custom_actions: &'a [CustomAction],
    /// From `snippets.toml`, for completion in the consoles.
    pub snippets: &'a [Snippet],
    /// What the user asked for in the explorer this frame, for the app to act on.
    pub explorer_actions: &'a mut Vec<ExplorerAction>,
    pub consoles: &'a mut BTreeMap<ConsoleKey, QueryConsole>,
    pub keymap: &'a Keymap,
    pub keymap_path: Option<&'a Path>,
    pub custom_files: &'a CustomFiles,
    pub command_runs: &'a CommandRuns,
    pub lint: &'a LintOptions,
    pub recent_completions: &'a RecentCompletions,
    /// What the user asked of each console this frame.
    pub console_actions: &'a mut Vec<(ConsoleKey, ConsoleAction)>,
    /// Consoles whose tabs the user asked to close this frame. The tabs stay until the app
    /// closes them, which may first ask about rolling back an open transaction.
    pub close_requests: &'a mut Vec<ConsoleKey>,
    pub settings_tab: &'a mut SettingsTab,
    pub settings: &'a AppSettings,
    pub mcp_defaults: &'a mut McpDefaultsEditor,
    /// What the user asked of the Settings tab this frame.
    pub settings_actions: &'a mut Vec<SettingsAction>,
    pub diagrams: &'a mut Diagrams,
    /// What the user asked of the diagram tabs this frame.
    pub diagram_actions: &'a mut Vec<DiagramAction>,
}

impl TabViewer for Panels<'_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> Id {
        Id::new(&*tab)
    }

    fn title(&mut self, tab: &mut Tab) -> WidgetText {
        let palette = self.palette;
        let tool_window = |icon: Icon, title: &str| {
            tab_title(palette, icon, title, fonts::semibold(TAB_TITLE_SIZE))
        };
        match tab {
            Tab::Explorer => tool_window(icons::node(dbrover_core::NodeKind::Database), "Explorer"),
            Tab::Welcome => tool_window(
                Icon {
                    glyph: icons::SPARKLE,
                    role: Role::Primary,
                },
                "Welcome",
            ),
            Tab::LogConsole => tool_window(
                Icon {
                    glyph: icons::LIST_DASHES,
                    role: Role::Weak,
                },
                "Log console",
            ),
            Tab::Settings => tool_window(
                Icon {
                    glyph: icons::GEAR,
                    role: Role::Primary,
                },
                "Settings",
            ),
            Tab::Diagram(key) => tab_title(
                palette,
                Icon {
                    glyph: icons::GRAPH,
                    role: Role::Primary,
                },
                key.name.as_str(),
                FontId::proportional(TAB_TITLE_SIZE),
            ),
            Tab::QueryConsole(key) => {
                let title = self
                    .consoles
                    .get(key)
                    .map_or_else(|| "console".to_owned(), QueryConsole::tab_title);
                let role = self
                    .console_color(key)
                    .map_or(Role::Secondary, Role::Source);
                let icon = Icon {
                    glyph: icons::FILE_SQL,
                    role,
                };
                tab_title(palette, icon, &title, FontId::proportional(TAB_TITLE_SIZE))
            }
        }
    }

    /// A console showing a `.sql` file names its path on hover.
    fn on_tab_button(&mut self, tab: &mut Tab, response: &egui::Response) {
        if let Tab::QueryConsole(key) = tab
            && let Some(path) = self.consoles.get(key).and_then(QueryConsole::file_path)
        {
            response.clone().on_hover_text(path.display().to_string());
        }
    }

    fn tab_style_override(&self, tab: &Tab, global_style: &TabStyle) -> Option<TabStyle> {
        let Tab::QueryConsole(key) = tab else {
            return None;
        };
        let color = self.console_color(key)?;
        Some(theme::source_tab_style(global_style, self.palette, color))
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        match tab {
            Tab::Explorer => explorer::show(
                ui,
                &explorer::ExplorerView {
                    data_sources: self.data_sources,
                    projects: self.projects,
                    saved_consoles: self.saved_consoles,
                    saved_diagrams: self.diagrams.saved(),
                    actions: self.custom_actions,
                },
                self.explorer_options,
                self.explorer_actions,
            ),
            Tab::Welcome => welcome::show(ui, self.data_sources, self.explorer_actions),
            Tab::LogConsole => self.log_console.ui(ui),
            Tab::Settings => self.settings_tab.show(
                ui,
                SettingsView {
                    settings: self.settings,
                    mcp: self.mcp_defaults,
                    keymap: self.keymap,
                    keymap_path: self.keymap_path,
                    custom_files: self.custom_files,
                    command_runs: self.command_runs,
                },
                self.settings_actions,
            ),
            Tab::Diagram(key) => self.diagram_actions.extend(self.diagrams.show(ui, key)),
            Tab::QueryConsole(key) => match self.consoles.get_mut(key) {
                Some(query_console) => {
                    let data_source = query_console.data_source();
                    if let Some(color) = self.data_sources.color_of(data_source) {
                        source_band(ui, self.palette.source(color));
                    }
                    let entry = self
                        .data_sources
                        .entries()
                        .iter()
                        .find(|entry| entry.data_source.id == data_source);
                    let driver = entry.map(|entry| entry.data_source.config.driver());
                    let snippets: Vec<&Snippet> = driver
                        .map(|driver| {
                            self.snippets
                                .iter()
                                .filter(|snippet| snippet.runs_on(driver))
                                .collect()
                        })
                        .unwrap_or_default();
                    let cell_actions: Vec<&CustomAction> = driver
                        .map(|driver| {
                            let on_cells = SubjectKind {
                                driver,
                                on: SubjectOn::Cells,
                            };
                            self.custom_actions
                                .iter()
                                .filter(|action| on_cells.offers(action))
                                .collect()
                        })
                        .unwrap_or_default();
                    let context = ConsoleContext {
                        keymap: self.keymap,
                        lint: self.lint,
                        recent: self.recent_completions,
                        catalog: entry.map(|entry| {
                            TreeCatalog::new(&entry.catalog, &entry.data_source.config)
                        }),
                        cell_actions: &cell_actions,
                        snippets: &snippets,
                    };
                    let mut actions = Vec::new();
                    console::show(ui, query_console, &context, &mut actions);
                    self.console_actions
                        .extend(actions.into_iter().map(|action| (key.clone(), action)));
                }
                // Restored from the saved layout, waiting for the engine to read its file.
                None => {
                    ui.weak("Loading…");
                }
            },
        }
    }

    /// The explorer and log console can't be reopened from a menu yet, so they can't be closed.
    fn is_closeable(&self, tab: &Tab) -> bool {
        matches!(
            tab,
            Tab::Welcome | Tab::QueryConsole(_) | Tab::Settings | Tab::Diagram(_)
        )
    }

    fn on_close(&mut self, tab: &mut Tab) -> OnCloseResponse {
        match tab {
            Tab::QueryConsole(key) => {
                self.close_requests.push(key.clone());
                OnCloseResponse::Ignore
            }
            Tab::Diagram(key) => {
                self.diagrams.close(key);
                OnCloseResponse::Close
            }
            _ => OnCloseResponse::Close,
        }
    }
}

impl Panels<'_> {
    /// The colour of the data source a console runs on, looked up every frame so a colour
    /// changed in the data source dialog shows at once.
    fn console_color(&self, key: &ConsoleKey) -> Option<SourceColor> {
        let data_source = self.consoles.get(key)?.data_source();
        self.data_sources.color_of(data_source)
    }
}

/// An icon in its palette colour, then the title in the tab's own text colour.
fn tab_title(palette: &Palette, icon: Icon, title: &str, font: FontId) -> WidgetText {
    let mut job = LayoutJob::default();
    job.append(
        icon.glyph,
        0.0,
        TextFormat::simple(fonts::icon(TAB_TITLE_SIZE + 1.0), icon.role.color(palette)),
    );
    // PLACEHOLDER takes the tab's text colour, which egui_dock varies with the tab's state.
    job.append(title, 6.0, TextFormat::simple(font, Color32::PLACEHOLDER));
    job.into()
}

/// A thin band in the data source's colour along the top of a console.
fn source_band(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), SOURCE_BAND_HEIGHT),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(rect, 1.0, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_is_a_column_beside_the_consoles_and_the_log_console() {
        let dock = default_layout();
        let tree = dock.main_surface();
        let tabs_at = |node: NodeIndex| tree[node].tabs().unwrap_or_default().to_vec();
        let main_area = NodeIndex::root().right();

        assert_eq!(tabs_at(NodeIndex::root().left()), [Tab::Explorer]);
        assert_eq!(tabs_at(main_area.left()), [Tab::Welcome]);
        assert_eq!(tabs_at(main_area.right()), [Tab::LogConsole]);
    }

    #[test]
    fn default_layout_has_every_tab_once() {
        let dock = default_layout();

        for tab in [Tab::Explorer, Tab::Welcome, Tab::LogConsole] {
            let count = dock.iter_all_tabs().filter(|(_, t)| **t == tab).count();
            assert_eq!(count, 1, "{tab:?}");
        }
    }
}
