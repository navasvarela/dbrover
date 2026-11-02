//! The Settings tab, opened by View › Settings… in the dock beside the consoles: a sidebar of
//! pages under a search field, and the chosen page, or everything the search finds. The
//! settings' pages save `settings.toml` at once; the MCP defaults go to `projects.toml` with
//! their own button, and the theme is egui's, kept with the window layout. The Keymap and
//! Custom actions pages list `keymap.toml` and `actions.toml`, which are edited by hand, and
//! each project has a page of its own, saved to `projects.toml` with its own button.

mod actions_page;
mod controls;
mod exports_page;
mod keymap_page;
mod project_page;
mod search;
mod setting;

use std::path::Path;

use dbrover_core::{DataSource, ProjectId};
use dbrover_engine::Event;
use dbrover_store::{McpOverrides, McpPermissions, Project, Settings};
use egui::{
    Button, CentralPanel, Color32, Frame, Margin, Panel, RichText, ScrollArea, TextEdit,
    ThemePreference, Ui, WidgetInfo, WidgetType, vec2,
};

use self::controls::Draft;
use self::project_page::{ProjectDrafts, ProjectView};
use self::search::Found;
use self::setting::{Category, Setting};
use crate::command_runs::CommandRuns;
use crate::custom_files::CustomFiles;
use crate::folder_switch::{FolderInUse, FolderRequest};
use crate::form;
use crate::icons;
use crate::keymap::Keymap;
use crate::mcp_settings::McpDefaultsEditor;
use crate::settings::AppSettings;
use crate::state::DataSources;
use crate::theme::Palette;
use crate::{fonts, widgets};

const SIDEBAR_WIDTH: f32 = 210.0;
/// The page's settings stop widening here: rows wider than this are hard to read across.
const PAGE_MAX_WIDTH: f32 = 680.0;
const PAGE_MARGIN: Margin = Margin::symmetric(24, 18);
const SIDEBAR_MARGIN: Margin = Margin::symmetric(10, 12);
const NAV_ITEM_HEIGHT: f32 = 26.0;
const PAGE_TITLE_SIZE: f32 = 18.0;
/// After a project's name in the sidebar while it has changes to save.
const UNSAVED_MARK: &str = "●";

/// What the user asked of the tab this frame, for the app to do.
pub enum SettingsAction {
    /// Write these settings to `settings.toml`.
    Save(Settings),
    /// Write these MCP defaults to `projects.toml`.
    SaveMcpDefaults(McpPermissions),
    /// Read `settings.toml` again, after editing it by hand.
    Reload,
    Theme(ThemePreference),
    /// Read `keymap.toml` again.
    ReloadKeymap,
    /// Write a starting `keymap.toml`, unless there is one.
    CreateKeymap,
    /// Read `actions.toml`, `snippets.toml`, `theme.toml` and `exports.toml` again.
    ReloadCustomFiles,
    /// Write a starting `actions.toml`, unless there is one.
    CreateActions,
    /// Write a starting `exports.toml`, unless there is one.
    CreateExports,
    /// Write a project's MCP settings to `projects.toml`.
    SaveProject {
        id: ProjectId,
        mcp_enabled: bool,
        overrides: McpOverrides,
    },
    /// Put the entry an MCP client needs for the project on the clipboard.
    CopyMcpConfiguration(ProjectId),
    /// Change the settings folder, from Files.
    SettingsFolder(FolderRequest),
}

/// What the tab needs of the app to draw itself.
pub struct SettingsView<'a> {
    pub settings: &'a AppSettings,
    pub mcp: &'a mut McpDefaultsEditor,
    pub keymap: &'a Keymap,
    /// Where `keymap.toml` is, once the engine has said.
    pub keymap_path: Option<&'a Path>,
    /// `actions.toml` and the other customisation files, as last read.
    pub custom_files: &'a CustomFiles,
    /// Which command actions run without asking.
    pub command_runs: &'a CommandRuns,
    /// Sorted by name, as the explorer lists them.
    pub projects: &'a [Project],
    pub data_sources: &'a DataSources,
    /// The MCP defaults as saved, which each project's settings start from.
    pub mcp_defaults: &'a McpPermissions,
    /// The settings folder in use; unknown in tests that don't give one.
    pub folder: Option<&'a FolderInUse>,
}

