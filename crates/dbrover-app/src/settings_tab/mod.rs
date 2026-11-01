//! The Settings tab, opened by View › Settings… in the dock beside the consoles: a sidebar of
//! pages under a search field, and the chosen page, or everything the search finds. The
//! settings' pages save `settings.toml` at once; the MCP defaults go to `projects.toml` with
//! their own button, and the theme is egui's, kept with the window layout. The Keymap page
//! lists `keymap.toml`, which is edited by hand.

mod controls;
mod keymap_page;
mod search;
mod setting;

use std::path::Path;

use dbrover_store::{CustomAction, McpPermissions, Settings};
use egui::{
    Button, CentralPanel, Color32, Frame, Margin, Panel, RichText, ScrollArea, TextEdit,
    ThemePreference, Ui, WidgetInfo, WidgetType, vec2,
};

use self::controls::Draft;
use self::search::Found;
use self::setting::{Category, Setting};
use crate::commands::AppCommand;
use crate::form;
use crate::icons;
use crate::keymap::Keymap;
use crate::mcp_settings::McpDefaultsEditor;
use crate::settings::AppSettings;
use crate::theme::Palette;
use crate::{fonts, widgets};

const SIDEBAR_WIDTH: f32 = 210.0;
/// The page's settings stop widening here: rows wider than this are hard to read across.
const PAGE_MAX_WIDTH: f32 = 680.0;
const PAGE_MARGIN: Margin = Margin::symmetric(24, 18);
const SIDEBAR_MARGIN: Margin = Margin::symmetric(10, 12);
const NAV_ITEM_HEIGHT: f32 = 26.0;
const PAGE_TITLE_SIZE: f32 = 18.0;

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
    /// Open the custom actions, which have a window of their own.
    Run(AppCommand),
}

/// What the tab needs of the app to draw itself.
pub struct SettingsView<'a> {
    pub settings: &'a AppSettings,
    pub mcp: &'a mut McpDefaultsEditor,
    pub keymap: &'a Keymap,
    /// Where `keymap.toml` is, once the engine has said.
    pub keymap_path: Option<&'a Path>,
    /// `actions.toml`'s actions, which the keymap can bind too.
    pub custom_actions: &'a [CustomAction],
}

/// A page of the tab, listed in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Settings(Category),
    Keymap,
}

impl Default for Page {
    fn default() -> Self {
        Self::Settings(Category::default())
    }
}

impl Page {
    /// The pages in the sidebar's order.
    fn listed() -> impl Iterator<Item = Self> {
        Category::ALL
            .into_iter()
            .map(Self::Settings)
            .chain([Self::Keymap])
    }

    fn title(self) -> &'static str {
        match self {
            Self::Settings(category) => category.title(),
            Self::Keymap => "Keymap",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Settings(category) => category.glyph(),
            Self::Keymap => icons::KEYBOARD,
        }
    }

    /// The line under the page's title.
    fn summary(self) -> &'static str {
        match self {
            Self::Settings(category) => category.summary(),
            Self::Keymap => {
                "The keys for each action. Change them in keymap.toml, then click Reload."
            }
        }
    }
}

/// The tab's own state: the page shown and what is typed in the search field. Not saved: the
/// tab opens on Appearance.
#[derive(Debug, Default)]
pub struct SettingsTab {
    page: Page,
    query: String,
}

impl SettingsTab {
    /// Shows `page`, leaving any search, for a menu or link that opens the tab on it.
    pub fn show_page(&mut self, page: Page) {
        self.page = page;
        self.query.clear();
    }

