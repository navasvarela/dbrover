//! The script that turns the left schema into the right one (W13), to read and run by hand.
//!
//! Statements come in phases, so each finds what it needs and nothing it would trip over:
//! 1. drop views that go, change, or read a table or view about to change, dependents first;
//! 2. drop indexes that go or change;
//! 3. drop foreign keys that go or change, on every table, before any table is dropped;
//! 4. drop tables that go, without `CASCADE`;
//! 5. create new tables, without their foreign keys;
//! 6. change tables that differ, without adding foreign keys;
//! 7. add foreign keys, once every table they point at exists;
//! 8. create views, dependencies first;
//! 9. create indexes, after the views, since a materialized view can have them.
//!
//! Phases 3, 6 and 7 are E4's `AlterTable` run in stages, each from the table as the stage
//! before left it: "without the foreign keys that go", "as it ends, with only the foreign keys
//! it kept", and "as it ends". So the script has no `ALTER TABLE` writer of its own.

mod index;
mod order;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use dbrover_sql::{
    DdlError, DdlOperation, Dialect, ExistingTable, ObjectName, TableAlteration, TableSpec,
    ViewKind, ViewSpec, quote_name,
};

use crate::{
    Change, CompareError, Comparison, ConstraintKind, Schema, Table, TableDifferences, compare,
};

/// How the script's header names its two sides, such as "schema staging on shop-dev".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sides {
    pub left: String,
    pub right: String,
}

/// The script, and the differences it couldn't write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub sql: String,
    pub not_written: Vec<NotWritten>,
}

/// A difference the script leaves to the user, written in it as a comment where its
/// statements would be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotWritten {
    /// The table, index or view it is part of.
    pub object: String,
    /// What the change is and why it isn't written, as one sentence.
    pub reason: String,
}

/// The script that, run on the left schema, makes it match the right; `None` when they are
/// the same.
///
/// # Errors
///
/// [`CompareError::Engines`] when the schemas are of different engines.
pub fn script(
    left: &Schema,
    right: &Schema,
    sides: &Sides,
) -> Result<Option<Script>, CompareError> {
    let comparison = compare(left, right)?;
    if comparison.is_empty() {
        return Ok(None);
    }
    let mut writer = Writer::new(left, right, &comparison);
    writer.write();
    Ok(Some(writer.finish(sides)))
}

/// The statements of one object, with the comments that go before them.
#[derive(Default)]
struct Group {
    comments: Vec<String>,
    statements: Vec<String>,
}

struct Writer<'a> {
    left: &'a Schema,
    right: &'a Schema,
    comparison: &'a Comparison,
    dialect: Dialect,
    groups: Vec<Group>,
    not_written: Vec<NotWritten>,
    /// Views phase 1 drops.
    dropped_views: BTreeSet<&'a str>,
    /// Views phase 8 creates.
    created_views: BTreeSet<&'a str>,
    /// Each table that both sides have, as phase 3 leaves it: without the foreign keys that go.
    /// `None` for a table whose change phase 6 couldn't write, which phase 7 then leaves alone.
    staged: Vec<(&'a str, Option<TableSpec>)>,
}

impl<'a> Writer<'a> {
    fn new(left: &'a Schema, right: &'a Schema, comparison: &'a Comparison) -> Self {
        Self {
            left,
            right,
            comparison,
            dialect: left.dialect,
            groups: Vec::new(),
            not_written: Vec::new(),
            dropped_views: BTreeSet::new(),
            created_views: BTreeSet::new(),
            staged: Vec::new(),
        }
    }

    fn write(&mut self) {
        self.drop_views();
        self.drop_indexes();
        self.drop_foreign_keys();
        self.drop_tables();
        self.create_tables();
        self.change_tables();
        self.add_foreign_keys();
        self.create_views();
        self.create_indexes();
    }