/// A page of the tab, listed in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Settings(Category),
    Keymap,
    Actions,
    Exports,
    /// A project's MCP settings, under the sidebar's Projects caption.
    Project(ProjectId),
}

impl Default for Page {
    fn default() -> Self {
        Self::Settings(Category::default())
    }
}

impl Page {
    /// The pages above the projects, in the sidebar's order.
    fn listed() -> impl Iterator<Item = Self> {
        Category::ALL.into_iter().map(Self::Settings).chain([
            Self::Keymap,
            Self::Actions,
            Self::Exports,
        ])
    }

    fn title(self) -> &'static str {
        match self {
            Self::Settings(category) => category.title(),
            Self::Keymap => "Keymap",
            Self::Actions => "Custom actions",
            Self::Exports => "Exports",
            // A project's page is titled with its name, which the page doesn't hold.
            Self::Project(_) => "Project",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Settings(category) => category.glyph(),
            Self::Keymap => icons::KEYBOARD,
            Self::Actions => icons::LIGHTNING,
            Self::Exports => icons::EXPORT,
            Self::Project(_) => icons::FOLDER_SIMPLE,
        }
    }

    /// The line under the page's title.
    fn summary(self) -> &'static str {
        match self {
            Self::Settings(category) => category.summary(),
            Self::Keymap => {
                "The keys for each action. Change them in keymap.toml, then click Reload."
            }
            Self::Actions => {
                "Your own actions from actions.toml, in the Actions menu, on right-click, and on \
                 the keys the keymap gives them."
            }
            Self::Exports => {
                "Your own export formats from exports.toml, in the export dialog and under \
                 Copy as."
            }
            Self::Project(_) => project_page::SUMMARY,
        }
    }
}

/// The tab's own state: the page shown, what is typed in the search field, and the projects'
/// unsaved changes. Not saved: the tab opens on Appearance.
#[derive(Debug, Default)]
pub struct SettingsTab {
    page: Page,
    query: String,
    projects: ProjectDrafts,
}

impl SettingsTab {
    /// Shows `page`, leaving any search, for a menu or link that opens the tab on it.
    pub fn show_page(&mut self, page: Page) {
        self.page = page;
        self.query.clear();
    }

    /// Takes in an engine event: a project's save, or why it failed.
    pub fn apply(&mut self, event: &Event) {
        self.projects.apply(event);
    }

