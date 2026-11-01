//! `ALTER TABLE`: the statements that turn an existing table into a new definition, for
//! Modify table.
//!
//! The statements run in an order that works on every engine:
//! 1. drop the constraints that go or change, foreign keys first and the primary key last;
//! 2. drop columns;
//! 3. rename columns, after dropping, so a column can take a dropped one's name;
//! 4. add columns;
//! 5. change columns' types, defaults and nullability;
//! 6. add the new primary key, then unique keys, checks and foreign keys;
//! 7. rename the table, so every statement before names it as it was.

mod rebuild;

use std::collections::{BTreeMap, BTreeSet};

pub use rebuild::rebuild_refusal;

use super::table::{self, column_definition};
use super::{
    Check, ColumnSpec, DdlError, ExistingTable, ForeignKey, ObjectName, TableAlteration, TableSpec,
    UniqueKey, column_list, quote_name,
};
use crate::{Dialect, TableChange};

pub(super) fn alter(
    alteration: &TableAlteration,
    dialect: Dialect,
) -> Result<Vec<String>, DdlError> {
    let diff = Diff::new(alteration, dialect)?;
    let statements = match refuse_what_the_dialect_cant_do(&diff, dialect) {
        Ok(()) => Writer::new(&diff, dialect).statements()?,
        Err(_) if dialect.capabilities().rebuilds_tables => rebuild::statements(&diff, dialect)?,
        Err(refusal) => return Err(refusal),
    };
    if statements.is_empty() {
        return Err(DdlError::NothingChanged);
    }
    Ok(statements)
}

/// Whether the change rebuilds the table rather than altering it in place (SQLite, for what
/// its `ALTER TABLE` can't do). `false` for a change that can't be written at all.
pub(super) fn rebuilds(alteration: &TableAlteration, dialect: Dialect) -> bool {
    dialect.capabilities().rebuilds_tables
        && Diff::new(alteration, dialect)
            .is_ok_and(|diff| refuse_what_the_dialect_cant_do(&diff, dialect).is_err())
}

/// What changes between the table and its new definition, worked out once.
struct Diff<'a> {
    table: &'a ExistingTable,
    after: &'a TableSpec,
    conversions: &'a BTreeMap<String, String>,
    /// The table's columns that the new definition leaves out.
    dropped: Vec<&'a ColumnSpec>,
    /// Old name, new name, in the new definition's order.
    renamed: Vec<(&'a str, &'a str)>,
    added: Vec<&'a ColumnSpec>,
    /// Kept columns whose type, nullability or default changed.
    changed: Vec<ColumnChange<'a>>,
    primary_key_changed: bool,
    dropped_constraints: Vec<Constraint<'a>>,
    added_constraints: Vec<Constraint<'a>>,
    new_name: Option<&'a str>,
}

/// A column the table keeps, as it was and as it will be.
struct ColumnChange<'a> {
    before: &'a ColumnSpec,
    after: &'a ColumnSpec,
}

impl ColumnChange<'_> {
    fn type_changed(&self) -> bool {
        self.before.data_type.trim() != self.after.data_type.trim()
    }

    fn nullability_changed(&self) -> bool {
        self.before.nullable != self.after.nullable
    }

    fn default_changed(&self) -> bool {
        default_of(self.before) != default_of(self.after)
    }

    fn is_change(&self) -> bool {
        self.type_changed() || self.nullability_changed() || self.default_changed()
    }
}

fn default_of(column: &ColumnSpec) -> Option<&str> {
    column
        .default
        .as_deref()
        .map(str::trim)
        .filter(|default| !default.is_empty())
}

/// A unique key, foreign key or check, of the table or of its new definition. A check keeps
/// its position, from which ClickHouse names an unnamed one.
#[derive(Clone, Copy)]
enum Constraint<'a> {
    Unique(&'a UniqueKey),
    Foreign(&'a ForeignKey),
    Check(usize, &'a Check),
}

impl Constraint<'_> {
    fn name(&self) -> Option<&str> {
        let name = match self {
            Self::Unique(key) => key.name.as_deref(),
            Self::Foreign(key) => key.name.as_deref(),
            Self::Check(_, check) => check.name.as_deref(),
        };
        name.map(str::trim).filter(|name| !name.is_empty())
    }

    /// Foreign keys are dropped first, since they may lean on a unique key, and added last,
    /// once the keys they point at exist.
    fn drop_order(&self) -> u8 {
        match self {
            Self::Foreign(_) => 0,
            Self::Check(..) => 1,
            Self::Unique(_) => 2,
        }
    }

    fn noun(&self) -> &'static str {
        match self {
            Self::Unique(_) => "unique key",
            Self::Foreign(_) => "foreign key",
            Self::Check(..) => "check",
        }
    }
}

