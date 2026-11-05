//! What differs between two schemas: each kind of object matched by name.

use std::collections::BTreeMap;
use std::fmt;

use dbrover_core::IdentityGeneration;
use dbrover_sql::{ColumnSpec, Dialect, ForeignKey, ReferentialAction, ViewKind};

use crate::{Index, Schema, Table, View};

/// Why two schemas can't be compared.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompareError {
    #[error(
        "A {left} schema can't be compared with a {right} one.",
        left = .left.display_name(),
        right = .right.display_name()
    )]
    Engines { left: Dialect, right: Dialect },
}

/// Everything that differs between a left and a right schema, read as what changes going from
/// the left to the right: an object only on the right is added, one only on the left removed.
/// Objects that are the same on both sides aren't listed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comparison {
    pub tables: Vec<Difference<TableDifferences>>,
    pub indexes: Vec<Difference<Vec<FieldChange<IndexField>>>>,
    pub views: Vec<Difference<Vec<FieldChange<ViewField>>>>,
}

/// One named thing that isn't the same on both sides, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference<D> {
    pub name: String,
    pub change: Change<D>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change<D> {
    /// Only the left has it: going to the right removes it.
    OnlyLeft,
    /// Only the right has it: going to the right adds it.
    OnlyRight,
    /// Both have it, and it differs in `D`.
    Differs(D),
}

impl<D> Change<D> {
    /// The mark the differences list shows: `+` added, `−` removed, `~` changed.
    pub fn mark(&self) -> &'static str {
        match self {
            Self::OnlyLeft => "\u{2212}",
            Self::OnlyRight => "+",
            Self::Differs(_) => "~",
        }
    }
}

/// Something of an object, as each side has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldChange<F> {
    pub field: F,
    pub left: String,
    pub right: String,
}

