//! **Query folders** in the explorer: each listed folder's tree of `.sql` files.

use std::collections::BTreeSet;
use std::path::PathBuf;

use dbrover_engine::QueryFolderProblem;
use dbrover_store::{QueryFile, QueryTree};
use egui::{Id, Label, RichText, Sense, Ui};

use super::{Listing, QueryFolder, QueryFolderAction, QueryFolders};
use crate::explorer::expander;
use crate::icons;
use crate::theme::Palette;
use crate::widgets;

pub const SECTION_TITLE: &str = "Query folders";

/// What the section remembers between frames.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SectionState {
    /// The section itself is folded.
    pub collapsed: bool,
    /// Folders and subfolders the user opened, by their path on this machine. Everything
    /// starts closed but a folder just added, so a long list stays short.
    pub expanded: BTreeSet<PathBuf>,
}

/// What a frame of the section needs.
struct Tree<'a> {
    /// Lowercase filter text: only files whose names hold it are shown, with the folders
    /// leading to them, opened.
    needle: Option<&'a str>,
    palette: &'a Palette,
}

/// Draws the section, under the data sources and reports. While filtering it is hidden
/// unless a file's name matches.
pub fn section(
    ui: &mut Ui,
    folders: &QueryFolders,
    needle: Option<&str>,
    state: &mut SectionState,
    actions: &mut Vec<QueryFolderAction>,
) {
    let tree = Tree {
        needle,
        palette: &Palette::for_visuals(ui.visuals()),
    };
    let shown: Vec<&QueryFolder> = folders
        .listed()
        .iter()
        .filter(|folder| tree.shows_folder(folder))
        .collect();
    if needle.is_some() && shown.is_empty() {
        return;
    }
    ui.add_space(4.0);
    let expanded = needle.is_some() || !state.collapsed;
    let row = ui.horizontal(|ui| {
        let toggled = expander(ui, expanded, SECTION_TITLE).clicked();
        ui.label(widgets::glyph(icons::FOLDER_OPEN).color(tree.palette.primary));
        let name = ui.add(
            Label::new(RichText::new(SECTION_TITLE).strong())
                .selectable(false)
                .sense(Sense::click()),
        );
        widgets::count_badge(ui, folders.listed().len());
        (name, toggled)
    });
    let (name, toggled) = row.inner;
    if toggled || name.double_clicked() {
        state.collapsed = expanded;
    }
    widgets::context_menu(&name, |ui| section_menu(ui, actions));
    if !expanded {
        return;
    }
    ui.indent(Id::new("explorer_query_folders"), |ui| {
        if folders.listed().is_empty() {
            ui.weak("No folders yet. Right-click Query folders to add one.");
        }
        for folder in shown {
            tree.folder(ui, folder, state, actions);
        }
    });
}

fn section_menu(ui: &mut Ui, actions: &mut Vec<QueryFolderAction>) {
    let items = [
        (
            icons::FOLDER_SIMPLE_PLUS,
            "Add query folder…",
            QueryFolderAction::Add,
        ),
        (icons::ARROWS_CLOCKWISE, "Reload", QueryFolderAction::Reload),
    ];
    for (glyph, label, action) in items {
        if widgets::menu_item(ui, glyph, label).clicked() {
            actions.push(action);
        }
    }
}

impl Tree<'_> {
    fn shows_folder(&self, folder: &QueryFolder) -> bool {
        match (self.needle, folder.listing()) {
            (None, _) => true,
            (Some(_), Listing::Read(tree)) => self.shows(tree),
            (Some(_), _) => false,
        }
    }

    fn shows(&self, tree: &QueryTree) -> bool {
        tree.files.iter().any(|file| self.shows_file(file))
            || tree.folders.iter().any(|folder| self.shows(&folder.tree))
    }

    fn shows_file(&self, file: &QueryFile) -> bool {
        self.needle
            .is_none_or(|needle| file.name.to_lowercase().contains(needle))
    }

    /// A listed folder: its row, with its path on hover, then what was read of it.
    fn folder(
        &self,
        ui: &mut Ui,
        folder: &QueryFolder,
        state: &mut SectionState,
        actions: &mut Vec<QueryFolderAction>,
    ) {
        let name = folder.name();
        let row = self.folder_row(ui, &name, folder.path().to_owned(), state);
        let row = row.on_hover_text(folder.path().display().to_string());
        widgets::context_menu(&row, |ui| {
            if widgets::menu_item(ui, icons::X, "Remove from list").clicked() {
                actions.push(QueryFolderAction::Remove(folder.written().to_owned()));
            }
            ui.separator();
            section_menu(ui, actions);
        });
        if !self.is_open(state, folder.path()) {
            return;
        }
        ui.indent(
            Id::new(("query_folder", folder.path())),
            |ui| match folder.listing() {
                Listing::Reading => {
                    ui.weak("Reading…");
                }
                Listing::Read(tree) if tree.files.is_empty() && tree.folders.is_empty() => {
                    ui.weak("No .sql files here.");
                }
                Listing::Read(tree) => self.tree(ui, tree, state),
                Listing::Failed(problem) => {
                    ui.colored_label(self.palette.error, problem_text(problem));
                }
            },
        );
    }

    fn tree(&self, ui: &mut Ui, tree: &QueryTree, state: &mut SectionState) {
        for folder in tree
            .folders
            .iter()
            .filter(|folder| self.shows(&folder.tree))
        {
            self.folder_row(ui, &folder.name, folder.path.clone(), state);
            if self.is_open(state, &folder.path) {
                ui.indent(Id::new(("query_folder", &folder.path)), |ui| {
                    self.tree(ui, &folder.tree, state);
                });
            }
        }
        for file in tree.files.iter().filter(|file| self.shows_file(file)) {
            self.file_row(ui, file);
        }
    }

    /// A folder's caret, icon and name; clicking the name opens or closes it, as the caret
    /// does.
    fn folder_row(
        &self,
        ui: &mut Ui,
        name: &str,
        path: PathBuf,
        state: &mut SectionState,
    ) -> egui::Response {
        let open = self.is_open(state, &path);
        let row = ui.horizontal(|ui| {
            let toggled = expander(ui, open, name).clicked();
            ui.label(widgets::glyph(icons::FOLDER_SIMPLE).color(self.palette.warning));
            let label = ui.add(Label::new(name).selectable(false).sense(Sense::click()));
            (label, toggled)
        });
        let (label, toggled) = row.inner;
        if (toggled || label.clicked()) && self.needle.is_none() {
            if open {
                state.expanded.remove(&path);
            } else {
                state.expanded.insert(path);
            }
        }
        label
    }

    fn file_row(&self, ui: &mut Ui, file: &QueryFile) -> egui::Response {
        ui.horizontal(|ui| {
            // Lined up with the folders' icons, past where their carets are.
            ui.add_space(ui.spacing().icon_width);
            ui.label(widgets::glyph(icons::FILE_SQL).color(self.palette.text_weak));
            ui.add(
                Label::new(file.name.as_str())
                    .selectable(false)
                    .sense(Sense::click()),
            )
            .on_hover_text(file.path.display().to_string())
        })
        .inner
    }

    /// While filtering, every folder leading to a match is open.
    fn is_open(&self, state: &SectionState, path: &std::path::Path) -> bool {
        self.needle.is_some() || state.expanded.contains(path)
    }
}

fn problem_text(problem: &QueryFolderProblem) -> String {
    match problem {
        QueryFolderProblem::Missing => "Not on this machine.".to_owned(),
        QueryFolderProblem::Failed(error) => format!("Can't read the folder: {error}"),
    }
}
