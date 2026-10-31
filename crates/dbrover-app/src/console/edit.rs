//! Editing result rows: which results can be edited, the changes waiting to be submitted
//! (cells typed over, rows added, rows marked for deletion), and the statements that submit
//! them: SQL for a table's rows, one Redis command per change for a key's value, one MongoDB
//! command per change for a collection's documents. Pure; the grid draws it and the engine runs
//! the statements.

use std::collections::{BTreeMap, BTreeSet};

use dbrover_core::{Cell, ColumnInfo, Identifier, RelationName, Row, TableDescription};
use dbrover_engine::RowEdit;
use dbrover_export::{SourceTable, SqlTable};
use dbrover_language::DocumentView;
use dbrover_language::redis::KeyView;
use dbrover_sql::{ColumnValue, Dialect, RowSql, Token, TokenKind, first_word, tokenize};
use dbrover_store::{NamedCell, ReplacedCell, RowChange, RowChangeKind};

use super::documents::DocumentResult;

/// Words that make a `SELECT` combine, group or deduplicate rows, so a result row no longer
/// stands for one table row.
const NOT_ONE_ROW_PER_ROW: &[&str] = &[
    "join",
    "union",
    "intersect",
    "except",
    "group",
    "having",
    "distinct",
    "window",
];

/// Whether a statement's result can be edited, as far as is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Editability {
    /// Not looked at yet.
    Unknown,
    /// The table is being described; editing waits for the answer.
    Describing(RelationName),
    Editable(EditTarget),
    /// A Redis key's value, as the command that read it whole shows it.
    Key(KeyView),
    /// A collection's documents, as a `find` returned them.
    Documents(DocumentsTarget),
    ReadOnly(String),
}

/// What editing a result's documents needs: where they can be changed, and the columns that
/// name each loaded document (`_id`, a hit's `_index`), which aren't typed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentsTarget {
    pub view: DocumentView,
    key_columns: Vec<usize>,
}

impl DocumentsTarget {
    pub fn new(view: DocumentView, columns: &[ColumnInfo]) -> Self {
        let key_columns = columns
            .iter()
            .enumerate()
            .filter(|(_, column)| view.is_key(&column.name))
            .map(|(index, _)| index)
            .collect();
        Self { view, key_columns }
    }
}

/// What submitting edits needs: the table, and how result columns map onto it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditTarget {
    pub table: TableDescription,
    /// The result columns holding the primary key, in key order.
    key: Vec<usize>,
    /// Per result column: whether it is a column of the table (and named once).
    editable: Vec<bool>,
}

impl EditTarget {
    pub fn is_editable(&self, column: usize) -> bool {
        self.editable.get(column).copied().unwrap_or(false)
    }

    /// The table, its columns and its key among the result's, for the SQL export formats.
    pub fn source_table(&self) -> SourceTable {
        SourceTable {
            name: SqlTable {
                schema: Some(self.table.schema.clone()),
                name: self.table.name.clone(),
            },
            columns: Some(
                (0..self.editable.len())
                    .filter(|&column| self.is_editable(column))
                    .collect(),
            ),
            key: Some(self.key.clone()),
        }
    }
}

/// The table a statement's rows come from, if they come from exactly one table, one row per
/// table row: a `SELECT` from a single table, without joins, set operations, grouping or
/// `DISTINCT`. `Err` says why not.
pub fn editable_source(sql: &str, dialect: Dialect) -> Result<RelationName, String> {
    let not_editable = || Err("Only rows selected from a single table can be edited".to_owned());
    if first_word(sql, dialect).as_deref() != Some("select") {
        return not_editable();
    }
    let tokens: Vec<Token> = tokenize(sql, dialect)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();
    let text = |token: &Token| &sql[token.range.clone()];
    let mut depth = 0usize;
    let mut from = None;
    for (index, token) in tokens.iter().enumerate() {
        match text(token) {
            "(" => depth += 1,
            ")" => depth = depth.saturating_sub(1),
            _ if depth > 0 || token.kind != TokenKind::Word => {}
            word => {
                let word = word.to_ascii_lowercase();
                if NOT_ONE_ROW_PER_ROW.contains(&word.as_str())
                    || (word == "from" && from.is_some())
                {
                    return not_editable();
                }
                if word == "from" {
                    from = Some(index + 1);
                }
            }
        }
    }
    let Some(start) = from else {
        return not_editable();
    };
    let Some((name, next)) = relation_name(sql, &tokens, start) else {
        return not_editable();
    };
    if tokens.get(next).is_some_and(|token| text(token) == ",") {
        return not_editable();
    }
    Ok(name)
}

fn relation_name(sql: &str, tokens: &[Token], start: usize) -> Option<(RelationName, usize)> {
    let identifier = |index: usize| {
        let token = tokens.get(index)?;
        let written = &sql[token.range.clone()];
        match token.kind {
            TokenKind::Word => Some(Identifier {
                name: written.to_owned(),
                quoted: false,
            }),
            TokenKind::QuotedIdentifier => Some(Identifier {
                name: written
                    .get(1..written.len().checked_sub(1)?)?
                    .replace("\"\"", "\""),
                quoted: true,
            }),
            _ => None,
        }
    };
    let first = identifier(start)?;
    let dotted = tokens
        .get(start + 1)
        .is_some_and(|token| &sql[token.range.clone()] == ".");
    if dotted {
        let name = identifier(start + 2)?;
        Some((
            RelationName {
                schema: Some(first),
                name,
            },
            start + 3,
        ))
    } else {
        Some((
            RelationName {
                schema: None,
                name: first,
            },
            start + 1,
        ))
    }
}

