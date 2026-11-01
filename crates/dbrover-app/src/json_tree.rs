//! A JSON value as a tree: each field and item on a line of its own with its type, objects and
//! arrays folded and unfolded, and on each line's menu **Copy value**, **Copy path** and
//! **Copy all**.
//!
//! No engine in it: the cell viewer shows a SQL engine's JSON column with it, and documents and
//! Redis's JSON values use the same tree (`docs/nosql.md`, N0b).

use dbrover_document::{Path, Step, Syntax, Value, ValueType};
use egui::collapsing_header::CollapsingState;
use egui::{Id, Label, Response, RichText, Sense, Ui};

use crate::widgets;

/// How many fields or items of one object or array are drawn. The tree is laid out whole each
/// frame, so an array of a hundred thousand items unfolded would make every frame slow; past
/// this, a line says how many more there are, and **Copy value** on the array still copies
/// them all.
const MAX_CHILDREN_SHOWN: usize = 1_000;

/// What the tree shows: a value, read as `syntax`, under the name `root`.
pub struct JsonTree<'a> {
    /// What tells this tree's folds apart from another's: the same value shown twice keeps
    /// its folds apart.
    pub id: Id,
    pub root: &'a str,
    pub value: &'a Value,
    pub syntax: Syntax,
}

impl JsonTree<'_> {
    /// Draws the tree, its top level unfolded.
    pub fn show(&self, ui: &mut Ui) {
        self.node(ui, self.root, &Path::default(), self.value);
    }

    fn node(&self, ui: &mut Ui, key: &str, path: &Path, value: &Value) {
        let children = value.children(self.syntax);
        if children.is_empty() {
            ui.horizontal(|ui| {
                // Lines up with the keys of the folding lines beside it, past their toggle.
                ui.add_space(ui.spacing().icon_width + ui.spacing().icon_spacing);
                self.line(ui, key, path, value);
            });
            return;
        }
        let id = self.id.with(path);
        let unfolded_at_first = path.steps().is_empty();
        let mut line_clicked = false;
        let mut header = CollapsingState::load_with_default_open(ui.ctx(), id, unfolded_at_first)
            .show_header(ui, |ui| {
                line_clicked = self.line(ui, key, path, value).clicked();
            });
        if line_clicked {
            header.toggle();
        }
        header.body(|ui| {
            for (step, child) in children.iter().take(MAX_CHILDREN_SHOWN) {
                let child_key = match step {
                    Step::Field(name) => name.clone(),
                    Step::Index(index) => index.to_string(),
                };
                self.node(ui, &child_key, &path.child(step.clone()), child);
            }
            if children.len() > MAX_CHILDREN_SHOWN {
                ui.weak(format!(
                    "{} more not shown; Copy value copies them all",
                    children.len() - MAX_CHILDREN_SHOWN
                ));
            }
        });
    }

    /// One line: the key, the value as shown, and its type, with the copy menu on the key and
    /// the value. Returns their response: a click on either folds and unfolds a line that has
    /// children.
    fn line(&self, ui: &mut Ui, key: &str, path: &Path, value: &Value) -> Response {
        let shown = value.shown(self.syntax);
        let key_response = ui.add(
            Label::new(RichText::new(key).strong())
                .selectable(false)
                .sense(Sense::click()),
        );
        let value_text = match shown.value_type {
            ValueType::Object | ValueType::Array => RichText::new(&shown.text).weak(),
            ValueType::Null | ValueType::Undefined => RichText::new(&shown.text).italics().weak(),
            _ => RichText::new(&shown.text).monospace(),
        };
        let value_response = ui.add(
            Label::new(value_text)
                .truncate()
                .selectable(false)
                .sense(Sense::click()),
        );
        ui.weak(shown.value_type.name());
        for response in [&key_response, &value_response] {
            widgets::context_menu(response, |ui| self.copy_menu(ui, path, value));
        }
        key_response.union(value_response)
    }

    fn copy_menu(&self, ui: &mut Ui, path: &Path, value: &Value) {
        if ui.button("Copy value").clicked() {
            ui.ctx().copy_text(value.copied(self.syntax));
        }
        if !path.steps().is_empty() && ui.button("Copy path").clicked() {
            ui.ctx().copy_text(path.to_string());
        }
        if ui.button("Copy all").clicked() {
            ui.ctx().copy_text(self.value.to_json());
        }
    }
}
