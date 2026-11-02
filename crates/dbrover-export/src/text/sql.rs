//! SQL statements that put the rows into a table: an `INSERT` for every row or batch of rows,
//! or an `UPDATE` of each row by its primary key. Both are written by `dbrover_sql`'s
//! `RowSql`, as the grid's edits are.

use std::io::{self, Write};

use dbrover_core::{Cell, ResultShape, ValueKind};
use dbrover_sql::{ColumnValue, Dialect, RowSql, Table, TypedColumn};
use serde::{Deserialize, Serialize};

use super::TextFormat;
use crate::ExportColumn;

/// The most rows one `INSERT` writes: SQL Server's limit for a `VALUES` list, and plenty for
/// the other databases.
pub const MAX_ROWS_PER_INSERT: u16 = 1_000;

/// Why the SQL formats aren't offered for rows from a language without a SQL dialect.
const NO_DIALECT: &str = "Only for rows from a SQL database";
/// Why they aren't offered for documents, which come from no one table.
const DOCUMENTS: &str = "Not for documents, which have no table";

/// Where exported rows came from, as far as the formats need to know: the SQL formats, their
/// dialect and table; JSON and JSON Lines, whether they are documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSource {
    /// `None` for a language without a SQL dialect, whose rows the SQL formats can't write.
    pub dialect: Option<Dialect>,
    /// The table the rows were selected from, when they come from exactly one.
    pub table: Option<SourceTable>,
    /// Rows, or documents flattened into rows (ADR 0006).
    pub shape: ResultShape,
}

impl RowSource {
    /// The same rows with only the result's columns at `selected`, in that order: the table's
    /// columns among them, and its key if they hold all of it.
    pub fn select_columns(&self, selected: &[usize]) -> Self {
        let position = |column: &usize| selected.iter().position(|chosen| chosen == column);
        Self {
            dialect: self.dialect,
            shape: self.shape,
            table: self.table.as_ref().map(|table| SourceTable {
                name: table.name.clone(),
                columns: table
                    .columns
                    .as_ref()
                    .map(|columns| columns.iter().filter_map(position).collect()),
                key: table
                    .key
                    .as_ref()
                    .and_then(|key| key.iter().map(position).collect()),
            }),
        }
    }

    /// Rows DbRover doesn't know the table of.
    pub fn untabled(dialect: Dialect) -> Self {
        Self {
            dialect: Some(dialect),
            table: None,
            shape: ResultShape::Rows,
        }
    }

    /// Rows or documents from a language without a SQL dialect.
    pub fn without_dialect(shape: ResultShape) -> Self {
        Self {
            dialect: None,
            table: None,
            shape,
        }
    }

    /// The dialect, for the SQL formats, or why they can't write these rows.
    fn sql_dialect(&self) -> Result<Dialect, &'static str> {
        match self.shape {
            ResultShape::Documents => Err(DOCUMENTS),
            ResultShape::Rows => self.dialect.ok_or(NO_DIALECT),
        }
    }
}

/// A table's name, with its schema when known; without one, the database finds the table by
/// its search path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlTable {
    pub schema: Option<String>,
    pub name: String,
}

impl std::fmt::Display for SqlTable {
    /// The name as people read it: `schema.name`, or the name alone.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.schema {
            Some(schema) => write!(f, "{schema}.{}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

impl SqlTable {
    /// The name as `RowSql` takes it.
    pub fn as_table(&self) -> Table<'_> {
        Table {
            schema: self.schema.as_deref(),
            name: &self.name,
        }
    }
}

/// The table rows were selected from, or that the user named, and how the result's columns
/// map onto it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTable {
    pub name: SqlTable,
    /// The positions of the result's columns that are the table's, in result order, the others
    /// (computed values) being left out of the statements; `None` when every column is: a
    /// whole table's rows, or rows for a table the user named.
    pub columns: Option<Vec<usize>>,
    /// The positions of the result's columns holding the table's primary key, in key order;
    /// `None` when the result can't tell rows apart, without a key or without all of it.
    pub key: Option<Vec<usize>>,
}

/// How many rows one `INSERT` writes: from 1 to [`MAX_ROWS_PER_INSERT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "u16", into = "u16")]
pub struct RowsPerInsert(u16);

impl RowsPerInsert {
    pub const ONE: Self = Self(1);

    pub fn get(self) -> u16 {
        self.0
    }

    /// `rows`, brought within 1 to [`MAX_ROWS_PER_INSERT`].
    pub fn clamped(rows: u16) -> Self {
        Self(rows.clamp(1, MAX_ROWS_PER_INSERT))
    }
}

impl From<u16> for RowsPerInsert {
    fn from(rows: u16) -> Self {
        Self::clamped(rows)
    }
}

impl From<RowsPerInsert> for u16 {
    fn from(rows: RowsPerInsert) -> Self {
        rows.0
    }
}

impl Default for RowsPerInsert {
    fn default() -> Self {
        Self::ONE
    }
}

/// What the user chooses about `INSERT`s; the table comes from the rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InsertOptions {
    pub rows_per_statement: RowsPerInsert,
}

/// `INSERT`s into a table, with everything needed to write them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertSpec {
    pub dialect: Dialect,
    pub table: SourceTable,
    pub options: InsertOptions,
}