/// Maps a result onto the described table. Read-only, with the reason, if the table has no
/// primary key or the result lacks part of it.
pub fn edit_target(table: TableDescription, columns: &[ColumnInfo]) -> Editability {
    if table.primary_key.is_empty() {
        return Editability::ReadOnly(format!(
            "{} has no primary key, so its rows can't be told apart",
            table.name
        ));
    }
    let named_once = |name: &str| columns.iter().filter(|column| column.name == name).count() == 1;
    let editable: Vec<bool> = columns
        .iter()
        .map(|column| table.has_column(&column.name) && named_once(&column.name))
        .collect();
    let key: Option<Vec<usize>> = table
        .primary_key
        .iter()
        .map(|key_column| {
            columns
                .iter()
                .position(|column| &column.name == key_column)
                .filter(|&index| editable[index])
        })
        .collect();
    match key {
        Some(key) => Editability::Editable(EditTarget {
            table,
            key,
            editable,
        }),
        None => Editability::ReadOnly(format!(
            "Select the primary key ({}) to edit rows",
            table.primary_key.join(", ")
        )),
    }
}

/// Editing one statement's result: whether it can be, what changed, and the cell being
/// typed into.
#[derive(Debug)]
pub struct EditSession {
    pub editability: Editability,
    pub pending: PendingEdits,
    pub editing: Option<CellEditor>,
    /// Edits were sent and the database hasn't answered yet.
    pub submitting: bool,
    /// Why the last submit failed; the changes stay pending.
    pub error: Option<String>,
    /// Rows were added, so the statement should run again to show what the database put in
    /// the columns left to their defaults.
    pub reload: bool,
}

impl Default for EditSession {
    fn default() -> Self {
        Self {
            editability: Editability::Unknown,
            pending: PendingEdits::default(),
            editing: None,
            submitting: false,
            error: None,
            reload: false,
        }
    }
}

impl EditSession {
    /// The table the rows can be edited in; `None` for a key's value, which isn't a table.
    pub fn target(&self) -> Option<&EditTarget> {
        match &self.editability {
            Editability::Editable(target) => Some(target),
            Editability::Unknown
            | Editability::Describing(_)
            | Editability::Key(_)
            | Editability::Documents(_)
            | Editability::ReadOnly(_) => None,
        }
    }

    /// The collection whose documents the rows are, when they can be edited.
    pub fn documents(&self) -> Option<&DocumentsTarget> {
        match &self.editability {
            Editability::Documents(target) => Some(target),
            Editability::Unknown
            | Editability::Describing(_)
            | Editability::Editable(_)
            | Editability::Key(_)
            | Editability::ReadOnly(_) => None,
        }
    }

    /// The key whose value the rows are, when they can be edited.
    pub fn key_view(&self) -> Option<&KeyView> {
        match &self.editability {
            Editability::Key(view) => Some(view),
            Editability::Unknown
            | Editability::Describing(_)
            | Editability::Editable(_)
            | Editability::Documents(_)
            | Editability::ReadOnly(_) => None,
        }
    }

    /// Whether rows can be added and deleted right now: a table's and a collection's always, a
    /// key's when its value is a collection.
    pub fn can_change_rows(&self) -> bool {
        !self.submitting
            && (self.target().is_some()
                || self.documents().is_some()
                || self.key_view().is_some_and(KeyView::changes_rows))
    }

    /// Drops every pending change.
    pub fn revert(&mut self) {
        self.pending.clear();
        self.editing = None;
        self.error = None;
    }

    /// Whether `column` of `row` can be typed into: never in a row marked for deletion.
    pub fn is_editable(&self, row: RowRef, column: usize) -> bool {
        if self.submitting || self.pending.is_deleted(row) {
            return false;
        }
        if let Some(documents) = self.documents() {
            return matches!(row, RowRef::New(_)) || !documents.key_columns.contains(&column);
        }
        match (self.target(), self.key_view(), row) {
            (Some(target), _, _) => target.is_editable(column),
            (None, Some(view), RowRef::Loaded(_)) => view.edits_loaded(column),
            (None, Some(view), RowRef::New(_)) => view.edits_new(column),
            (None, None, _) => false,
        }
    }

    /// Adds an empty row, every column at its default.
    pub fn add_row(&mut self) {
        if self.can_change_rows() {
            self.pending.add_row();
        }
    }

    /// Marks rows for deletion, or drops added ones. The cell being typed into goes too, since
    /// its row may be gone or have moved.
    pub fn delete_rows(&mut self, rows: &[RowRef]) {
        if !self.can_change_rows() {
            return;
        }
        self.editing = None;
        self.pending.delete(rows);
    }
}

/// A row of the grid: one of the loaded rows, by its index among them, or one added and not
/// submitted yet, by its index among those.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowRef {
    Loaded(usize),
    New(usize),
}

impl RowRef {
    /// How errors name the row, counting from 1 as the grid's row numbers do.
    pub fn describe(self) -> String {
        match self {
            Self::Loaded(row) => format!("Row {}", row + 1),
            Self::New(row) => format!("New row {}", row + 1),
        }
    }
}