impl<'a> Diff<'a> {
    fn new(alteration: &'a TableAlteration, dialect: Dialect) -> Result<Self, DdlError> {
        let before = &alteration.table.spec;
        let after = &alteration.after;
        table::validate(after, dialect)?;
        if after.name.schema.is_some() && after.name.schema != before.name.schema {
            return Err(DdlError::CantAlter {
                what: "move a table to another schema",
                dialect,
            });
        }
        let origins = Origins::new(alteration)?;
        let mut renamed = Vec::new();
        let mut added = Vec::new();
        let mut changed = Vec::new();
        for column in &after.columns {
            let Some(old) = origins.origin(&column.name) else {
                added.push(column);
                continue;
            };
            let Some(previous) = before.columns.iter().find(|before| before.name == old) else {
                added.push(column);
                continue;
            };
            if previous.identity != column.identity {
                return Err(DdlError::CantAlter {
                    what: "change whether an existing column is numbered by the database",
                    dialect,
                });
            }
            if old != column.name {
                renamed.push((previous.name.as_str(), column.name.as_str()));
            }
            let change = ColumnChange {
                before: previous,
                after: column,
            };
            if change.is_change() {
                changed.push(change);
            }
        }
        let dropped = before
            .columns
            .iter()
            .filter(|column| !origins.is_kept(&column.name))
            .collect();
        let renaming = Renaming {
            origins: &origins,
            before,
            after,
        };
        let (dropped_constraints, added_constraints) = constraint_changes(&renaming);
        let new_name = (after.name.name != before.name.name).then_some(after.name.name.as_str());
        Ok(Self {
            table: &alteration.table,
            after,
            conversions: &alteration.conversions,
            dropped,
            renamed,
            added,
            changed,
            primary_key_changed: renaming.columns(&before.primary_key).as_ref()
                != Some(&after.primary_key),
            dropped_constraints,
            added_constraints,
            new_name,
        })
    }

    fn before(&self) -> &'a TableSpec {
        &self.table.spec
    }

    /// The kinds of change it makes, each once.
    fn kinds(&self) -> BTreeSet<TableChange> {
        let mut kinds = BTreeSet::new();
        let mut add = |kind, needed: bool| {
            if needed {
                kinds.insert(kind);
            }
        };
        add(TableChange::RenameTable, self.new_name.is_some());
        add(TableChange::RenameColumn, !self.renamed.is_empty());
        add(TableChange::AddColumn, !self.added.is_empty());
        add(TableChange::DropColumn, !self.dropped.is_empty());
        for change in &self.changed {
            add(TableChange::ChangeColumnType, change.type_changed());
            add(TableChange::ChangeNullability, change.nullability_changed());
            add(TableChange::ChangeDefault, change.default_changed());
        }
        add(TableChange::ChangePrimaryKey, self.primary_key_changed);
        add(
            TableChange::DropConstraint,
            !self.dropped_constraints.is_empty(),
        );
        add(
            TableChange::AddConstraint,
            !self.added_constraints.is_empty(),
        );
        kinds
    }
}

/// Where each column of the new definition comes from.
struct Origins<'a> {
    /// New name → old name, for every column the table keeps.
    old_names: BTreeMap<&'a str, &'a str>,
    /// Old name → new name, the other way round.
    new_names: BTreeMap<&'a str, &'a str>,
}