    fn finish(self, sides: &Sides) -> Script {
        let mut lines = vec![
            format!(
                "-- Changes {} (left) to match {} (right).",
                sides.left, sides.right
            ),
            "-- Written by DbRover's schema compare. Read it before running it: nothing has run."
                .to_owned(),
            "-- Columns are matched by name, so a renamed column is dropped and added again: edit"
                .to_owned(),
            "-- the script to rename it instead, or its data is lost.".to_owned(),
            String::new(),
        ];
        let transactional = self.dialect.capabilities().transactional_ddl;
        if transactional {
            lines.push("BEGIN;".to_owned());
        }
        if self.dialect == Dialect::Postgres {
            lines.push(
                "-- The definitions below name the schema's own objects without the schema."
                    .to_owned(),
            );
            lines.push(format!(
                "SET LOCAL search_path TO {};",
                quote_name(self.dialect, &self.left.name)
            ));
        }
        for group in self.groups {
            lines.push(String::new());
            lines.extend(
                group
                    .comments
                    .into_iter()
                    .map(|comment| format!("-- {comment}")),
            );
            lines.extend(
                group
                    .statements
                    .into_iter()
                    .map(|statement| format!("{statement};")),
            );
        }
        if transactional {
            lines.push(String::new());
            lines.push("COMMIT;".to_owned());
        }
        let mut sql = lines.join("\n");
        sql.push('\n');
        Script {
            sql,
            not_written: self.not_written,
        }
    }

    fn name(&self, name: &str) -> ObjectName {
        ObjectName::new(Some(&self.left.name), name)
    }

    fn push(&mut self, group: Group) {
        if !group.comments.is_empty() || !group.statements.is_empty() {
            self.groups.push(group);
        }
    }

    /// Records a difference the script doesn't write, as a comment in `group`.
    fn not_written(&mut self, group: &mut Group, object: &str, reason: String) {
        group.comments.push(format!("Not written: {reason}"));
        self.not_written.push(NotWritten {
            object: object.to_owned(),
            reason,
        });
    }

    /// Writes `operation` into `group`, or a comment saying why it can't be.
    fn operation(&mut self, group: &mut Group, object: &str, operation: DdlOperation) -> bool {
        match operation.statements(self.dialect) {
            Ok(statements) => {
                group.statements.extend(statements);
                true
            }
            Err(DdlError::NothingChanged) => true,
            Err(error) => {
                self.not_written(group, object, format!("{object}: {error}"));
                false
            }
        }
    }