/// A cell being typed into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellEditor {
    pub row: RowRef,
    pub column: usize,
    pub text: String,
    /// Whether the text field has been given focus yet; it asks once, when it appears.
    pub focused: bool,
}

/// How many rows the pending changes add, change and delete.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChangeCount {
    pub inserted: usize,
    pub updated: usize,
    pub deleted: usize,
}

impl ChangeCount {
    pub fn is_empty(self) -> bool {
        self == Self::default()
    }

    /// What submitting does, for a confirmation: "inserts 1 row, updates 2 edited rows and
    /// deletes 1 row".
    pub fn describe(self) -> String {
        let plural = |count: usize, one: &str, many: &str| match count {
            1 => format!("1 {one}"),
            n => format!("{n} {many}"),
        };
        let parts: Vec<String> = [
            (
                self.inserted,
                "inserts",
                plural(self.inserted, "row", "rows"),
            ),
            (
                self.updated,
                "updates",
                plural(self.updated, "edited row", "edited rows"),
            ),
            (self.deleted, "deletes", plural(self.deleted, "row", "rows")),
        ]
        .into_iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(_, verb, rows)| format!("{verb} {rows}"))
        .collect();
        match parts.split_last() {
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
            None => "changes nothing".to_owned(),
        }
    }
}

/// Changes made in the grid and not submitted yet: values typed over loaded cells, rows
/// added, and loaded rows marked for deletion.
#[derive(Debug, Default)]
pub struct PendingEdits {
    /// By loaded row, then column.
    changes: BTreeMap<usize, BTreeMap<usize, Cell>>,
    /// Added rows, each with the values typed into it by column; a column not set gets its
    /// default.
    inserted: Vec<BTreeMap<usize, Cell>>,
    /// Loaded rows to delete.
    deleted: BTreeSet<usize>,
}