impl<'a> Origins<'a> {
    /// A column of the new definition keeps a column of the table when it is renamed from
    /// it, or has its name and that column wasn't renamed to something else.
    fn new(alteration: &'a TableAlteration) -> Result<Self, DdlError> {
        let before = &alteration.table.spec;
        let after = &alteration.after;
        let has = |spec: &TableSpec, name: &str| spec.columns.iter().any(|c| c.name == name);
        let mut renamed: BTreeMap<&str, &str> = BTreeMap::new();
        for (new, old) in &alteration.renamed {
            if new == old || !has(after, new) {
                continue;
            }
            if !has(before, old) {
                return Err(DdlError::UnknownColumn {
                    column: old.clone(),
                    used_in: "rename",
                });
            }
            if renamed.values().any(|taken| taken == old) {
                return Err(DdlError::RenamedTwice(old.clone()));
            }
            renamed.insert(new, old);
        }
        let mut old_names = BTreeMap::new();
        for column in &after.columns {
            let name = column.name.as_str();
            let old = match renamed.get(name) {
                Some(old) => Some(*old),
                None if renamed.values().any(|old| *old == name) => None,
                None => before
                    .columns
                    .iter()
                    .find(|column| column.name == name)
                    .map(|column| column.name.as_str()),
            };
            if let Some(old) = old {
                old_names.insert(name, old);
            }
        }
        let new_names = old_names.iter().map(|(new, old)| (*old, *new)).collect();
        Ok(Self {
            old_names,
            new_names,
        })
    }

    fn origin(&self, name: &str) -> Option<&'a str> {
        self.old_names.get(name).copied()
    }

    fn is_kept(&self, old: &str) -> bool {
        self.new_names.contains_key(old)
    }
}

/// The table's names in terms of the new definition's, to compare its constraints with the
/// new ones.
struct Renaming<'r, 'a> {
    origins: &'r Origins<'a>,
    before: &'a TableSpec,
    after: &'a TableSpec,
}

impl Renaming<'_, '_> {
    /// The columns' new names; `None` if one of them is dropped.
    fn columns(&self, columns: &[String]) -> Option<Vec<String>> {
        columns
            .iter()
            .map(|old| {
                self.origins
                    .new_names
                    .get(old.as_str())
                    .map(|new| (*new).to_owned())
            })
            .collect()
    }

    fn unique_kept(&self, before: &UniqueKey, after: &UniqueKey) -> bool {
        same_name(before.name.as_deref(), after.name.as_deref())
            && self.columns(&before.columns).as_ref() == Some(&after.columns)
    }

    fn foreign_kept(&self, before: &ForeignKey, after: &ForeignKey) -> bool {
        let to_itself = same_table(&before.target, &self.before.name);
        let target_kept = if to_itself {
            same_table(&after.target, &self.after.name)
                && self.columns(&before.target_columns).as_ref() == Some(&after.target_columns)
        } else {
            same_table(&before.target, &after.target)
                && before.target_columns == after.target_columns
        };
        same_name(before.name.as_deref(), after.name.as_deref())
            && self.columns(&before.columns).as_ref() == Some(&after.columns)
            && target_kept
            && before.on_delete == after.on_delete
            && before.on_update == after.on_update
    }

    fn check_kept(before: &Check, after: &Check) -> bool {
        same_name(before.name.as_deref(), after.name.as_deref())
            && before.expression.trim() == after.expression.trim()
    }
}

/// A constraint keeps its name when the new definition gives it none: the database named it.
fn same_name(before: Option<&str>, after: Option<&str>) -> bool {
    match after.map(str::trim).filter(|name| !name.is_empty()) {
        None => true,
        Some(after) => before.map(str::trim) == Some(after),
    }
}

/// Whether two names are the same table, one of them perhaps without its schema.
fn same_table(left: &ObjectName, right: &ObjectName) -> bool {
    left.name == right.name
        && (left.schema.is_none() || right.schema.is_none() || left.schema == right.schema)
}

