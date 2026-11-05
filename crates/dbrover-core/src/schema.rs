//! A whole schema as a driver reads it, for comparing two schemas and saving a snapshot of one.

use crate::{TableConstraints, TableDescription};

/// The tables, indexes and views of one schema, as its catalog describes them.
///
/// Names inside definitions (a view's query, a default, a check, a column's type) name the
/// schema's own objects without the schema and other schemas' objects with theirs, so the same
/// object described in two schemas reads the same. Foreign keys keep their target's schema as
/// the catalog has it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaDescription {
    pub schema: String,
    pub tables: Vec<SchemaTable>,
    /// Indexes the schema's tables and materialized views have, apart from those that hold up
    /// a primary key or unique constraint, which their table's constraints describe.
    pub indexes: Vec<IndexDescription>,
    pub views: Vec<ViewDescription>,
}

/// A table of a described schema, as Modify table loads one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaTable {
    pub description: TableDescription,
    pub constraints: TableConstraints,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDescription {
    pub name: String,
    /// The table or materialized view it is on, in the same schema.
    pub table: String,
    pub unique: bool,
    /// The access method as the database names it, such as `btree` or `gin`.
    pub method: String,
    /// The key, in order: each a column's name, or an expression as the database prints it.
    pub key: Vec<String>,
    /// Columns stored in the index without being part of its key (`INCLUDE`).
    pub include: Vec<String>,
    /// A partial index's condition, without `WHERE`.
    pub predicate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewDescription {
    pub name: String,
    pub materialized: bool,
    /// The query as the database prints it.
    pub query: String,
    /// The tables and views of the same schema the query reads, which must exist before the
    /// view is created and which it must be dropped before changing.
    pub reads: Vec<String>,
}