impl PendingEdits {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.inserted.is_empty() && self.deleted.is_empty()
    }

    /// How many loaded cells have changed.
    pub fn changed_cells(&self) -> usize {
        self.changes.values().map(BTreeMap::len).sum()
    }

    pub fn count(&self) -> ChangeCount {
        ChangeCount {
            inserted: self.inserted.len(),
            updated: self.changes.len(),
            deleted: self.deleted.len(),
        }
    }

    /// How many rows have been added.
    pub fn new_rows(&self) -> usize {
        self.inserted.len()
    }

    /// The value typed into a cell, if one was.
    pub fn get(&self, row: RowRef, column: usize) -> Option<&Cell> {
        match row {
            RowRef::Loaded(row) => self.changes.get(&row)?.get(&column),
            RowRef::New(row) => self.inserted.get(row)?.get(&column),
        }
    }

    pub fn is_deleted(&self, row: RowRef) -> bool {
        matches!(row, RowRef::Loaded(row) if self.deleted.contains(&row))
    }

    /// Records a new value; setting a loaded cell back to what was loaded drops the change.
    pub fn set(&mut self, rows: &[Row], row: RowRef, column: usize, value: Cell) {
        let row = match row {
            RowRef::New(row) => {
                if let Some(added) = self.inserted.get_mut(row) {
                    added.insert(column, value);
                }
                return;
            }
            RowRef::Loaded(row) => row,
        };
        let original = rows.get(row).and_then(|loaded| loaded.get(column));
        let changes = self.changes.entry(row).or_default();
        if original == Some(&value) {
            changes.remove(&column);
        } else {
            changes.insert(column, value);
        }
        if changes.is_empty() {
            self.changes.remove(&row);
        }
    }

    pub fn add_row(&mut self) {
        self.inserted.push(BTreeMap::new());
    }

    /// Marks loaded rows for deletion, dropping what was typed into them, and drops added
    /// rows.
    pub fn delete(&mut self, rows: &[RowRef]) {
        let mut added = Vec::new();
        for &row in rows {
            match row {
                RowRef::Loaded(row) => {
                    self.changes.remove(&row);
                    self.deleted.insert(row);
                }
                RowRef::New(row) => added.push(row),
            }
        }
        // From the last, so the indices still to remove stay valid.
        added.sort_unstable();
        added.dedup();
        for row in added.into_iter().rev() {
            if row < self.inserted.len() {
                self.inserted.remove(row);
            }
        }
    }

    /// Unmarks a loaded row marked for deletion.
    pub fn restore(&mut self, row: usize) {
        self.deleted.remove(&row);
    }

    pub fn clear(&mut self) {
        self.changes.clear();
        self.inserted.clear();
        self.deleted.clear();
    }

    /// One statement per changed row, with the row it is for and, for the history, what it
    /// does to the row: the `DELETE`s, then the `UPDATE`s, then the `INSERT`s, so that a key a
    /// deleted row frees can be taken by an edited or added row. Loaded rows are named by their
    /// primary key as loaded.
    pub fn statements(
        &self,
        target: &EditTarget,
        columns: &[ColumnInfo],
        rows: &[Row],
        dialect: Dialect,
    ) -> Vec<(RowRef, RowEdit)> {
        let changes = RowChanges {
            sql: RowSql::new(dialect, &target.table),
            table: format!("{}.{}", target.table.schema, target.table.name),
            target,
            columns,
            rows,
        };
        let deletes = self
            .deleted
            .iter()
            .filter_map(|&row| Some((RowRef::Loaded(row), changes.delete(row)?)));
        let updates = self.changes.iter().filter_map(|(&row, changed)| {
            Some((RowRef::Loaded(row), changes.update(row, changed)?))
        });
        let inserts = self
            .inserted
            .iter()
            .enumerate()
            .map(|(row, values)| (RowRef::New(row), changes.insert(values)));
        deletes.chain(updates).chain(inserts).collect()
    }

    /// One Redis command per change to `view`'s value, with the row it is for and, for the
    /// history, what it does, in the order [`Self::statements`] gives SQL's: removals, changes,
    /// then additions.
    ///
    /// # Errors
    ///
    /// Which row lacks what its command needs: an added hash field, set member or sorted set
    /// member and score must be typed in.
    pub fn key_commands(
        &self,
        view: &KeyView,
        columns: &[ColumnInfo],
        rows: &[Row],
    ) -> Result<Vec<(RowRef, RowEdit)>, String> {
        let commands = KeyCommands {
            view,
            key: String::from_utf8_lossy(view.key()).into_owned(),
            columns,
        };
        let missing = |row: RowRef| {
            format!(
                "{}: type {} first",
                row.describe(),
                commands.named_columns()
            )
        };
        let mut edits = Vec::new();
        for &row in &self.deleted {
            let loaded = rows.get(row).map(Vec::as_slice).unwrap_or_default();
            let edit = commands
                .delete(loaded)
                .ok_or_else(|| missing(RowRef::Loaded(row)))?;
            edits.push((RowRef::Loaded(row), edit));
        }
        for (&row, changed) in &self.changes {
            let loaded = rows.get(row).map(Vec::as_slice).unwrap_or_default();
            for (&column, value) in changed {
                let edit = commands
                    .update(loaded, column, value)
                    .ok_or_else(|| missing(RowRef::Loaded(row)))?;
                edits.push((RowRef::Loaded(row), edit));
            }
        }
        for (index, values) in self.inserted.iter().enumerate() {
            let values: Vec<Cell> = (0..columns.len())
                .map(|column| values.get(&column).cloned().flatten())
                .collect();
            let edit = commands
                .insert(&values)
                .ok_or_else(|| missing(RowRef::New(index)))?;
            edits.push((RowRef::New(index), edit));
        }
        Ok(edits)
    }

    /// One command per change to `target`'s documents, in their language, with the row it is
    /// for and, for the history, what it does, in the order [`Self::statements`] gives SQL's:
    /// deletions, changed fields, then added documents. A document is found by its `_id` (and a
    /// hit by its `_index` too); a field is set with the type it had (see
    /// `dbrover_language::DocumentView::update`).
    ///
    /// # Errors
    ///
    /// Which row's change can't be written: a document that can't be named, text that isn't a
    /// value of the field's type, an added document with nothing typed in.
    pub fn document_commands(
        &self,
        target: &DocumentsTarget,
        documents: &DocumentResult,
        columns: &[ColumnInfo],
    ) -> Result<Vec<(RowRef, RowEdit)>, String> {
        let commands = DocumentCommands {
            view: &target.view,
            documents,
            columns,
        };
        let failed = |row: RowRef| move |reason: String| format!("{}: {reason}", row.describe());
        let mut edits = Vec::new();
        for &row in &self.deleted {
            let edit = commands.delete(row).map_err(failed(RowRef::Loaded(row)))?;
            edits.push((RowRef::Loaded(row), edit));
        }
        for (&row, changed) in &self.changes {
            for (&column, value) in changed {
                let edit = commands
                    .update(row, column, value)
                    .map_err(failed(RowRef::Loaded(row)))?;
                edits.push((RowRef::Loaded(row), edit));
            }
        }
        for (index, values) in self.inserted.iter().enumerate() {
            let edit = commands
                .insert(values)
                .map_err(failed(RowRef::New(index)))?;
            edits.push((RowRef::New(index), edit));
        }
        Ok(edits)
    }

    /// Brings the loaded rows up to date once the database has the changes: edited values
    /// written in, deleted rows removed. Added rows aren't loaded rows yet; the statement runs
    /// again to show them as the database has them.
    pub fn apply_to(&mut self, rows: &mut Vec<Row>) {
        for (row, changes) in std::mem::take(&mut self.changes) {
            if let Some(loaded) = rows.get_mut(row) {
                for (column, value) in changes {
                    if let Some(cell) = loaded.get_mut(column) {
                        *cell = value;
                    }
                }
            }
        }
        // From the last, so the indices still to remove stay valid.
        for row in std::mem::take(&mut self.deleted).into_iter().rev() {
            if row < rows.len() {
                rows.remove(row);
            }
        }
        self.inserted.clear();
    }
}

/// Writes each changed row's statement, and the change it makes with the statement that would
/// undo it.
struct RowChanges<'a> {
    sql: RowSql<'a>,
    /// As the history shows it.
    table: String,
    target: &'a EditTarget,
    columns: &'a [ColumnInfo],
    rows: &'a [Row],
}

