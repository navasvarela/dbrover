//! An object's definition as SQL, for reading the two sides of a difference side by side.

use dbrover_sql::{DdlOperation, ViewSpec};

use crate::Schema;
use crate::script::create_index;

/// The kinds of object a schema is compared by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectKind {
    Table,
    Index,
    View,
}

impl ObjectKind {
    /// The heading the differences list groups them under.
    pub fn plural(self) -> &'static str {
        match self {
            Self::Table => "Tables",
            Self::Index => "Indexes",
            Self::View => "Views",
        }
    }
}

impl Schema {
    /// The statement that creates the object, as E-track's builder writes it, with its `;`.
    /// `None` if the schema has no such object.
    ///
    /// Objects are named without the schema, as their definitions name the schema's own
    /// objects, so the same object's definition reads the same from two schemas and only what
    /// really differs is marked when they are set side by side.
    pub fn definition(&self, kind: ObjectKind, name: &str) -> Option<String> {
        let operation = match kind {
            ObjectKind::Table => DdlOperation::CreateTable(self.tables.get(name)?.spec.clone()),
            ObjectKind::Index => {
                let index = self.indexes.get(name)?;
                let table = dbrover_sql::ObjectName::new(None, &index.table);
                return Some(format!(
                    "{};",
                    create_index(name, index, &table, self.dialect)
                ));
            }
            ObjectKind::View => {
                let view = self.views.get(name)?;
                DdlOperation::CreateView(ViewSpec {
                    name: dbrover_sql::ObjectName::new(None, name),
                    kind: view.kind,
                    query: view.query.clone(),
                    with_data: true,
                })
            }
        };
        Some(match operation.statements(self.dialect) {
            Ok(statements) => statements
                .iter()
                .map(|statement| format!("{statement};"))
                .collect::<Vec<_>>()
                .join("\n"),
            Err(error) => format!("-- {error}"),
        })
    }
}

/// One row of two texts set side by side: a line both have, or the lines where they differ,
/// paired up in order, with `None` where one side has fewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow<'a> {
    pub left: Option<&'a str>,
    pub right: Option<&'a str>,
    pub differs: bool,
}

/// `left` and `right` aligned line by line on their longest common run of lines.
///
/// A plain longest-common-subsequence table: definitions are tens of lines, so its
/// quadratic size doesn't matter, and nothing here needs a diff library.
pub fn side_by_side<'a>(left: &'a str, right: &'a str) -> Vec<DiffRow<'a>> {
    let left: Vec<&str> = left.lines().collect();
    let right: Vec<&str> = right.lines().collect();
    let (n, m) = (left.len(), right.len());
    let mut common = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            common[i][j] = if left[i] == right[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let mut rows = Vec::new();
    let mut pending: (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && left[i] == right[j] {
            flush(&mut rows, &mut pending);
            rows.push(DiffRow {
                left: Some(left[i]),
                right: Some(right[j]),
                differs: false,
            });
            i += 1;
            j += 1;
        } else if j < m && (i == n || common[i][j + 1] >= common[i + 1][j]) {
            pending.1.push(right[j]);
            j += 1;
        } else {
            pending.0.push(left[i]);
            i += 1;
        }
    }
    flush(&mut rows, &mut pending);
    rows
}

fn flush<'a>(rows: &mut Vec<DiffRow<'a>>, pending: &mut (Vec<&'a str>, Vec<&'a str>)) {
    let lefts = std::mem::take(&mut pending.0);
    let rights = std::mem::take(&mut pending.1);
    let count = lefts.len().max(rights.len());
    for index in 0..count {
        rows.push(DiffRow {
            left: lefts.get(index).copied(),
            right: rights.get(index).copied(),
            differs: true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows<'a>(left: &'a str, right: &'a str) -> Vec<(Option<&'a str>, Option<&'a str>, bool)> {
        side_by_side(left, right)
            .into_iter()
            .map(|row| (row.left, row.right, row.differs))
            .collect()
    }

    #[test]
    fn aligns_what_both_have_and_pairs_what_differs() {
        assert_eq!(
            rows("a\nb\nc\nd", "a\nB\nc\nd\ne"),
            [
                (Some("a"), Some("a"), false),
                (Some("b"), Some("B"), true),
                (Some("c"), Some("c"), false),
                (Some("d"), Some("d"), false),
                (None, Some("e"), true),
            ]
        );
    }

    #[test]
    fn one_side_empty() {
        assert_eq!(
            rows("", "x\ny"),
            [(None, Some("x"), true), (None, Some("y"), true)]
        );
        assert!(rows("", "").is_empty());
    }
}
