//! One-line summaries of how a table differs, for the differences list and the command line.

use crate::{Change, ConstraintKind, TableDifferences};

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

fn verb<D>(change: &Change<D>) -> &'static str {
    let [added, removed, changed] = VERBS;
    match change {
        Change::OnlyRight => added,
        Change::OnlyLeft => removed,
        Change::Differs(_) => changed,
    }
}