    pub fn show(&mut self, ui: &mut Ui, view: SettingsView<'_>, actions: &mut Vec<SettingsAction>) {
        let found = self.found(ui.ctx(), &view);
        Panel::left("settings_sidebar")
            .resizable(false)
            .exact_size(SIDEBAR_WIDTH)
            .frame(Frame::new().inner_margin(SIDEBAR_MARGIN))
            .show(ui, |ui| self.sidebar(ui, &found, actions));
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
            view.custom_actions,
        );
        Setting::all()
            .into_iter()
            .map(Found::Setting)
            .chain(shortcuts.into_iter().map(Found::Shortcut))
            .filter(|found| found.matches(&self.query))
            .collect()
    }

    fn sidebar(&mut self, ui: &mut Ui, found: &[Found<'_>], actions: &mut Vec<SettingsAction>) {
        // The field shares the panel's fill in the light theme; an outline shows where it is.
        let border = Palette::for_visuals(ui.visuals()).border;
        ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border);
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
        for page in Page::listed() {
            let count = found.iter().filter(|found| found.page() == page).count();
            let item = NavItem {
                selected: !self.searching() && self.page == page,
                dimmed: self.searching() && count == 0,
                count: self.searching().then_some(count),
                ..NavItem::new(page.glyph(), page.title())
            };
            if item.show(ui).clicked() {
                self.show_page(page);
            }
        }
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        if NavItem::new(icons::LIGHTNING, "Custom actions…")
            .show(ui)
            .clicked()
        {
            actions.push(SettingsAction::Run(AppCommand::ShowActions));
        }
    }

    fn draw_page(&self, ui: &mut Ui, view: SettingsView<'_>, actions: &mut Vec<SettingsAction>) {
        page_title(ui, self.page.title(), self.page.summary());
        match self.page {
            Page::Settings(category) => {
                let settings: Vec<Found<'_>> = Setting::all()
                    .into_iter()
                    .filter(|setting| setting.category() == category)
                    .map(Found::Setting)
                    .collect();
                settings_page(ui, &settings, view, actions, |found| {
                    found.section().to_owned()
                });
            }
            Page::Keymap => {
                let shortcuts = keymap_page::shortcuts(
                    |shortcut| ui.ctx().format_shortcut(shortcut),
                    view.keymap,
                    view.custom_actions,
                );
                keymap_page::show(ui, view.keymap, view.keymap_path, &shortcuts, actions);
            }
        }
    }

    /// What the search found, under its page's title and section.
    fn results(
        &self,
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
        settings_page(ui, found, view, actions, |found| {
            format!("{} › {}", found.page().title(), found.section())
        });
    }
}

/// `found` in cards, a card per run that shares a page and section, between the problems and
/// the path of `settings.toml`, which the settings among them are saved to.
fn settings_page(
    ui: &mut Ui,
    found: &[Found<'_>],
    view: SettingsView<'_>,
    actions: &mut Vec<SettingsAction>,
    caption: impl Fn(&Found<'_>) -> String,
) {
    let palette = Palette::for_visuals(ui.visuals());
    file_status(ui, view.settings, &palette);
    let theme = ui.ctx().options(|options| options.theme_preference);
    let mut draft = Draft {
        settings: view.settings.settings().clone(),
        theme,
        mcp: view.mcp,
        mcp_to_save: None,
    };
    let groups = found.chunk_by(|a, b| a.page() == b.page() && a.section() == b.section());
    for group in groups {
        form::section(ui, &caption(&group[0]), |ui| {
            form::card(ui, |rows| {
                for found in group {
                    match found {
                        Found::Setting(setting) => controls::row(rows, *setting, &mut draft),
                        Found::Shortcut(shortcut) => keymap_page::row(rows, shortcut),
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

/// A sidebar entry: an icon and a name, highlighted when chosen, with a count at the right
/// while searching.
struct NavItem<'a> {
    glyph: &'a str,
    text: &'a str,
    selected: bool,
    /// Greys out a page the search finds nothing in.
    dimmed: bool,
    count: Option<usize>,
}

impl<'a> NavItem<'a> {
    fn new(glyph: &'a str, text: &'a str) -> Self {
        Self {
            glyph,
            text,
            selected: false,
            dimmed: false,
            count: None,
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
        let count = self
            .count
            .map(|count| count.to_string())
            .unwrap_or_default();
        let item = Button::selectable(self.selected, job)
            .right_text(RichText::new(count).small().color(palette.text_weak))
            .min_size(vec2(ui.available_width(), NAV_ITEM_HEIGHT));
        let response = ui.add(item);
        response.widget_info(|| {
            WidgetInfo::selected(WidgetType::Button, true, self.selected, self.text)
        });
        response
    }
}