/// How a table differs: its columns and its constraints, each matched by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableDifferences {
    pub columns: Vec<Difference<Vec<FieldChange<ColumnField>>>>,
    pub constraints: Vec<ConstraintDifference>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintDifference {
    pub kind: ConstraintKind,
    /// The constraint's name, or its definition where the database keeps no name.
    pub name: String,
    /// When both sides have it, its definition on each.
    pub change: Change<FieldChange<ConstraintKind>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConstraintKind {
    PrimaryKey,
    Unique,
    ForeignKey,
    Check,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnField {
    Type,
    Nullable,
    Default,
    Identity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexField {
    Table,
    Unique,
    Method,
    Key,
    Include,
    Predicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewField {
    Kind,
    Query,
}

/// Compares two schemas of the same engine.
///
/// # Errors
///
/// [`CompareError::Engines`] when the schemas are of different engines.
pub fn compare(left: &Schema, right: &Schema) -> Result<Comparison, CompareError> {
    if left.dialect != right.dialect {
        return Err(CompareError::Engines {
            left: left.dialect,
            right: right.dialect,
        });
    }
    Ok(Comparison {
        tables: match_by_name(&left.tables, &right.tables, compare_tables),
        indexes: match_by_name(&left.indexes, &right.indexes, compare_indexes),
        views: match_by_name(&left.views, &right.views, compare_views),
    })
}

impl Comparison {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty() && self.indexes.is_empty() && self.views.is_empty()
    }

    /// How many objects differ.
    pub fn len(&self) -> usize {
        self.tables.len() + self.indexes.len() + self.views.len()
    }
}

/// Pairs the entries of two maps by name, in name order, and keeps those that aren't the
/// same: `differ` says how two entries of the same name differ, `None` when they don't.
fn match_by_name<T, D>(
    left: &BTreeMap<String, T>,
    right: &BTreeMap<String, T>,
    differ: impl Fn(&T, &T) -> Option<D>,
) -> Vec<Difference<D>> {
    pair(left, right, differ)
        .into_iter()
        .map(|(name, paired)| Difference {
            name: name.clone(),
            change: paired.change(),
        })
        .collect()
}

/// An entry of one side or both, after pairing by key.
enum Paired<'a, T, D> {
    OnlyLeft(&'a T),
    OnlyRight(&'a T),
    Differs(&'a T, D),
}

impl<T, D> Paired<'_, T, D> {
    fn change(self) -> Change<D> {
        match self {
            Self::OnlyLeft(_) => Change::OnlyLeft,
            Self::OnlyRight(_) => Change::OnlyRight,
            Self::Differs(_, difference) => Change::Differs(difference),
        }
    }
}

fn pair<'a, K: Ord, T, D>(
    left: &'a BTreeMap<K, T>,
    right: &'a BTreeMap<K, T>,
    differ: impl Fn(&T, &T) -> Option<D>,
) -> Vec<(&'a K, Paired<'a, T, D>)> {
    let mut sides: BTreeMap<&K, (Option<&T>, Option<&T>)> = BTreeMap::new();
    for (key, item) in left {
        sides.entry(key).or_default().0 = Some(item);
    }
    for (key, item) in right {
        sides.entry(key).or_default().1 = Some(item);
    }
    sides
        .into_iter()
        .filter_map(|(key, sides)| {
            let paired = match sides {
                (Some(left), None) => Paired::OnlyLeft(left),
                (None, Some(right)) => Paired::OnlyRight(right),
                (Some(left), Some(right)) => Paired::Differs(left, differ(left, right)?),
                (None, None) => return None,
            };
            Some((key, paired))
        })
        .collect()
}

fn compare_tables(left: &Table, right: &Table) -> Option<TableDifferences> {
    let columns = match_by_name(
        &by_name(&left.spec.columns, |column| column.name.clone()),
        &by_name(&right.spec.columns, |column| column.name.clone()),
        |left, right| non_empty(compare_columns(left, right)),
    );
    let constraints = compare_constraints(left, right);
    (!columns.is_empty() || !constraints.is_empty()).then_some(TableDifferences {
        columns,
        constraints,
    })
}

fn compare_columns(left: &ColumnSpec, right: &ColumnSpec) -> Vec<FieldChange<ColumnField>> {
    [
        (
            ColumnField::Type,
            left.data_type.clone(),
            right.data_type.clone(),
        ),
        (
            ColumnField::Nullable,
            yes_no(left.nullable),
            yes_no(right.nullable),
        ),
        (
            ColumnField::Default,
            or_none(&left.default),
            or_none(&right.default),
        ),
        (ColumnField::Identity, identity(left), identity(right)),
    ]
    .into_iter()
    .filter_map(|(field, left, right)| changed(field, left, right))
    .collect()
}

fn compare_constraints(left: &Table, right: &Table) -> Vec<ConstraintDifference> {
    let left = constraints_of(left);
    let right = constraints_of(right);
    pair(&left, &right, |left, right| {
        (left.definition != right.definition).then(|| FieldChange {
            field: (),
            left: left.definition.clone(),
            right: right.definition.clone(),
        })
    })
    .into_iter()
    .map(|(&(kind, _), paired)| {
        let (Paired::OnlyLeft(constraint)
        | Paired::OnlyRight(constraint)
        | Paired::Differs(constraint, _)) = paired;
        let name = constraint.name.clone();
        let change = match paired.change() {
            Change::OnlyLeft => Change::OnlyLeft,
            Change::OnlyRight => Change::OnlyRight,
            Change::Differs(change) => Change::Differs(FieldChange {
                field: kind,
                left: change.left,
                right: change.right,
            }),
        };
        ConstraintDifference { kind, name, change }
    })
    .collect()
}

/// A constraint as compared: the name shown for it, and its definition on one line.
struct Constraint {
    name: String,
    definition: String,
}

/// A table's constraints, by kind and name. The primary key, one per table, is matched
/// whatever its name, and its name is part of its definition.
fn constraints_of(table: &Table) -> BTreeMap<(ConstraintKind, String), Constraint> {
    let spec = &table.spec;
    let mut constraints = BTreeMap::new();
    if !spec.primary_key.is_empty() {
        let key_columns = columns(&spec.primary_key);
        let (name, definition) = match &table.primary_key_name {
            Some(name) => (name.clone(), format!("{name} {key_columns}")),
            None => (ConstraintKind::PrimaryKey.to_string(), key_columns),
        };
        constraints.insert(
            (ConstraintKind::PrimaryKey, String::new()),
            Constraint { name, definition },
        );
    }
    let named = spec
        .unique_keys
        .iter()
        .map(|key| (ConstraintKind::Unique, &key.name, columns(&key.columns)))
        .chain(
            spec.foreign_keys
                .iter()
                .map(|key| (ConstraintKind::ForeignKey, &key.name, foreign_key(key))),
        )
        .chain(
            spec.checks
                .iter()
                .map(|check| (ConstraintKind::Check, &check.name, check.expression.clone())),
        );
    for (kind, name, definition) in named {
        let name = name.clone().unwrap_or_else(|| definition.clone());
        constraints.insert((kind, name.clone()), Constraint { name, definition });
    }
    constraints
}

fn compare_indexes(left: &Index, right: &Index) -> Option<Vec<FieldChange<IndexField>>> {
    non_empty(
        [
            (IndexField::Table, left.table.clone(), right.table.clone()),
            (
                IndexField::Unique,
                yes_no(left.unique),
                yes_no(right.unique),
            ),
            (
                IndexField::Method,
                left.method.clone(),
                right.method.clone(),
            ),
            (IndexField::Key, columns(&left.key), columns(&right.key)),
            (
                IndexField::Include,
                optional_columns(&left.include),
                optional_columns(&right.include),
            ),
            (
                IndexField::Predicate,
                or_none(&left.predicate),
                or_none(&right.predicate),
            ),
        ]
        .into_iter()
        .filter_map(|(field, left, right)| changed(field, left, right))
        .collect(),
    )
}

fn compare_views(left: &View, right: &View) -> Option<Vec<FieldChange<ViewField>>> {
    non_empty(
        [
            (ViewField::Kind, view_kind(left.kind), view_kind(right.kind)),
            (ViewField::Query, left.query.clone(), right.query.clone()),
        ]
        .into_iter()
        .filter_map(|(field, left, right)| changed(field, left, right))
        .collect(),
    )
}

fn changed<F>(field: F, left: String, right: String) -> Option<FieldChange<F>> {
    (left != right).then_some(FieldChange { field, left, right })
}

fn non_empty<T>(items: Vec<T>) -> Option<Vec<T>> {
    (!items.is_empty()).then_some(items)
}

fn by_name<T: Clone>(items: &[T], name: impl Fn(&T) -> String) -> BTreeMap<String, T> {
    items
        .iter()
        .map(|item| (name(item), item.clone()))
        .collect()
}

const NONE: &str = "none";

fn or_none(text: &Option<String>) -> String {
    text.clone().unwrap_or_else(|| NONE.to_owned())
}

fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.to_owned()
}

fn identity(column: &ColumnSpec) -> String {
    match column.identity {
        None => NONE.to_owned(),
        Some(identity) => {
            let generation = match identity.generation {
                IdentityGeneration::Always => "always",
                IdentityGeneration::ByDefault => "by default",
            };
            if identity.start == 1 && identity.increment == 1 {
                generation.to_owned()
            } else {
                format!(
                    "{generation}, from {} by {}",
                    identity.start, identity.increment
                )
            }
        }
    }
}

fn view_kind(kind: ViewKind) -> String {
    match kind {
        ViewKind::View => "view",
        ViewKind::Materialized => "materialized view",
    }
    .to_owned()
}

fn columns(names: &[String]) -> String {
    format!("({})", names.join(", "))
}

fn optional_columns(names: &[String]) -> String {
    if names.is_empty() {
        NONE.to_owned()
    } else {
        columns(names)
    }
}

fn foreign_key(key: &ForeignKey) -> String {
    let target = match &key.target.schema {
        Some(schema) => format!("{schema}.{}", key.target.name),
        None => key.target.name.clone(),
    };
    let mut definition = format!(
        "{} \u{2192} {target} {}",
        columns(&key.columns),
        columns(&key.target_columns)
    );
    for (event, action) in [("delete", key.on_delete), ("update", key.on_update)] {
        if action != ReferentialAction::NoAction {
            definition.push_str(&format!(" on {event} {}", action.sql().to_lowercase()));
        }
    }
    definition
}

impl ConstraintKind {
    pub fn noun(self) -> &'static str {
        match self {
            Self::PrimaryKey => "primary key",
            Self::Unique => "unique key",
            Self::ForeignKey => "foreign key",
            Self::Check => "check",
        }
    }
}

impl fmt::Display for ConstraintKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.noun())
    }
}

impl fmt::Display for ColumnField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Type => "type",
            Self::Nullable => "nullable",
            Self::Default => "default",
            Self::Identity => "identity",
        })
    }
}

impl fmt::Display for IndexField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Table => "table",
            Self::Unique => "unique",
            Self::Method => "method",
            Self::Key => "key",
            Self::Include => "include",
            Self::Predicate => "where",
        })
    }
}

impl fmt::Display for ViewField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Kind => "kind",
            Self::Query => "query",
        })
    }
}

/// `field: left → right`, or for a query, which runs over several lines, `query changed`.
impl<F: fmt::Display + Copy + 'static> fmt::Display for FieldChange<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.left.contains('\n') || self.right.contains('\n') {
            write!(f, "{} changed", self.field)
        } else {
            write!(f, "{}: {} \u{2192} {}", self.field, self.left, self.right)
        }
    }
}

#[cfg(test)]
mod tests;