impl RowChanges<'_> {
    fn delete(&self, row: usize) -> Option<RowEdit> {
        let key = self.key(row, &BTreeMap::new())?;
        let loaded = self.rows.get(row)?;
        let whole_row: Vec<NamedCell> = loaded
            .iter()
            .enumerate()
            .filter(|(column, _)| self.target.is_editable(*column))
            .filter_map(|(column, value)| self.named(column, value.clone()))
            .collect();
        Some(self.edit(
            self.sql.delete(&column_values(&key)),
            RowChangeKind::Deleted {
                key,
                row: whole_row.clone(),
            },
            Some(self.sql.insert(&column_values(&whole_row))),
        ))
    }

    /// The revert sets the old values back on the row as the edit leaves it: by its new key,
    /// if the edit changed the key.
    fn update(&self, row: usize, changed: &BTreeMap<usize, Cell>) -> Option<RowEdit> {
        let key = self.key(row, &BTreeMap::new())?;
        let new_key = self.key(row, changed)?;
        let loaded = self.rows.get(row)?;
        let set = self.named_all(changed);
        let cells: Vec<ReplacedCell> = changed
            .iter()
            .filter_map(|(&column, new)| {
                Some(ReplacedCell {
                    column: self.columns.get(column)?.name.clone(),
                    old: loaded.get(column)?.clone(),
                    new: new.clone(),
                })
            })
            .collect();
        let old: Vec<NamedCell> = cells
            .iter()
            .map(|cell| NamedCell {
                column: cell.column.clone(),
                value: cell.old.clone(),
            })
            .collect();
        Some(
            self.edit(
                self.sql.update(&column_values(&set), &column_values(&key)),
                RowChangeKind::Updated { key, cells },
                Some(
                    self.sql
                        .update(&column_values(&old), &column_values(&new_key)),
                ),
            ),
        )
    }

    /// An added row's generated key isn't known, so it has no revert.
    fn insert(&self, values: &BTreeMap<usize, Cell>) -> RowEdit {
        let values = self.named_all(values);
        self.edit(
            self.sql.insert(&column_values(&values)),
            RowChangeKind::Inserted { values },
            None,
        )
    }

    /// The row's primary key, with the values in `changed` in place of the loaded ones.
    fn key(&self, row: usize, changed: &BTreeMap<usize, Cell>) -> Option<Vec<NamedCell>> {
        let loaded = self.rows.get(row)?;
        self.target
            .key
            .iter()
            .map(|&column| {
                let value = changed.get(&column).or(loaded.get(column))?;
                self.named(column, value.clone())
            })
            .collect()
    }

    fn named_all(&self, values: &BTreeMap<usize, Cell>) -> Vec<NamedCell> {
        values
            .iter()
            .filter_map(|(&column, value)| self.named(column, value.clone()))
            .collect()
    }

    fn named(&self, column: usize, value: Cell) -> Option<NamedCell> {
        Some(NamedCell {
            column: self.columns.get(column)?.name.clone(),
            value,
        })
    }

    fn edit(&self, sql: String, kind: RowChangeKind, revert: Option<String>) -> RowEdit {
        RowEdit {
            sql,
            change: Some(RowChange {
                table: self.table.clone(),
                kind,
                revert,
            }),
        }
    }
}

/// Writes each change to a key's value as its command, and the change it makes, with the
/// command that would undo it, for the history.
struct KeyCommands<'a> {
    view: &'a KeyView,
    /// As the history shows it, in the place of a table.
    key: String,
    columns: &'a [ColumnInfo],
}

impl KeyCommands<'_> {
    /// The columns that name a row, which an added row must have typed in.
    fn named_columns(&self) -> String {
        let names: Vec<&str> = self
            .columns
            .iter()
            .enumerate()
            .filter(|(index, _)| self.view.edits_new(*index))
            .map(|(_, column)| column.name.as_str())
            .collect();
        names.join(" and ")
    }

    fn delete(&self, row: &[Cell]) -> Option<RowEdit> {
        let command = self.view.delete(row)?;
        let values = self.named_all(row);
        Some(self.edit(
            command,
            RowChangeKind::Deleted {
                key: self.naming(row),
                row: values,
            },
            self.view.insert(row),
        ))
    }

    fn update(&self, row: &[Cell], column: usize, value: &Cell) -> Option<RowEdit> {
        let command = self.view.update(row, column, value)?;
        let old = row.get(column)?.clone();
        let revert = self.view.update(row, column, &old);
        Some(self.edit(
            command,
            RowChangeKind::Updated {
                key: self.naming(row),
                cells: vec![ReplacedCell {
                    column: self.columns.get(column)?.name.clone(),
                    old,
                    new: value.clone(),
                }],
            },
            revert,
        ))
    }

    fn insert(&self, values: &[Cell]) -> Option<RowEdit> {
        let command = self.view.insert(values)?;
        let revert = self.view.delete(values);
        Some(self.edit(
            command,
            RowChangeKind::Inserted {
                values: self.named_all(values),
            },
            revert,
        ))
    }

    /// The cells that tell a loaded row apart: those that can't be typed over.
    fn naming(&self, row: &[Cell]) -> Vec<NamedCell> {
        self.columns
            .iter()
            .zip(row)
            .enumerate()
            .filter(|(index, _)| !self.view.edits_loaded(*index))
            .map(|(_, (column, value))| NamedCell {
                column: column.name.clone(),
                value: value.clone(),
            })
            .collect()
    }

    fn named_all(&self, row: &[Cell]) -> Vec<NamedCell> {
        self.columns
            .iter()
            .zip(row)
            .map(|(column, value)| NamedCell {
                column: column.name.clone(),
                value: value.clone(),
            })
            .collect()
    }

    fn edit(&self, command: String, kind: RowChangeKind, revert: Option<String>) -> RowEdit {
        RowEdit {
            sql: command,
            change: Some(RowChange {
                table: self.key.clone(),
                kind,
                revert,
            }),
        }
    }
}

