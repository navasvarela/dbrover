//! Schema compare (W13) and schema snapshots (VC3), without a database or a window: a schema's
//! model, the comparison of two, the script that turns one into the other, and the snapshot
//! file. See `docs/schema-compare.md`.

mod compare;
mod model;
mod summary;

pub use compare::{
    Change, ColumnField, CompareError, Comparison, ConstraintDifference, ConstraintKind,
    Difference, FieldChange, IndexField, TableDifferences, ViewField, compare,
};
pub use model::{Index, Schema, Table, View};
