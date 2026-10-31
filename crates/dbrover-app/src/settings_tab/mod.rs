//! The Settings tab, opened by View › Settings… in the dock beside the consoles: a sidebar of
//! categories under a search field, and the chosen category's settings, or every setting the
//! search finds. Changes are saved to `settings.toml` at once; the MCP defaults go to
//! `projects.toml` with their own button, and the theme is egui's, kept with the window layout.

mod controls;
mod setting;

use dbrover_store::{McpPermissions, Settings};
use egui::{
    Button, CentralPanel, Color32, Frame, Margin, Panel, RichText, ScrollArea, TextEdit,
    ThemePreference, Ui, WidgetInfo, WidgetType, vec2,
};

use self::controls::Draft;
use self::setting::{Category, Setting};
use crate::commands::AppCommand;
use crate::form;
use crate::icons;
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
    /// Open the keymap or the custom actions, which have windows of their own.
    Run(AppCommand),
}

/// What the tab needs of the app to draw itself.
pub struct SettingsView<'a> {
    pub settings: &'a AppSettings,
    pub mcp: &'a mut McpDefaultsEditor,
}

/// The tab's own state: the category shown and what is typed in the search field. Not saved:
/// the tab opens on Appearance.
#[derive(Debug, Default)]
pub struct SettingsTab {
    category: Category,
    query: String,
}

impl SettingsTab {
    pub fn show(&mut self, ui: &mut Ui, view: SettingsView<'_>, actions: &mut Vec<SettingsAction>) {
        Panel::left("settings_sidebar")
            .resizable(false)
            .exact_size(SIDEBAR_WIDTH)
            .frame(Frame::new().inner_margin(SIDEBAR_MARGIN))
            .show(ui, |ui| self.sidebar(ui, actions));
        CentralPanel::default().frame(Frame::new()).show(ui, |ui| {
            ScrollArea::vertical()
                .id_salt("settings_page")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Frame::new().inner_margin(PAGE_MARGIN).show(ui, |ui| {
                        ui.set_max_width(PAGE_MAX_WIDTH);
                        self.page(ui, view, actions);
                    });
                });
        });
    }

    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// The settings to show: the chosen category's, or every one the search finds.
    fn shown(&self) -> Vec<Setting> {
        Setting::all()
            .into_iter()
            .filter(|setting| {
                if self.searching() {
                    setting.matches(&self.query)
                } else {
                    setting.category() == self.category
                }
            })
            .collect()
    }

    fn sidebar(&mut self, ui: &mut Ui, actions: &mut Vec<SettingsAction>) {
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
        let found: Vec<Setting> = if self.searching() {
            self.shown()
        } else {
            Vec::new()
        };
        for category in Category::ALL {
            let count = found
                .iter()
                .filter(|setting| setting.category() == category)
                .count();
            let item = NavItem {
                selected: !self.searching() && self.category == category,
                dimmed: self.searching() && count == 0,
                count: self.searching().then_some(count),
                ..NavItem::new(category.glyph(), category.title())
            };
            if item.show(ui).clicked() {
                self.category = category;
                self.query.clear();
            }
        }
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        for (glyph, text, command) in [
            (icons::KEYBOARD, "Keymap…", AppCommand::ShowKeymap),
            (icons::LIGHTNING, "Custom actions…", AppCommand::ShowActions),
        ] {
            if NavItem::new(glyph, text).show(ui).clicked() {
                actions.push(SettingsAction::Run(command));
            }
        }
    }

    fn page(&mut self, ui: &mut Ui, view: SettingsView<'_>, actions: &mut Vec<SettingsAction>) {
        let palette = Palette::for_visuals(ui.visuals());
        let shown = self.shown();
        if self.searching() {
            let heading = match shown.len() {
                0 => "No settings match".to_owned(),
                1 => "1 setting".to_owned(),
                n => format!("{n} settings"),
            };
            page_title(ui, &heading, &format!("Matching “{}”", self.query.trim()));
        } else {
            page_title(ui, self.category.title(), self.category.summary());
        }
        file_status(ui, view.settings, &palette);
        let theme = ui.ctx().options(|options| options.theme_preference);
        let mut draft = Draft {
            settings: view.settings.settings().clone(),
            theme,
            mcp: view.mcp,
            mcp_to_save: None,
        };
        for group in sections(&shown) {
            let caption = if self.searching() {
                format!("{} › {}", group[0].category().title(), group[0].section())
            } else {
                group[0].section().to_owned()
            };
            form::section(ui, &caption, |ui| {
                form::card(ui, |rows| {
                    for &setting in group {
                        controls::row(rows, setting, &mut draft);
                    }
                });
            });
        }
        file_footer(ui, view.settings, actions);
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
}

/// `settings` split into runs that share a card: the same category and section.
fn sections(settings: &[Setting]) -> Vec<&[Setting]> {
    settings
        .chunk_by(|a, b| a.category() == b.category() && a.section() == b.section())
        .collect()
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

/// Where the settings are saved, and Reload, for after editing the file by hand.
fn file_footer(ui: &mut Ui, settings: &AppSettings, actions: &mut Vec<SettingsAction>) {
    ui.horizontal_wrapped(|ui| {
        if let Some(path) = settings.path() {
            form::description_label(ui, &format!("Saved in {}", path.display()));
        }
        if ui
            .link("Reload")
            .on_hover_text("Read the file again, after editing it by hand")
            .clicked()
        {
            actions.push(SettingsAction::Reload);
        }
    });
}

/// A sidebar entry: an icon and a name, highlighted when chosen, with a count at the right
/// while searching.
struct NavItem<'a> {
    glyph: &'a str,
    text: &'a str,
    selected: bool,
    /// Greys out a category the search finds nothing in.
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