    fn tables_where(&self, wanted: impl Fn(&Change<TableDifferences>) -> bool) -> Vec<&'a str> {
        self.comparison
            .tables
            .iter()
            .filter(|table| wanted(&table.change))
            .map(|table| table.name.as_str())
            .collect()
    }

    /// Tables that go, or whose columns or keys change: the tables views and foreign keys
    /// can't stay attached to while the script changes them.
    fn disturbed_tables(&self) -> BTreeSet<&'a str> {
        self.comparison
            .tables
            .iter()
            .filter(|table| match &table.change {
                Change::OnlyLeft => true,
                Change::OnlyRight => false,
                Change::Differs(differences) => {
                    !differences.columns.is_empty()
                        || differences.constraints.iter().any(|constraint| {
                            matches!(
                                constraint.kind,
                                ConstraintKind::PrimaryKey | ConstraintKind::Unique
                            )
                        })
                }
            })
            .map(|table| table.name.as_str())
            .collect()
    }

    /// Phase 1: views that go or change, that read a table that goes or changes, or that
    /// read a view dropped for either reason.
    fn drop_views(&mut self) {
        let changing: BTreeSet<&str> = self
            .tables_where(|change| !matches!(change, Change::OnlyRight))
            .into_iter()
            .collect();
        let mut dropped: BTreeSet<&'a str> = self
            .comparison
            .views
            .iter()
            .filter(|view| !matches!(view.change, Change::OnlyRight))
            .filter_map(|view| self.left.views.get_key_value(&view.name))
            .map(|(name, _)| name.as_str())
            .collect();
        loop {
            let more: Vec<&'a str> = self
                .left
                .views
                .iter()
                .filter(|(name, _)| !dropped.contains(name.as_str()))
                .filter(|(_, view)| {
                    view.reads.iter().any(|read| {
                        changing.contains(read.as_str()) || dropped.contains(read.as_str())
                    })
                })
                .map(|(name, _)| name.as_str())
                .collect();
            if more.is_empty() {
                break;
            }
            dropped.extend(more);
        }
        let left = self.left;
        let mut order = order::dependencies_first(&dropped, |name| reads(left, name));
        order.reverse();
        for name in order {
            let Some(view) = left.views.get(name) else {
                continue;
            };
            let mut group = Group::default();
            if view.kind == ViewKind::Materialized {
                group.comments.push(format!(
                    "Drops materialized view {name} and the rows it stores."
                ));
            }
            let operation = DdlOperation::DropView {
                view: self.name(name),
                kind: view.kind,
                cascade: false,
            };
            self.operation(&mut group, name, operation);
            self.push(group);
        }
        self.dropped_views = dropped;
    }

    /// Phase 2: indexes that go or change, apart from those on a table or view that goes,
    /// which go with it.
    fn drop_indexes(&mut self) {
        let dropped_tables: BTreeSet<&str> = self
            .tables_where(|change| matches!(change, Change::OnlyLeft))
            .into_iter()
            .collect();
        for difference in &self.comparison.indexes {
            if matches!(difference.change, Change::OnlyRight) {
                continue;
            }
            let Some(index) = self.left.indexes.get(&difference.name) else {
                continue;
            };
            let table = index.table.as_str();
            if dropped_tables.contains(table) || self.dropped_views.contains(table) {
                continue;
            }
            let mut group = Group::default();
            let operation = DdlOperation::DropIndex {
                table: self.name(table),
                index: difference.name.clone(),
            };
            self.operation(&mut group, &difference.name, operation);
            self.push(group);
        }
    }

    /// Phase 3: on every table of the left, the foreign keys that go, change, or point at a
    /// table whose keys or columns change. Each table both sides have is staged for phase 6
    /// as this leaves it.
    fn drop_foreign_keys(&mut self) {
        let disturbed = self.disturbed_tables();
        let left = self.left;
        for (name, table) in &left.tables {
            let before = table.spec_in(&left.name);
            let mut after = before.clone();
            let right = self.right.tables.get(name);
            after.foreign_keys.retain(|key| {
                let kept_on_the_right =
                    right.is_some_and(|right| right.spec_in(&left.name).foreign_keys.contains(key));
                let target_steady = key.target.schema.as_deref() != Some(&left.name)
                    || !disturbed.contains(key.target.name.as_str());
                kept_on_the_right && target_steady
            });
            let mut group = Group::default();
            if after != before {
                let operation = alter(table, before, after.clone());
                self.operation(&mut group, name, operation);
            }
            self.push(group);
            if right.is_some() {
                self.staged.push((name.as_str(), Some(after)));
            }
        }
    }

    /// Phase 4.
    fn drop_tables(&mut self) {
        for name in self.tables_where(|change| matches!(change, Change::OnlyLeft)) {
            let mut group = Group::default();
            group
                .comments
                .push(format!("Drops table {name} and every row in it."));
            let operation = DdlOperation::DropTable {
                table: self.name(name),
                cascade: false,
            };
            self.operation(&mut group, name, operation);
            self.push(group);
        }
    }

    /// Phase 5: new tables, without their foreign keys, which phase 7 adds.
    fn create_tables(&mut self) {
        for name in self.tables_where(|change| matches!(change, Change::OnlyRight)) {
            let Some(table) = self.right.tables.get(name) else {
                continue;
            };
            let mut spec = table.spec_in(&self.left.name);
            spec.foreign_keys.clear();
            let mut group = Group::default();
            if self.operation(&mut group, name, DdlOperation::CreateTable(spec)) {
                let default_name = self.default_primary_key_name(name);
                self.rename_primary_key(&mut group, name, default_name, table);
            }
            self.push(group);
        }
    }

    /// Phase 6: each table that differs, from phase 3's stage to how it ends, keeping the
    /// foreign keys phase 3 kept. Changes E4 can't make in place are left as comments, and
    /// the rest of the table's change is still written.
    fn change_tables(&mut self) {
        let staged = std::mem::take(&mut self.staged);
        let mut done = Vec::new();
        for (name, before) in staged {
            let (Some(before), Some(left), Some(right)) = (
                before,
                self.left.tables.get(name),
                self.right.tables.get(name),
            ) else {
                continue;
            };
            let mut group = Group::default();
            self.comment_dropped_columns(&mut group, name);
            let mut after = right.spec_in(&self.left.name);
            after.foreign_keys.clone_from(&before.foreign_keys);
            self.keep_what_cant_change(&mut group, name, &before, &mut after);
            let written = before == after
                || self.operation(&mut group, name, alter(left, before, after.clone()));
            if written {
                let kept_name = if left.spec.primary_key.is_empty() {
                    self.default_primary_key_name(name)
                } else {
                    left.primary_key_name.clone()
                };
                self.rename_primary_key(&mut group, name, kept_name, right);
            }
            self.push(group);
            done.push((name, written.then_some(after)));
        }
        self.staged = done;
    }

    fn comment_dropped_columns(&self, group: &mut Group, table: &str) {
        let Some(differences) =
            self.comparison
                .tables
                .iter()
                .find_map(|difference| match &difference.change {
                    Change::Differs(differences) if difference.name == table => Some(differences),
                    _ => None,
                })
        else {
            return;
        };
        for column in &differences.columns {
            if matches!(column.change, Change::OnlyLeft) {
                group.comments.push(format!(
                    "Drops column {table}.{} and the values in it.",
                    column.name
                ));
            }
        }
    }

    /// Gives `after` the left's identity and serial types where they change, which no
    /// `ALTER TABLE` E4 writes can do, and says so.
    fn keep_what_cant_change(
        &mut self,
        group: &mut Group,
        table: &str,
        before: &TableSpec,
        after: &mut TableSpec,
    ) {
        for column in &mut after.columns {
            let Some(old) = before.columns.iter().find(|old| old.name == column.name) else {
                continue;
            };
            let object = format!("{table}.{}", column.name);
            if old.identity != column.identity {
                column.identity = old.identity;
                let reason = format!(
                    "column {object} {} an identity column, which can't be changed in place; \
                     change it by hand.",
                    if old.identity.is_some() {
                        "stops being"
                    } else {
                        "becomes"
                    }
                );
                self.not_written(group, table, reason);
            }
            let type_changed = old.data_type != column.data_type;
            if type_changed && (is_serial(&old.data_type) || is_serial(&column.data_type)) {
                let reason = format!(
                    "column {object} changes from {} to {}, and a serial type can't be \
                     changed in place; change it by hand.",
                    old.data_type, column.data_type
                );
                column.data_type.clone_from(&old.data_type);
                column.default.clone_from(&old.default);
                self.not_written(group, table, reason);
            }
        }
    }

    /// The name PostgreSQL gives a primary key added without one.
    fn default_primary_key_name(&self, table: &str) -> Option<String> {
        (self.dialect == Dialect::Postgres).then(|| format!("{table}_pkey"))
    }

    /// Renames the table's primary key from `current` to the right's name, where the
    /// statements before left it with another.
    fn rename_primary_key(
        &self,
        group: &mut Group,
        table: &str,
        current: Option<String>,
        right: &Table,
    ) {
        if self.dialect != Dialect::Postgres || right.spec.primary_key.is_empty() {
            return;
        }
        let (Some(current), Some(wanted)) = (current, &right.primary_key_name) else {
            return;
        };
        if &current != wanted {
            group.statements.push(format!(
                "ALTER TABLE {} RENAME CONSTRAINT {} TO {}",
                self.name(table).to_sql(self.dialect),
                quote_name(self.dialect, &current),
                quote_name(self.dialect, wanted)
            ));
        }
    }

    /// Phase 7: the right's foreign keys on new tables and on changed ones, from how phases 5
    /// and 6 left each.
    fn add_foreign_keys(&mut self) {
        let left_schema = self.left.name.clone();
        let new_tables = self.tables_where(|change| matches!(change, Change::OnlyRight));
        let mut stages: Vec<(&str, TableSpec)> = Vec::new();
        for name in new_tables {
            if let Some(table) = self.right.tables.get(name) {
                let mut spec = table.spec_in(&left_schema);
                spec.foreign_keys.clear();
                stages.push((name, spec));
            }
        }
        for (name, stage) in std::mem::take(&mut self.staged) {
            if let Some(stage) = stage {
                stages.push((name, stage));
            }
        }
        stages.sort_by_key(|(name, _)| *name);
        for (name, before) in stages {
            let Some(right) = self.right.tables.get(name) else {
                continue;
            };
            let after = right.spec_in(&left_schema);
            if after.foreign_keys == before.foreign_keys {
                continue;
            }
            let mut group = Group::default();
            self.operation(&mut group, name, alter(right, before, after));
            self.push(group);
        }
    }

    /// Phase 8: views that are new, change, or were dropped in phase 1, dependencies first.
    fn create_views(&mut self) {
        let right = self.right;
        let created: BTreeSet<&'a str> = right
            .views
            .keys()
            .map(String::as_str)
            .filter(|name| {
                self.dropped_views.contains(name)
                    || self.comparison.views.iter().any(|view| view.name == *name)
            })
            .collect();
        for name in order::dependencies_first(&created, |name| reads(right, name)) {
            let Some(view) = right.views.get(name) else {
                continue;
            };
            let spec = ViewSpec {
                name: self.name(name),
                kind: view.kind,
                query: view.query.clone(),
                with_data: true,
            };
            let mut group = Group::default();
            self.operation(&mut group, name, DdlOperation::CreateView(spec));
            self.push(group);
        }
        self.created_views = created;
    }

    /// Phase 9: indexes that are new or change, and those of materialized views phase 8
    /// created again.
    fn create_indexes(&mut self) {
        for (name, index) in &self.right.indexes {
            let differs = self
                .comparison
                .indexes
                .iter()
                .any(|difference| &difference.name == name);
            if !differs && !self.created_views.contains(index.table.as_str()) {
                continue;
            }
            let statement = index::create(name, index, &self.name(&index.table), self.dialect);
            self.push(Group {
                comments: Vec::new(),
                statements: vec![statement],
            });
        }
    }
}

/// `AlterTable` from `before` to `after`, keeping `table`'s primary key name.
fn alter(table: &Table, before: TableSpec, after: TableSpec) -> DdlOperation {
    let mut existing = ExistingTable::from(before);
    existing
        .primary_key_name
        .clone_from(&table.primary_key_name);
    DdlOperation::AlterTable(Box::new(TableAlteration {
        table: existing,
        after,
        renamed: Default::default(),
        conversions: Default::default(),
    }))
}

fn reads<'s>(schema: &'s Schema, view: &str) -> Vec<&'s str> {
    schema
        .views
        .get(view)
        .map(|view| view.reads.iter().map(String::as_str).collect())
        .unwrap_or_default()
}

fn is_serial(data_type: &str) -> bool {
    matches!(
        data_type,
        "smallserial" | "serial" | "bigserial" | "serial2" | "serial4" | "serial8"
    )
}