/// Writes each change to a result's documents as its command, in the language that read them,
/// and the change it makes, with the command that would undo it, for the history.
struct DocumentCommands<'a> {
    view: &'a DocumentView,
    documents: &'a DocumentResult,
    columns: &'a [ColumnInfo],
}

impl DocumentCommands<'_> {
    fn document(&self, row: usize) -> Result<&dbrover_document::Value, String> {
        self.documents
            .document(row)
            .ok_or_else(|| "this row isn't a document".to_owned())
    }

    fn delete(&self, row: usize) -> Result<RowEdit, String> {
        let document = self.document(row)?;
        let edit = self.view.delete(document)?;
        Ok(self.edit(
            edit.command,
            RowChangeKind::Deleted {
                key: key_cells(edit.key),
                row: vec![named("document", Some(document.to_json()))],
            },
            edit.revert,
        ))
    }

    fn update(&self, row: usize, column: usize, value: &Cell) -> Result<RowEdit, String> {
        let document = self.document(row)?;
        let field = self
            .documents
            .field_of(column)
            .ok_or_else(|| "this column isn't a field".to_owned())?;
        let old = self.documents.value(row, column);
        let (edit, path) = self.view.update(document, field, old, value.as_deref())?;
        Ok(self.edit(
            edit.command,
            RowChangeKind::Updated {
                key: key_cells(edit.key),
                cells: vec![ReplacedCell {
                    column: path,
                    old: old.map(dbrover_document::Value::to_json),
                    new: value.clone(),
                }],
            },
            edit.revert,
        ))
    }

    fn insert(&self, values: &BTreeMap<usize, Cell>) -> Result<RowEdit, String> {
        let mut fields = Vec::new();
        for (&column, value) in values {
            let name = &self
                .columns
                .get(column)
                .ok_or_else(|| "this column isn't a field".to_owned())?
                .name;
            fields.push((name.as_str(), value.as_deref()));
        }
        let command = self.view.insert(&fields)?;
        let typed = fields
            .iter()
            .map(|(name, value)| named(name, value.map(str::to_owned)))
            .collect();
        Ok(self.edit(command, RowChangeKind::Inserted { values: typed }, None))
    }

    fn edit(&self, command: String, kind: RowChangeKind, revert: Option<String>) -> RowEdit {
        RowEdit {
            sql: command,
            change: Some(RowChange {
                table: self.view.name().to_owned(),
                kind,
                revert,
            }),
        }
    }
}

/// What names a document, as the history records a row's key.
fn key_cells(key: Vec<(String, String)>) -> Vec<NamedCell> {
    key.into_iter()
        .map(|(column, value)| named(&column, Some(value)))
        .collect()
}

fn named(column: &str, value: Cell) -> NamedCell {
    NamedCell {
        column: column.to_owned(),
        value,
    }
}

