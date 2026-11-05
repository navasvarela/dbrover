//! How a comparison reads as text: a table's one-line summary and the lines under it, for the
//! differences list, and the whole report `dbrover schema compare` prints.

use std::fmt::Display;

use crate::{
    Change, Comparison, ConstraintKind, Difference, FieldChange, ObjectKind, TableDifferences,
};

const COLUMN: &str = "column";
const VERBS: [&str; 3] = ["added", "removed", "changed"];

impl TableDifferences {
    /// What changed, counted by kind, columns first: "2 columns added, 1 column changed,
    /// 1 check removed".
    pub fn summary(&self) -> String {
        let items: Vec<(&str, &str)> = self
            .columns
            .iter()
            .map(|column| (COLUMN, verb(&column.change)))
            .chain(
                self.constraints
                    .iter()
                    .map(|constraint| (constraint.kind.noun(), verb(&constraint.change))),
            )
            .collect();
        let nouns = [
            COLUMN,
            ConstraintKind::PrimaryKey.noun(),
            ConstraintKind::Unique.noun(),
            ConstraintKind::ForeignKey.noun(),
            ConstraintKind::Check.noun(),
        ];
        nouns
            .into_iter()
            .flat_map(|noun| VERBS.into_iter().map(move |verb| (noun, verb)))
            .filter_map(|(noun, verb)| {
                let count = items.iter().filter(|item| **item == (noun, verb)).count();
                let plural = if count == 1 { "" } else { "s" };
                (count > 0).then(|| format!("{count} {noun}{plural} {verb}"))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl TableDifferences {
    /// The summary, then a line for each column and constraint that differs: `~ total: type:
    /// numeric(10,2) → numeric(12,2)`, `+ column note`, `− check orders_total_check`.
    pub fn details(&self) -> Vec<String> {
        let mut lines = vec![self.summary()];
        for column in &self.columns {
            match &column.change {
                Change::Differs(fields) => lines.extend(
                    fields
                        .iter()
                        .map(|field| format!("~ {}: {field}", column.name)),
                ),
                change => lines.push(format!("{} column {}", change.mark(), column.name)),
            }
        }
        for constraint in &self.constraints {
            lines.push(match &constraint.change {
                Change::Differs(change) => format!("~ {change}"),
                change => format!("{} {} {}", change.mark(), constraint.kind, constraint.name),
            });
        }
        lines
    }
}

/// Each field that differs, a line each.
pub fn field_details<F: Display + Copy + 'static>(fields: &[FieldChange<F>]) -> Vec<String> {
    fields.iter().map(ToString::to_string).collect()
}

/// What a difference of one kind says under its name: what differs, or which side has it.
fn lines_of<D>(difference: &Difference<D>, details: impl Fn(&D) -> Vec<String>) -> Vec<String> {
    match &difference.change {
        Change::Differs(differences) => details(differences),
        Change::OnlyLeft => vec!["only on the left".to_owned()],
        Change::OnlyRight => vec!["only on the right".to_owned()],
    }
}

impl Comparison {
    /// Each object that differs under its kind's heading, with its mark, and what differs in it
    /// indented under it: what `dbrover schema compare` prints. Empty when nothing differs.
    pub fn report(&self) -> String {
        let mut out = String::new();
        let mut section = |kind: ObjectKind, entries: Vec<(&str, &str, Vec<String>)>| {
            if entries.is_empty() {
                return;
            }
            out.push_str(kind.plural());
            out.push('\n');
            for (mark, name, lines) in entries {
                out.push_str(&format!("  {mark} {name}\n"));
                for line in lines {
                    out.push_str(&format!("      {line}\n"));
                }
            }
        };
        section(
            ObjectKind::Table,
            self.tables
                .iter()
                .map(|table| {
                    (
                        table.change.mark(),
                        table.name.as_str(),
                        lines_of(table, TableDifferences::details),
                    )
                })
                .collect(),
        );
        section(
            ObjectKind::Index,
            self.indexes
                .iter()
                .map(|index| {
                    (
                        index.change.mark(),
                        index.name.as_str(),
                        lines_of(index, |fields| field_details(fields)),
                    )
                })
                .collect(),
        );
        section(
            ObjectKind::View,
            self.views
                .iter()
                .map(|view| {
                    (
                        view.change.mark(),
                        view.name.as_str(),
                        lines_of(view, |fields| field_details(fields)),
                    )
                })
                .collect(),
        );
        out
    }
}

fn verb<D>(change: &Change<D>) -> &'static str {
    let [added, removed, changed] = VERBS;
    match change {
        Change::OnlyRight => added,
        Change::OnlyLeft => removed,
        Change::Differs(_) => changed,
    }
}