/// `UPDATE`s of a table's rows by their primary key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSpec {
    pub dialect: Dialect,
    pub table: SqlTable,
    /// The result's columns that are set, by position.
    pub set: Vec<usize>,
    /// The result's columns holding the primary key, by position, in key order.
    pub key: Vec<usize>,
}

impl UpdateSpec {
    /// The `UPDATE`s for rows from `source`.
    ///
    /// # Errors
    ///
    /// Why the rows can't be updated: they don't come from one table, the result lacks its
    /// primary key, or every column it has is part of the key.
    pub fn for_rows(source: &RowSource) -> Result<Self, &'static str> {
        let dialect = source.sql_dialect()?;
        let table = source
            .table
            .as_ref()
            .ok_or("Only for rows selected from one table, which the grid can edit")?;
        let key = table
            .key
            .clone()
            .ok_or("Only for rows the grid can edit: select the table's whole primary key")?;
        let columns = table
            .columns
            .as_ref()
            .ok_or("Only for rows the grid can edit, whose columns DbRover knows")?;
        let set: Vec<usize> = columns
            .iter()
            .copied()
            .filter(|column| !key.contains(column))
            .collect();
        if set.is_empty() {
            return Err("Every column selected is part of the primary key: nothing to update");
        }
        Ok(Self {
            dialect,
            table: table.name.clone(),
            set,
            key,
        })
    }
}

impl InsertSpec {
    /// The `INSERT`s for rows from `source`.
    ///
    /// # Errors
    ///
    /// Why not: the rows don't come from one table.
    pub fn for_rows(source: &RowSource, options: InsertOptions) -> Result<Self, &'static str> {
        let dialect = source.sql_dialect()?;
        let table = source
            .table
            .clone()
            .ok_or("Only for rows selected from one table, whose name DbRover knows")?;
        Ok(Self {
            dialect,
            table,
            options,
        })
    }
}

pub(crate) struct Insert {
    spec: InsertSpec,
    /// The table's columns, by name and kind, in the order the statements list them.
    columns: Vec<(String, ValueKind)>,
    /// Their positions among the rows' cells.
    positions: Vec<usize>,
    /// Rows waiting for a full batch, each with the table's columns only.
    batch: Vec<Vec<Cell>>,
}

impl Insert {
    pub(crate) fn new(spec: InsertSpec) -> Self {
        Self {
            spec,
            columns: Vec::new(),
            positions: Vec::new(),
            batch: Vec::new(),
        }
    }

    fn write_batch(&mut self, out: &mut dyn Write) -> io::Result<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let columns: Vec<TypedColumn<'_>> = self
            .columns
            .iter()
            .map(|(name, kind)| TypedColumn { name, kind: *kind })
            .collect();
        let sql = RowSql::new(self.spec.dialect, self.spec.table.name.as_table());
        writeln!(out, "{};", sql.insert_typed(&columns, &self.batch))?;
        self.batch.clear();
        Ok(())
    }
}

impl TextFormat for Insert {
    fn header(&mut self, _out: &mut dyn Write, columns: &[ExportColumn]) -> io::Result<()> {
        self.positions = match &self.spec.table.columns {
            Some(positions) => positions.clone(),
            None => (0..columns.len()).collect(),
        };
        self.columns = self
            .positions
            .iter()
            .filter_map(|&index| columns.get(index))
            .map(|column| (column.name.clone(), column.kind))
            .collect();
        Ok(())
    }

    fn row(&mut self, out: &mut dyn Write, row: &[Cell]) -> io::Result<()> {
        self.batch.push(pick(row, &self.positions));
        if self.batch.len() >= usize::from(self.spec.options.rows_per_statement.get()) {
            self.write_batch(out)?;
        }
        Ok(())
    }

    fn footer(&mut self, out: &mut dyn Write) -> io::Result<()> {
        self.write_batch(out)
    }
}

pub(crate) struct Update {
    spec: UpdateSpec,
    names: Vec<String>,
}

impl Update {
    pub(crate) fn new(spec: UpdateSpec) -> Self {
        Self {
            spec,
            names: Vec::new(),
        }
    }

    fn values<'a>(&'a self, row: &'a [Cell], columns: &[usize]) -> Vec<ColumnValue<'a>> {
        columns
            .iter()
            .filter_map(|&index| {
                Some(ColumnValue {
                    column: self.names.get(index)?,
                    value: row.get(index)?,
                })
            })
            .collect()
    }
}

impl TextFormat for Update {
    fn header(&mut self, _out: &mut dyn Write, columns: &[ExportColumn]) -> io::Result<()> {
        self.names = columns.iter().map(|column| column.name.clone()).collect();
        Ok(())
    }

    fn row(&mut self, out: &mut dyn Write, row: &[Cell]) -> io::Result<()> {
        let set = self.values(row, &self.spec.set);
        let key = self.values(row, &self.spec.key);
        let sql = RowSql::new(self.spec.dialect, self.spec.table.as_table());
        writeln!(out, "{};", sql.update(&set, &key))
    }

    fn footer(&mut self, _out: &mut dyn Write) -> io::Result<()> {
        Ok(())
    }
}

/// The cells of `row` at `columns`, in that order. A row shorter than its columns, which no
/// result has, gets `NULL`s.
fn pick(row: &[Cell], columns: &[usize]) -> Vec<Cell> {
    columns
        .iter()
        .map(|&index| row.get(index).cloned().flatten())
        .collect()
}