fn column_values(values: &[NamedCell]) -> Vec<ColumnValue<'_>> {
    values
        .iter()
        .map(|named| ColumnValue {
            column: &named.column,
            value: &named.value,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use dbrover_core::ColumnDescription;

    use super::*;

    fn column(name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.to_owned(),
            type_name: None,
        }
    }

    fn table(columns: &[&str], key: &[&str]) -> TableDescription {
        TableDescription {
            schema: "public".to_owned(),
            name: "people".to_owned(),
            columns: columns
                .iter()
                .map(|name| ColumnDescription {
                    name: (*name).to_owned(),
                    data_type: "text".to_owned(),
                    nullable: true,
                    default: None,
                    identity: None,
                })
                .collect(),
            primary_key: key.iter().map(|c| (*c).to_owned()).collect(),
            foreign_keys: vec![],
            referenced_by: vec![],
        }
    }

    fn cells(values: &[Option<&str>]) -> Row {
        values
            .iter()
            .map(|value| value.map(str::to_owned))
            .collect()
    }

    #[test]
    fn finds_the_one_table_a_select_reads() {
        let editable = [
            (
                "select * from people where id > 1 order by id",
                None,
                "people",
                false,
            ),
            (
                "SELECT id FROM public.\"People\" p LIMIT 5",
                Some("public"),
                "People",
                true,
            ),
            ("select (select 1), name from people", None, "people", false),
        ];
        for (sql, schema, name, quoted) in editable {
            let found = editable_source(sql, Dialect::Postgres).unwrap();
            assert_eq!(found.schema.map(|s| s.name).as_deref(), schema, "{sql}");
            assert_eq!(
                (found.name.name.as_str(), found.name.quoted),
                (name, quoted),
                "{sql}"
            );
        }
        for sql in [
            "update people set name = 'x'",
            "select 1",
            "select * from a, b",
            "select * from a join b on true",
            "select name, count(*) from people group by name",
            "select distinct name from people",
            "select * from a union select * from b",
            "select * from (select * from people) p",
        ] {
            assert!(editable_source(sql, Dialect::Postgres).is_err(), "{sql}");
        }
    }

    #[test]
    fn needs_the_whole_primary_key_in_the_result() {
        let described = table(&["id", "part", "name"], &["id", "part"]);
        let result = [
            column("id"),
            column("name"),
            column("part"),
            column("upper"),
        ];
        let Editability::Editable(target) = edit_target(described.clone(), &result) else {
            panic!("expected the result to be editable");
        };
        assert_eq!(target.key, [0, 2]);
        assert!(target.is_editable(1));
        assert!(!target.is_editable(3), "not a table column");

        assert!(matches!(
            edit_target(described.clone(), &[column("id"), column("name")]),
            Editability::ReadOnly(reason) if reason.contains("(id, part)")
        ));
        assert!(matches!(
            edit_target(table(&["n"], &[]), &[column("n")]),
            Editability::ReadOnly(reason) if reason.contains("no primary key")
        ));
        assert!(matches!(
            edit_target(described, &[column("id"), column("id"), column("part")]),
            Editability::ReadOnly(_)
        ));
    }

    fn editable(table: TableDescription, columns: &[ColumnInfo]) -> EditTarget {
        let Editability::Editable(target) = edit_target(table, columns) else {
            panic!("expected the result to be editable");
        };
        target
    }

    fn sql(statements: Vec<(RowRef, RowEdit)>) -> Vec<String> {
        statements.into_iter().map(|(_, edit)| edit.sql).collect()
    }

    fn named(column: &str, value: Option<&str>) -> NamedCell {
        NamedCell {
            column: column.to_owned(),
            value: value.map(str::to_owned),
        }
    }

    /// Each statement carries its row's change for the history: the values an update replaced,
    /// a deleted row's every value, and the statement that would put the row back, keyed by
    /// the row's new key when the edit changed the key.
    #[test]
    fn records_what_each_statement_does_to_its_row() {
        let columns = [column("id"), column("name"), column("note")];
        let target = editable(table(&["id", "name"], &["id"]), &columns);
        let rows = vec![
            cells(&[Some("1"), Some("ada"), Some("not a table column")]),
            cells(&[Some("2"), Some("bob"), None]),
        ];
        let mut pending = PendingEdits::default();
        pending.set(&rows, RowRef::Loaded(0), 0, Some("10".to_owned()));
        pending.set(&rows, RowRef::Loaded(0), 1, None);
        pending.delete(&[RowRef::Loaded(1)]);
        pending.add_row();
        pending.set(&rows, RowRef::New(0), 1, Some("cy".to_owned()));

        let changes: Vec<RowChange> = pending
            .statements(&target, &columns, &rows, Dialect::Sqlite)
            .into_iter()
            .map(|(_, edit)| edit.change.unwrap())
            .collect();
        let table = "public.people".to_owned();
        assert_eq!(
            changes,
            [
                RowChange {
                    table: table.clone(),
                    kind: RowChangeKind::Deleted {
                        key: vec![named("id", Some("2"))],
                        row: vec![named("id", Some("2")), named("name", Some("bob"))],
                    },
                    revert: Some(
                        r#"INSERT INTO "public"."people" ("id", "name") VALUES ('2', 'bob')"#
                            .to_owned()
                    ),
                },
                RowChange {
                    table: table.clone(),
                    kind: RowChangeKind::Updated {
                        key: vec![named("id", Some("1"))],
                        cells: vec![
                            ReplacedCell {
                                column: "id".to_owned(),
                                old: Some("1".to_owned()),
                                new: Some("10".to_owned()),
                            },
                            ReplacedCell {
                                column: "name".to_owned(),
                                old: Some("ada".to_owned()),
                                new: None,
                            },
                        ],
                    },
                    revert: Some(
                        r#"UPDATE "public"."people" SET "id" = '1', "name" = 'ada' WHERE "id" = '10'"#
                            .to_owned()
                    ),
                },
                RowChange {
                    table,
                    kind: RowChangeKind::Inserted {
                        values: vec![named("name", Some("cy"))],
                    },
                    revert: None,
                },
            ]
        );
    }

    #[test]
    fn writes_mysql_updates_with_backticks_and_doubled_backslashes() {
        let columns = [column("id"), column("name")];
        let target = editable(table(&["id", "name"], &["id"]), &columns);
        let rows = vec![cells(&[Some("1"), Some("ada")])];
        let mut pending = PendingEdits::default();
        pending.set(&rows, RowRef::Loaded(0), 1, Some(r"C:\ada's".to_owned()));

        assert_eq!(
            sql(pending.statements(&target, &columns, &rows, Dialect::Mysql)),
            [r"UPDATE `public`.`people` SET `name` = 'C:\\ada''s' WHERE `id` = '1'"]
        );
    }

    #[test]
    fn writes_sql_server_updates_with_brackets_and_unicode_literals() {
        let columns = [column("id"), column("name")];
        let target = editable(table(&["id", "name"], &["id"]), &columns);
        let rows = vec![cells(&[Some("1"), Some("ada")])];
        let mut pending = PendingEdits::default();
        pending.set(&rows, RowRef::Loaded(0), 1, Some("Zoë's]".to_owned()));

        assert_eq!(
            sql(pending.statements(&target, &columns, &rows, Dialect::SqlServer)),
            ["UPDATE [public].[people] SET [name] = N'Zoë''s]' WHERE [id] = N'1'"]
        );
    }

    #[test]
    fn writes_one_update_per_changed_row_keyed_by_the_loaded_key() {
        let columns = [column("id"), column("name"), column("note")];
        let target = editable(table(&["id", "name", "note"], &["id"]), &columns);
        let mut rows = vec![
            cells(&[Some("1"), Some("ada"), None]),
            cells(&[Some("2"), Some("bob"), Some("x")]),
        ];
        let mut pending = PendingEdits::default();
        pending.set(&rows, RowRef::Loaded(1), 2, None);
        pending.set(&rows, RowRef::Loaded(1), 1, Some("Bob's".to_owned()));
        pending.set(&rows, RowRef::Loaded(0), 1, Some("changed".to_owned()));
        pending.set(&rows, RowRef::Loaded(0), 1, Some("ada".to_owned()));
        assert_eq!(
            pending.changed_cells(),
            2,
            "setting a value back drops the change"
        );

        assert_eq!(
            sql(pending.statements(&target, &columns, &rows, Dialect::Postgres)),
            [r#"UPDATE "public"."people" SET "name" = 'Bob''s', "note" = NULL WHERE "id" = '2'"#]
        );
        pending.apply_to(&mut rows);
        assert!(pending.is_empty());
        assert_eq!(rows[1], cells(&[Some("2"), Some("Bob's"), None]));
    }

    /// Deletes first, then updates, then inserts, so a key a deleted row frees can be taken.
    /// A deleted row's edits are dropped, and an added row with nothing typed in gets every
    /// default.
    #[test]
    fn deletes_then_updates_then_inserts() {
        let columns = [column("id"), column("name")];
        let target = editable(table(&["id", "name"], &["id"]), &columns);
        let mut rows = vec![
            cells(&[Some("1"), Some("ada")]),
            cells(&[Some("2"), Some("bob")]),
            cells(&[Some("3"), Some("cy")]),
        ];
        let mut pending = PendingEdits::default();
        pending.add_row();
        pending.set(&rows, RowRef::New(0), 0, Some("2".to_owned()));
        pending.set(&rows, RowRef::New(0), 1, Some("Bo".to_owned()));
        pending.add_row();
        pending.set(&rows, RowRef::Loaded(0), 1, Some("Ada".to_owned()));
        pending.set(&rows, RowRef::Loaded(1), 1, Some("dropped".to_owned()));
        pending.delete(&[RowRef::Loaded(1)]);
        assert!(pending.is_deleted(RowRef::Loaded(1)));
        assert_eq!(
            pending.count(),
            ChangeCount {
                inserted: 2,
                updated: 1,
                deleted: 1
            }
        );

        let statements: Vec<(RowRef, String)> = pending
            .statements(&target, &columns, &rows, Dialect::Sqlite)
            .into_iter()
            .map(|(row, edit)| (row, edit.sql))
            .collect();
        assert_eq!(
            statements,
            [
                (
                    RowRef::Loaded(1),
                    r#"DELETE FROM "public"."people" WHERE "id" = '2'"#.to_owned()
                ),
                (
                    RowRef::Loaded(0),
                    r#"UPDATE "public"."people" SET "name" = 'Ada' WHERE "id" = '1'"#.to_owned()
                ),
                (
                    RowRef::New(0),
                    r#"INSERT INTO "public"."people" ("id", "name") VALUES ('2', 'Bo')"#.to_owned()
                ),
                (
                    RowRef::New(1),
                    r#"INSERT INTO "public"."people" DEFAULT VALUES"#.to_owned()
                ),
            ]
        );

        pending.apply_to(&mut rows);
        assert!(pending.is_empty());
        assert_eq!(
            rows,
            [
                cells(&[Some("1"), Some("Ada")]),
                cells(&[Some("3"), Some("cy")])
            ]
        );
    }

    #[test]
    fn drops_added_rows_and_restores_deleted_ones() {
        let rows = vec![cells(&[Some("1")])];
        let mut pending = PendingEdits::default();
        for _ in 0..3 {
            pending.add_row();
        }
        pending.set(&rows, RowRef::New(2), 0, Some("last".to_owned()));
        pending.delete(&[RowRef::New(0), RowRef::New(1), RowRef::Loaded(0)]);
        assert_eq!(pending.new_rows(), 1);
        assert_eq!(
            pending.get(RowRef::New(0), 0),
            Some(&Some("last".to_owned()))
        );

        pending.restore(0);
        assert!(!pending.is_deleted(RowRef::Loaded(0)));
    }

    #[test]
    fn describes_what_a_submit_does() {
        let count = |inserted, updated, deleted| ChangeCount {
            inserted,
            updated,
            deleted,
        };
        let cases = [
            (count(0, 1, 0), "updates 1 edited row"),
            (count(0, 2, 0), "updates 2 edited rows"),
            (count(1, 0, 3), "inserts 1 row and deletes 3 rows"),
            (
                count(1, 2, 1),
                "inserts 1 row, updates 2 edited rows and deletes 1 row",
            ),
        ];
        for (count, expected) in cases {
            assert_eq!(count.describe(), expected);
        }
    }
}