    pub fn show(&mut self, ui: &mut Ui, view: SettingsView<'_>, actions: &mut Vec<SettingsAction>) {
        let found = self.found(ui.ctx(), &view);
        Panel::left("settings_sidebar")
            .resizable(false)
            .exact_size(SIDEBAR_WIDTH)
            .frame(Frame::new().inner_margin(SIDEBAR_MARGIN))
            .show(ui, |ui| self.sidebar(ui, &found, view.projects));
        CentralPanel::default().frame(Frame::new()).show(ui, |ui| {
            ScrollArea::vertical()
                .id_salt("settings_page")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Frame::new().inner_margin(PAGE_MARGIN).show(ui, |ui| {
                        ui.set_max_width(PAGE_MAX_WIDTH);
                        if self.searching() {
                            self.results(ui, &found, view, actions);
                        } else {
                            self.draw_page(ui, view, actions);
                        }
                    });
                });
        });
    }

    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// Everything the search finds, in the sidebar's order; nothing while not searching.
    fn found<'a>(&self, ctx: &egui::Context, view: &SettingsView<'a>) -> Vec<Found<'a>> {
        if !self.searching() {
            return Vec::new();
        }
        let shortcuts = keymap_page::shortcuts(
            |shortcut| ctx.format_shortcut(shortcut),
            view.keymap,
            view.custom_files.actions(),
            view.custom_files.exports(),
        );
        let actions = view.custom_files.actions().iter().map(Found::Action);
        let exports = view.custom_files.exports().iter().map(Found::Export);
        let projects = view.projects.iter().map(Found::Project);
        Setting::all()
            .into_iter()
            .map(Found::Setting)
            .chain(shortcuts.into_iter().map(Found::Shortcut))
            .chain(actions)
            .chain(exports)
            .chain(projects)
            .filter(|found| found.matches(&self.query))
            .collect()
    }

    fn sidebar(&mut self, ui: &mut Ui, found: &[Found<'_>], projects: &[Project]) {
        let field = ui.add(
            TextEdit::singleline(&mut self.query)
                .hint_text(widgets::icon_text(
                    ui,
                    icons::MAGNIFYING_GLASS,
                    Color32::PLACEHOLDER,
                    "Search settings",
                ))
                .desired_width(f32::INFINITY),
        );
        widgets::name(field, WidgetType::TextEdit, "Search settings", true);
        ui.add_space(8.0);
        // Many projects make the list taller than the window.
        ScrollArea::vertical()
            .id_salt("settings_sidebar")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for page in Page::listed() {
                    self.nav_item(ui, page, page.title(), found);
                }
                ui.add_space(4.0);
                ui.separator();
                form::section(ui, "Projects", |ui| {
                    if projects.is_empty() {
                        form::description_label(ui, "No projects yet");
                    }
                    for project in projects {
                        self.nav_item(ui, Page::Project(project.id), &project.name, found);
                    }
                });
            });
    }

    /// `page`'s entry in the sidebar, named `text`: chosen, with what the search finds on it, or
    /// with a dot while it has changes to save.
    fn nav_item(&mut self, ui: &mut Ui, page: Page, text: &str, found: &[Found<'_>]) {
        let count = found.iter().filter(|found| found.page() == page).count();
        let unsaved = match page {
            Page::Project(id) => self.projects.changed(id),
            Page::Settings(_) | Page::Keymap | Page::Actions | Page::Exports => false,
        };
        let badge = if self.searching() {
            Some(count.to_string())
        } else {
            unsaved.then(|| UNSAVED_MARK.to_owned())
        };
        let item = NavItem {
            selected: !self.searching() && self.page == page,
            dimmed: self.searching() && count == 0,
            badge,
            ..NavItem::new(page.glyph(), text)
        };
        if item.show(ui).clicked() {
            self.show_page(page);
        }
    }

    fn draw_page(
        &mut self,
        ui: &mut Ui,
        view: SettingsView<'_>,
        actions: &mut Vec<SettingsAction>,
    ) {
        if let Page::Project(id) = self.page {
            self.project_page(ui, id, &view, actions);
            return;
        }
        page_title(ui, self.page.title(), self.page.summary());
        match self.page {
            Page::Settings(category) => {
                let settings: Vec<Found<'_>> = Setting::all()
                    .into_iter()
                    .filter(|setting| setting.category() == category)
                    .map(Found::Setting)
                    .collect();
                // Settings' pages have no Open to click.
                let _ = settings_page(ui, &settings, view, actions, |found| {
                    found.section().to_owned()
                });
            }
            Page::Keymap => {
                let shortcuts = keymap_page::shortcuts(
                    |shortcut| ui.ctx().format_shortcut(shortcut),
                    view.keymap,
                    view.custom_files.actions(),
                    view.custom_files.exports(),
                );
                keymap_page::show(ui, view.keymap, view.keymap_path, &shortcuts, actions);
            }
            Page::Actions => actions_page::show(
                ui,
                view.custom_files,
                view.keymap,
                view.command_runs,
                actions,
            ),
            Page::Exports => exports_page::show(ui, view.custom_files, view.keymap, actions),
            Page::Project(_) => {}
        }
    }

    /// The project's page, or Appearance once the project is deleted.
    fn project_page(
        &mut self,
        ui: &mut Ui,
        id: ProjectId,
        view: &SettingsView<'_>,
        actions: &mut Vec<SettingsAction>,
    ) {
        let Some(project) = view.projects.iter().find(|project| project.id == id) else {
            self.page = Page::default();
            ui.ctx().request_repaint();
            return;
        };
        page_title(ui, &project.name, project_page::SUMMARY);
        let members: Vec<&DataSource> = view
            .data_sources
            .entries()
            .iter()
            .map(|entry| &entry.data_source)
            .filter(|data_source| data_source.project == Some(id))
            .collect();
        let project_view = ProjectView {
            project,
            defaults: view.mcp_defaults,
            members,
        };
        self.projects.show(ui, &project_view, actions);
    }

    /// What the search found, under its page's title and section.
    fn results(
        &mut self,
        ui: &mut Ui,
        found: &[Found<'_>],
        view: SettingsView<'_>,
        actions: &mut Vec<SettingsAction>,
    ) {
        let heading = match found.len() {
            0 => "Nothing matches".to_owned(),
            1 => "1 match".to_owned(),
            n => format!("{n} matches"),
        };
        page_title(ui, &heading, &format!("Matching “{}”", self.query.trim()));
        let opened = settings_page(ui, found, view, actions, |found| found.caption());
        if let Some(page) = opened {
            self.show_page(page);
        }
    }
}

/// `found` in cards, a card per run that shares a page and section, between the problems and
/// the path of `settings.toml`, which the settings among them are saved to. Returns the page
/// whose Open was clicked, for what is edited on its own page.
fn settings_page(
    ui: &mut Ui,
    found: &[Found<'_>],
    view: SettingsView<'_>,
    actions: &mut Vec<SettingsAction>,
    caption: impl Fn(&Found<'_>) -> String,
) -> Option<Page> {
    let palette = Palette::for_visuals(ui.visuals());
    file_status(ui, view.settings, &palette);
    let theme = ui.ctx().options(|options| options.theme_preference);
    let mut draft = Draft {
        settings: view.settings.settings().clone(),
        theme,
        mcp: view.mcp,
        mcp_to_save: None,
        folder: view.folder,
        folder_request: None,
    };
    let mut opened = None;
    let defaults = view.mcp_defaults;
    let groups = found.chunk_by(|a, b| caption(a) == caption(b));
    for group in groups {
        form::section(ui, &caption(&group[0]), |ui| {
            form::card(ui, |rows| {
                for found in group {
                    match found {
                        Found::Setting(setting) => controls::row(rows, *setting, &mut draft),
                        Found::Shortcut(shortcut) => keymap_page::row(rows, shortcut),
                        Found::Action(action) => {
                            if actions_page::found_row(rows, action) {
                                opened = Some(found.page());
                            }
                        }
                        Found::Export(format) => {
                            if exports_page::found_row(rows, format) {
                                opened = Some(found.page());
                            }
                        }
                        Found::Project(project) => {
                            if project_page::found_row(rows, project, defaults) {
                                opened = Some(found.page());
                            }
                        }
                    }
                }
            });
        });
    }
    let about = view
        .settings
        .path()
        .map(|path| format!("Saved in {}", path.display()));
    file_line(ui, about.as_deref(), |ui| {
        if ui
            .link("Reload")
            .on_hover_text("Read the file again, after editing it by hand")
            .clicked()
        {
            actions.push(SettingsAction::Reload);
        }
    });
    if draft.theme != theme {
        actions.push(SettingsAction::Theme(draft.theme));
    }
    if &draft.settings != view.settings.settings() {
        actions.push(SettingsAction::Save(draft.settings));
    }
    if let Some(defaults) = draft.mcp_to_save {
        actions.push(SettingsAction::SaveMcpDefaults(defaults));
    }
    if let Some(request) = draft.folder_request {
        actions.push(SettingsAction::SettingsFolder(request));
    }
    opened
}

fn page_title(ui: &mut Ui, title: &str, summary: &str) {
    ui.label(RichText::new(title).font(fonts::semibold(PAGE_TITLE_SIZE)));
    form::description_label(ui, summary);
    ui.add_space(14.0);
}

/// Why `settings.toml` couldn't be read, and what in it couldn't be used, above the settings.
fn file_status(ui: &mut Ui, settings: &AppSettings, palette: &Palette) {
    if let Some(error) = settings.error() {
        widgets::status(ui, icons::X_CIRCLE, palette.error, error);
    }
    for problem in settings.problems() {
        widgets::status(ui, icons::WARNING, palette.warning, &problem);
    }
    if settings.error().is_some() || !settings.problems().is_empty() {
        ui.add_space(8.0);
    }
}

/// What in a file the page lists was skipped, or why it couldn't be read, at the top of the
/// page.
fn problems(ui: &mut Ui, problems: &[String]) {
    let palette = Palette::for_visuals(ui.visuals());
    for problem in problems {
        widgets::status(ui, icons::WARNING, palette.warning, problem);
    }
    if !problems.is_empty() {
        ui.add_space(8.0);
    }
}

/// The page's file, said by `about`, and `add_links` to act on it, such as Reload.
fn file_line(ui: &mut Ui, about: Option<&str>, add_links: impl FnOnce(&mut Ui)) {
    ui.horizontal_wrapped(|ui| {
        if let Some(about) = about {
            form::description_label(ui, about);
        }
        add_links(ui);
    });
}

/// The file a page lists, by name with its full path on hover, and `add_links` to act on it.
/// The name alone keeps the line short; where the folder is, is in the manual's files chapter.
fn source_line(ui: &mut Ui, path: Option<&Path>, add_links: impl FnOnce(&mut Ui)) {
    ui.horizontal_wrapped(|ui| {
        if let Some(path) = path {
            let palette = Palette::for_visuals(ui.visuals());
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default();
            let text = RichText::new(format!("Read from {name}"))
                .small()
                .color(palette.text_weak);
            ui.label(text).on_hover_text(path.display().to_string());
        }
        add_links(ui);
    });
}

/// A sidebar entry: an icon and a name, highlighted when chosen, with a badge at the right: a
/// count while searching, or a mark of unsaved changes.
struct NavItem<'a> {
    glyph: &'a str,
    text: &'a str,
    selected: bool,
    /// Greys out a page the search finds nothing in.
    dimmed: bool,
    badge: Option<String>,
}

impl<'a> NavItem<'a> {
    fn new(glyph: &'a str, text: &'a str) -> Self {
        Self {
            glyph,
            text,
            selected: false,
            dimmed: false,
            badge: None,
        }
    }

    fn show(self, ui: &mut Ui) -> egui::Response {
        let palette = Palette::for_visuals(ui.visuals());
        let icon_color = match (self.selected, self.dimmed) {
            // The selected text's colour: the system's accent on a Mac.
            (true, _) => ui.visuals().selection.stroke.color,
            (false, true) => palette.border,
            (false, false) => palette.text_weak,
        };
        let job = widgets::icon_text(ui, self.glyph, icon_color, self.text);
        let item = Button::selectable(self.selected, job)
            .right_text(
                RichText::new(self.badge.unwrap_or_default())
                    .small()
                    .color(palette.text_weak),
            )
            .min_size(vec2(ui.available_width(), NAV_ITEM_HEIGHT));
        let response = ui.add(item);
        response.widget_info(|| {
            WidgetInfo::selected(WidgetType::Button, true, self.selected, self.text)
        });
        response
    }
}