/// The table's constraints the new definition drops or changes, and the new definition's that
/// the table doesn't have yet. A changed constraint is dropped and added again: a constraint
/// holds no data.
fn constraint_changes<'a>(
    renaming: &Renaming<'_, 'a>,
) -> (Vec<Constraint<'a>>, Vec<Constraint<'a>>) {
    let before = renaming.before;
    let after = renaming.after;
    let old: Vec<Constraint<'a>> = before
        .unique_keys
        .iter()
        .map(Constraint::Unique)
        .chain(before.foreign_keys.iter().map(Constraint::Foreign))
        .chain(
            before
                .checks
                .iter()
                .enumerate()
                .map(|(i, c)| Constraint::Check(i, c)),
        )
        .collect();
    let new: Vec<Constraint<'a>> = after
        .unique_keys
        .iter()
        .map(Constraint::Unique)
        .chain(
            after
                .checks
                .iter()
                .enumerate()
                .map(|(i, c)| Constraint::Check(i, c)),
        )
        .chain(after.foreign_keys.iter().map(Constraint::Foreign))
        .collect();
    let kept = |old: &Constraint<'_>, new: &Constraint<'_>| match (old, new) {
        (Constraint::Unique(old), Constraint::Unique(new)) => renaming.unique_kept(old, new),
        (Constraint::Foreign(old), Constraint::Foreign(new)) => renaming.foreign_kept(old, new),
        (Constraint::Check(_, old), Constraint::Check(_, new)) => Renaming::check_kept(old, new),
        _ => false,
    };
    let mut matched = vec![false; new.len()];
    let mut dropped = Vec::new();
    for constraint in &old {
        let found = new
            .iter()
            .enumerate()
            .position(|(index, candidate)| !matched[index] && kept(constraint, candidate));
        match found {
            Some(index) => matched[index] = true,
            None => dropped.push(*constraint),
        }
    }
    dropped.sort_by_key(Constraint::drop_order);
    let added = new
        .into_iter()
        .zip(matched)
        .filter_map(|(constraint, matched)| (!matched).then_some(constraint))
        .collect();
    (dropped, added)
}

/// Refuses, with the reason, the first change the dialect can't make in place.
fn refuse_what_the_dialect_cant_do(diff: &Diff<'_>, dialect: Dialect) -> Result<(), DdlError> {
    let capabilities = dialect.capabilities();
    if let Some(kind) = diff
        .kinds()
        .into_iter()
        .find(|kind| !capabilities.changes_in_place(*kind))
    {
        return Err(DdlError::CantAlter {
            what: kind.refusal(),
            dialect,
        });
    }
    if capabilities.restricted_add_column
        && diff
            .added
            .iter()
            .any(|column| !adds_in_place(column, diff.after))
    {
        return Err(DdlError::CantAlter {
            what: "add a column that is in the primary key, numbered, or NOT NULL without a \
                   default, to an existing table",
            dialect,
        });
    }
    Ok(())
}

/// Whether SQLite's `ADD COLUMN` takes the column: outside the key, and taking `NULL` or
/// having a default for the rows already there.
fn adds_in_place(column: &ColumnSpec, after: &TableSpec) -> bool {
    !after.primary_key.contains(&column.name)
        && column.identity.is_none()
        && (column.nullable || default_of(column).is_some())
}

/// Writes the statements, one per step and per dialect.
struct Writer<'d, 'a> {
    diff: &'d Diff<'a>,
    dialect: Dialect,
    /// The table as statements name it until the last one renames it.
    table: String,
}

impl<'d, 'a> Writer<'d, 'a> {
    fn new(diff: &'d Diff<'a>, dialect: Dialect) -> Self {
        Self {
            diff,
            dialect,
            table: diff.before().name.to_sql(dialect),
        }
    }

    fn statements(&self) -> Result<Vec<String>, DdlError> {
        let diff = self.diff;
        let mut statements = Vec::new();
        for constraint in &diff.dropped_constraints {
            statements.push(self.drop_constraint(constraint)?);
        }
        if diff.primary_key_changed && !diff.before().primary_key.is_empty() {
            statements.push(self.drop_primary_key()?);
        }
        for column in &diff.dropped {
            statements.extend(self.drop_column(column));
        }
        for (old, new) in &diff.renamed {
            statements.push(self.rename_column(old, new));
        }
        for column in &diff.added {
            statements.extend(self.add_column(column));
        }
        for change in &diff.changed {
            statements.extend(self.change_column(change));
        }
        if diff.primary_key_changed && !diff.after.primary_key.is_empty() {
            statements.push(self.add_primary_key());
        }
        for constraint in &diff.added_constraints {
            statements.push(self.add_constraint(constraint));
        }
        if let Some(new_name) = diff.new_name {
            statements.push(super::rename(
                "TABLE",
                &diff.before().name,
                new_name,
                self.dialect,
            ));
        }
        Ok(statements)
    }

    fn alter(&self, action: &str) -> String {
        format!("ALTER TABLE {} {action}", self.table)
    }

    fn name(&self, name: &str) -> String {
        quote_name(self.dialect, name)
    }

