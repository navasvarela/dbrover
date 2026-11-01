//! What the export dialog exports: a console's rows, a statement run straight to a file, or a
//! table's every row, and how a table's export is going.

use std::path::{Path, PathBuf};

use dbrover_core::{
    CatalogPath, DataSource, DataSourceId, Identifier, NodeKind, RelationName, ResultShape,
    TableDescription,
};
use dbrover_engine::{Command, RunId};
use dbrover_export::{ExportColumn, RowSource, SourceTable, SqlTable};
use dbrover_language::QueryLanguage;
use dbrover_sql::Dialect;
use dbrover_store::ConsoleKey;

use super::ExportPreview;
use crate::console::SubmittedRun;

/// What the dialog exports.
#[derive(Debug, Clone)]
pub enum ExportSubject {
    /// A console's result: its loaded rows, or every row by running it again.
    Grid(ConsoleKey),
    /// A statement from a console's editor, run straight into the file: Run to file.
    Statement {
        console: ConsoleKey,
        run: SubmittedRun,
    },
    /// Every row of a table or view, read on the explorer's connection.
    Table(TableTarget),
}

impl ExportSubject {
    pub fn heading(&self) -> String {
        match self {
            Self::Grid(_) => "Export rows".to_owned(),
            Self::Statement { .. } => "Run to file".to_owned(),
            Self::Table(target) => format!("Export {}", target.title),
        }
    }
}

/// A table or view to export, from the explorer.
#[derive(Debug, Clone)]
pub struct TableTarget {
    pub data_source: DataSourceId,
    pub table: SqlTable,
    /// As [`Command::DescribeTable`] names it, to tell its answer apart.
    pub relation: RelationName,
    /// The qualified name, as the dialog's heading shows it.
    pub title: String,
    pub dialect: Dialect,
    /// The data source's name, which templates are told.
    pub data_source_name: String,
}

impl TableTarget {
    /// The table or view at `path`, and the description to ask for: its columns and key are
    /// what the SQL formats and the preview need. `None` if the node doesn't hold rows.
    pub fn open(data_source: &DataSource, path: &CatalogPath) -> Option<(Self, Command)> {
        let node = path.last().filter(|node| {
            matches!(
                node.kind,
                NodeKind::Table | NodeKind::View | NodeKind::MaterializedView
            )
        })?;
        let dialect = QueryLanguage::from(data_source.config.driver()).dialect()?;
        let schema = path.name_of(NodeKind::Schema);
        let quoted = |name: &str| Identifier {
            name: name.to_owned(),
            quoted: true,
        };
        let relation = RelationName {
            schema: schema.map(quoted),
            name: quoted(&node.name),
        };
        let target = Self {
            data_source: data_source.id,
            table: SqlTable {
                schema: schema.map(str::to_owned),
                name: node.name.clone(),
            },
            title: path.qualified_name(dialect.quoting())?,
            relation: relation.clone(),
            dialect,
            data_source_name: data_source.name.clone(),
        };
        let describe = Command::DescribeTable {
            data_source: data_source.id,
            table: relation,
        };
        Some((target, describe))
    }

    /// What the dialog shows before the table is described: no columns yet, and the table
    /// to insert into without its key.
    pub fn preview(&self) -> ExportPreview {
        ExportPreview {
            columns: Vec::new(),
            sample: Vec::new(),
            document_sample: Vec::new(),
            loaded: 0,
            source: RowSource {
                dialect: Some(self.dialect),
                shape: ResultShape::Rows,
                table: Some(SourceTable {
                    name: self.sql_table(),
                    columns: None,
                    key: None,
                }),
            },
            context: super::export_context(
                &self.data_source_name,
                Some(&self.sql_table()),
                Vec::new(),
            ),
        }
    }

    /// The preview once described: the table's columns in table order, which is the order
    /// `SELECT *` returns them in, and its key among them.
    pub fn described(&self, description: &TableDescription) -> ExportPreview {
        let columns = description
            .columns
            .iter()
            .map(|column| ExportColumn {
                name: column.name.clone(),
                type_name: Some(column.data_type.clone()),
                kind: self.dialect.value_kind(Some(&column.data_type)),
            })
            .collect();
        let key = description
            .primary_key
            .iter()
            .map(|name| {
                description
                    .columns
                    .iter()
                    .position(|column| &column.name == name)
            })
            .collect::<Option<Vec<usize>>>()
            .filter(|key| !key.is_empty());
        ExportPreview {
            columns,
            source: RowSource {
                dialect: Some(self.dialect),
                shape: ResultShape::Rows,
                table: Some(SourceTable {
                    name: self.sql_table(),
                    columns: Some((0..description.columns.len()).collect()),
                    key,
                }),
            },
            ..self.preview()
        }
    }

    fn sql_table(&self) -> SqlTable {
        self.table.clone()
    }
}

/// How a table's export is going; the dialog stays open to show it, as the explorer has no
/// toolbar of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableProgress {
    Choosing,
    Exporting {
        run: RunId,
        path: PathBuf,
        rows: usize,
    },
    Done(Result<String, String>),
}

impl TableProgress {
    pub fn finished(path: &Path, result: &Result<usize, String>) -> Self {
        Self::Done(
            result
                .as_ref()
                .map(|rows| format!("Exported {rows} rows to {}", path.display()))
                .map_err(Clone::clone),
        )
    }
}