    fn drop_constraint(&self, constraint: &Constraint<'_>) -> Result<String, DdlError> {
        let name = constraint
            .name()
            .ok_or(DdlError::UnnamedConstraint(constraint.noun()))?;
        let name = self.name(name);
        Ok(match (self.dialect, constraint) {
            (Dialect::Mysql, Constraint::Unique(_)) => self.alter(&format!("DROP INDEX {name}")),
            (Dialect::Mysql, Constraint::Foreign(_)) => {
                self.alter(&format!("DROP FOREIGN KEY {name}"))
            }
            _ => self.alter(&format!("DROP CONSTRAINT {name}")),
        })
    }

    fn drop_primary_key(&self) -> Result<String, DdlError> {
        if self.dialect == Dialect::Mysql {
            return Ok(self.alter("DROP PRIMARY KEY"));
        }
        let name = self
            .diff
            .table
            .primary_key_name
            .as_deref()
            .ok_or(DdlError::UnnamedConstraint("primary key"))?;
        Ok(self.alter(&format!("DROP CONSTRAINT {}", self.name(name))))
    }

    fn add_primary_key(&self) -> String {
        let columns = column_list(self.dialect, &self.diff.after.primary_key);
        let name = match (&self.diff.table.primary_key_name, self.dialect) {
            (Some(name), Dialect::Postgres | Dialect::SqlServer) => {
                format!("CONSTRAINT {} ", self.name(name))
            }
            _ => String::new(),
        };
        self.alter(&format!("ADD {name}PRIMARY KEY {columns}"))
    }

    fn add_constraint(&self, constraint: &Constraint<'_>) -> String {
        let definition = match constraint {
            Constraint::Unique(key) => table::unique_key(key, self.dialect),
            Constraint::Foreign(key) => table::foreign_key(key, self.dialect),
            Constraint::Check(index, check) => {
                table::check(self.diff.after, *index, check, self.dialect)
            }
        };
        self.alter(&format!("ADD {definition}"))
    }

    /// SQL Server keeps a column's default as a constraint, which the column can't be
    /// dropped or changed under.
    fn default_constraint(&self, column: &ColumnSpec) -> Option<&'a str> {
        if self.dialect != Dialect::SqlServer {
            return None;
        }
        self.diff
            .table
            .default_names
            .get(&column.name)
            .map(String::as_str)
    }

    fn drop_column(&self, column: &ColumnSpec) -> Vec<String> {
        let mut statements = Vec::new();
        if let Some(name) = self.default_constraint(column) {
            statements.push(self.alter(&format!("DROP CONSTRAINT {}", self.name(name))));
        }
        statements.push(self.alter(&format!("DROP COLUMN {}", self.name(&column.name))));
        statements
    }

    fn rename_column(&self, old: &str, new: &str) -> String {
        match self.dialect {
            Dialect::SqlServer => {
                let quoting = self.dialect.quoting();
                let column = format!("{}.{}", self.table, self.name(old));
                format!(
                    "EXEC sp_rename {}, {}, 'COLUMN'",
                    quoting.string_literal(&column),
                    quoting.string_literal(new)
                )
            }
            _ => self.alter(&format!(
                "RENAME COLUMN {} TO {}",
                self.name(old),
                self.name(new)
            )),
        }
    }

    fn add_column(&self, column: &ColumnSpec) -> Vec<String> {
        match self.dialect {
            Dialect::SqlServer => {
                vec![self.alter(&format!("ADD {}", column_definition(column, self.dialect)))]
            }
            // DuckDB's `ADD COLUMN` takes no constraint, so NOT NULL follows on its own.
            Dialect::DuckDb if !column.nullable => {
                let nullable = ColumnSpec {
                    nullable: true,
                    ..column.clone()
                };
                vec![
                    self.alter(&format!(
                        "ADD COLUMN {}",
                        column_definition(&nullable, self.dialect)
                    )),
                    self.alter(&format!(
                        "ALTER COLUMN {} SET NOT NULL",
                        self.name(&column.name)
                    )),
                ]
            }
            _ => vec![self.alter(&format!(
                "ADD COLUMN {}",
                column_definition(column, self.dialect)
            ))],
        }
    }

    fn change_column(&self, change: &ColumnChange<'_>) -> Vec<String> {
        match self.dialect {
            // `ddl_statements` refuses CQL's DDL before this.
            Dialect::Postgres | Dialect::DuckDb | Dialect::Sqlite | Dialect::Cassandra => {
                self.alter_column(change)
            }
            Dialect::Mysql => self.modify_mysql_column(change),
            Dialect::SqlServer => self.alter_sql_server_column(change),
            Dialect::ClickHouse => self.modify_clickhouse_column(change),
        }
    }

    /// PostgreSQL's and DuckDB's `ALTER COLUMN`, one statement per thing that changed. A
    /// default that goes is dropped before the type changes, and a new one set after, so
    /// the old default never has to convert to the new type.
    fn alter_column(&self, change: &ColumnChange<'_>) -> Vec<String> {
        let column = &change.after.name;
        let alter =
            |action: String| self.alter(&format!("ALTER COLUMN {} {action}", self.name(column)));
        let mut statements = Vec::new();
        let default = default_of(change.after);
        if change.default_changed() && default.is_none() {
            statements.push(alter("DROP DEFAULT".to_owned()));
        }
        if change.type_changed() {
            let using = self
                .diff
                .conversions
                .get(column)
                .map(|using| using.trim())
                .filter(|using| !using.is_empty())
                .map(|using| format!(" USING {using}"))
                .unwrap_or_default();
            statements.push(alter(format!(
                "TYPE {}{using}",
                change.after.data_type.trim()
            )));
        }
        if let Some(default) = default.filter(|_| change.default_changed()) {
            statements.push(alter(format!("SET DEFAULT {default}")));
        }
        if change.nullability_changed() {
            let action = if change.after.nullable {
                "DROP NOT NULL"
            } else {
                "SET NOT NULL"
            };
            statements.push(alter(action.to_owned()));
        }
        statements
    }

    /// MySQL's `MODIFY COLUMN` restates the whole column, so it carries a new type or
    /// nullability; a new default alone is set with `ALTER COLUMN`, which leaves the rest of
    /// the column (its comment, its character set) as it is.
    fn modify_mysql_column(&self, change: &ColumnChange<'_>) -> Vec<String> {
        if change.type_changed() || change.nullability_changed() {
            return vec![self.alter(&format!(
                "MODIFY COLUMN {}",
                column_definition(change.after, self.dialect)
            ))];
        }
        let column = self.name(&change.after.name);
        vec![match default_of(change.after) {
            Some(default) => self.alter(&format!("ALTER COLUMN {column} SET DEFAULT {default}")),
            None => self.alter(&format!("ALTER COLUMN {column} DROP DEFAULT")),
        }]
    }

    /// SQL Server changes a column's type and nullability together, and its default is a
    /// constraint, which has to go while the column changes and comes back afterwards under
    /// the same name.
    fn alter_sql_server_column(&self, change: &ColumnChange<'_>) -> Vec<String> {
        let column = self.name(&change.after.name);
        let retyped = change.type_changed() || change.nullability_changed();
        let default_name = self.default_constraint(change.before);
        let mut statements = Vec::new();
        let dropped_default = default_name.filter(|_| retyped || change.default_changed());
        if let Some(name) = dropped_default {
            statements.push(self.alter(&format!("DROP CONSTRAINT {}", self.name(name))));
        }
        if retyped {
            let null = if change.after.nullable {
                "NULL"
            } else {
                "NOT NULL"
            };
            statements.push(self.alter(&format!(
                "ALTER COLUMN {column} {} {null}",
                change.after.data_type.trim()
            )));
        }
        let restored = dropped_default.is_some() || change.default_changed();
        if let Some(default) = default_of(change.after).filter(|_| restored) {
            let name = default_name
                .map(|name| format!("CONSTRAINT {} ", self.name(name)))
                .unwrap_or_default();
            statements.push(self.alter(&format!("ADD {name}DEFAULT {default} FOR {column}")));
        }
        statements
    }

    /// ClickHouse's `MODIFY COLUMN`: nullability is part of the type (`Nullable(…)`), so a
    /// new type or nullability restates the column, default included.
    fn modify_clickhouse_column(&self, change: &ColumnChange<'_>) -> Vec<String> {
        let column = self.name(&change.after.name);
        let mut statements = Vec::new();
        if change.type_changed() || change.nullability_changed() {
            statements.push(self.alter(&format!(
                "MODIFY COLUMN {}",
                column_definition(change.after, self.dialect)
            )));
        } else if let Some(default) = default_of(change.after) {
            statements.push(self.alter(&format!("MODIFY COLUMN {column} DEFAULT {default}")));
        }
        if change.default_changed() && default_of(change.after).is_none() {
            statements.push(self.alter(&format!("MODIFY COLUMN {column} REMOVE DEFAULT")));
        }
        statements
    }
}
