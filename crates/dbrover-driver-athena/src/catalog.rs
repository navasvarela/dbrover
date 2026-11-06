//! The explorer's tree, from Athena's own catalog API, which reads the Glue Data Catalog and
//! federated catalogs without running a query: catalogs (`ListDataCatalogs`), their databases
//! (`ListDatabases`), the tables and views of a database (`ListTableMetadata`), and a table's
//! columns and partition keys (`GetTableMetadata`). Every call is free; none scans data.
//!
//! Athena lists at most 50 of anything a request, so a level the explorer asks a thousand of
//! takes up to twenty requests, each continuing from the one before.

use std::collections::HashMap;

use aws_sdk_athena::Client;
use aws_sdk_athena::types::{Column, DataCatalogSummary, TableMetadata};
use dbrover_core::{
    CatalogNode, CatalogPath, ChildrenPage, ChildrenQuery, DriverError, NodeKind, PageToken,
};

use crate::errors::{self, Refused};

/// The most of anything Athena's list calls return at once, as its API reference sets it.
const MAX_LISTED: usize = 50;

/// The table type Glue gives a view.
const VIEW_TYPE: &str = "VIRTUAL_VIEW";

/// What a path names, by its shape: `[Database]` is a catalog, `[Database, Schema]` one of its
/// databases, and `[Database, Schema, Table | View]` a table or view.
enum Level<'a> {
    Catalogs,
    Databases {
        catalog: &'a str,
    },
    Tables {
        catalog: &'a str,
        database: &'a str,
    },
    Columns {
        catalog: &'a str,
        database: &'a str,
        table: &'a str,
    },
}

impl<'a> Level<'a> {
    fn of(path: &'a CatalogPath) -> Option<Self> {
        let names: Vec<(NodeKind, &str)> = path
            .segments()
            .iter()
            .map(|segment| (segment.kind, segment.name.as_str()))
            .collect();
        Some(match names.as_slice() {
            [] => Self::Catalogs,
            [(NodeKind::Database, catalog)] => Self::Databases { catalog },
            [(NodeKind::Database, catalog), (NodeKind::Schema, database)] => {
                Self::Tables { catalog, database }
            }
            [
                (NodeKind::Database, catalog),
                (NodeKind::Schema, database),
                (NodeKind::Table | NodeKind::View, table),
            ] => Self::Columns {
                catalog,
                database,
                table,
            },
            _ => return None,
        })
    }
}

/// Lists part of a level: what the explorer asks for, a thousand at a time.
pub(crate) struct Lister<'a> {
    pub client: &'a Client,
    /// The data source's own catalog, whose tables' definitions its queries can read.
    pub own_catalog: &'a str,
}

/// One answer of a list call: its nodes, and the token of the next.
struct Listed {
    nodes: Vec<CatalogNode>,
    next: Option<String>,
}

impl Lister<'_> {
    /// The nodes of `path`'s level that `query` asks for.
    ///
    /// # Errors
    ///
    /// [`DriverError::InvalidPath`] for a path this driver never lists, and
    /// [`DriverError::Connection`] when Athena refuses a request.
    pub(crate) async fn page(
        &self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        let level = Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
        if let Level::Columns {
            catalog,
            database,
            table,
        } = level
        {
            let nodes = self.columns(path, catalog, database, table).await?;
            return Ok(ChildrenPage::whole(nodes, query));
        }
        let limit = query.limit.unwrap_or(usize::MAX);
        let mut nodes = Vec::new();
        let mut token = query.after.as_ref().map(|token| token.as_str().to_owned());
        loop {
            let wanted = (limit - nodes.len()).min(MAX_LISTED);
            let listed = self.list(path, &level, query, token.take(), wanted).await?;
            nodes.extend(
                listed
                    .nodes
                    .into_iter()
                    .filter(|node| query.matches(&node.label)),
            );
            token = listed.next;
            if token.is_none() || nodes.len() >= limit {
                break;
            }
        }
        Ok(ChildrenPage {
            nodes,
            next: token.map(PageToken::new),
        })
    }

    async fn list(
        &self,
        path: &CatalogPath,
        level: &Level<'_>,
        query: &ChildrenQuery,
        token: Option<String>,
        wanted: usize,
    ) -> Result<Listed, DriverError> {
        let max = i32::try_from(wanted.max(1)).unwrap_or(1);
        let refused = |error: Refused| errors::connection(error);
        match level {
            Level::Catalogs => {
                let output = self
                    .client
                    .list_data_catalogs()
                    .set_next_token(token)
                    .max_results(max)
                    .send()
                    .await
                    .map_err(|error| refused(error.into()))?;
                Ok(Listed {
                    nodes: output
                        .data_catalogs_summary()
                        .iter()
                        .filter_map(|summary| catalog_node(path, summary))
                        .collect(),
                    next: output.next_token().map(str::to_owned),
                })
            }
            Level::Databases { catalog } => {
                let output = self
                    .client
                    .list_databases()
                    .catalog_name(*catalog)
                    .set_next_token(token)
                    .max_results(max)
                    .send()
                    .await
                    .map_err(|error| refused(error.into()))?;
                Ok(Listed {
                    nodes: output
                        .database_list()
                        .iter()
                        .map(|database| database_node(path, database.name()))
                        .collect(),
                    next: output.next_token().map(str::to_owned),
                })
            }
            Level::Tables { catalog, database } => {
                let output = self
                    .client
                    .list_table_metadata()
                    .catalog_name(*catalog)
                    .database_name(*database)
                    .set_expression(query.filter.as_deref().map(name_expression))
                    .set_next_token(token)
                    .max_results(max)
                    .send()
                    .await
                    .map_err(|error| refused(error.into()))?;
                let definable = *catalog == self.own_catalog;
                Ok(Listed {
                    nodes: output
                        .table_metadata_list()
                        .iter()
                        .map(|table| table_node(path, table, definable))
                        .collect(),
                    next: output.next_token().map(str::to_owned),
                })
            }
            Level::Columns { .. } => Err(DriverError::InvalidPath(path.clone())),
        }
    }

    /// A table's columns, then its partition keys, which Athena lists apart.
    async fn columns(
        &self,
        path: &CatalogPath,
        catalog: &str,
        database: &str,
        table: &str,
    ) -> Result<Vec<CatalogNode>, DriverError> {
        let output = self
            .client
            .get_table_metadata()
            .catalog_name(catalog)
            .database_name(database)
            .table_name(table)
            .send()
            .await
            .map_err(|error| errors::connection(Refused::from(error)))?;
        let Some(metadata) = output.table_metadata() else {
            return Ok(Vec::new());
        };
        let columns = metadata
            .columns()
            .iter()
            .map(|column| column_node(path, column, false));
        let partitions = metadata
            .partition_keys()
            .iter()
            .map(|column| column_node(path, column, true));
        Ok(columns.chain(partitions).collect())
    }
}

/// The statement that shows the definition of the table or view at `path`: `SHOW CREATE
/// TABLE` or `SHOW CREATE VIEW`, which Athena answers a line a row, scanning nothing.
///
/// # Errors
///
/// [`DriverError::InvalidPath`] for anything but a table or view of the data source's own
/// catalog, which its queries run in.
pub(crate) fn definition_statement(
    path: &CatalogPath,
    own_catalog: &str,
) -> Result<String, DriverError> {
    let invalid = || DriverError::InvalidPath(path.clone());
    let Some(Level::Columns {
        catalog,
        database,
        table,
    }) = Level::of(path)
    else {
        return Err(invalid());
    };
    if catalog != own_catalog {
        return Err(invalid());
    }
    let object = match path.last().map(|segment| segment.kind) {
        Some(NodeKind::View) => "VIEW",
        _ => "TABLE",
    };
    Ok(format!(
        "SHOW CREATE {object} {}.{}",
        hive_name(database),
        hive_name(table)
    ))
}

/// A name as Athena's DDL (Hive's) reads it: bare when it is lowercase letters, digits and
/// underscores, as Glue names are, else in backquotes.
fn hive_name(name: &str) -> String {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain {
        name.to_owned()
    } else {
        format!("`{}`", name.replace('`', "``"))
    }
}

/// `ListTableMetadata`'s expression for names that contain `filter`: a regular expression
/// Athena matches whole names against, with the filter's own characters escaped, lowercase as
/// Glue keeps names.
fn name_expression(filter: &str) -> String {
    let mut expression = String::from(".*");
    for c in filter.to_lowercase().chars() {
        if r"\.^$|?*+()[]{}".contains(c) {
            expression.push('\\');
        }
        expression.push(c);
    }
    expression.push_str(".*");
    expression
}

fn catalog_node(parent: &CatalogPath, summary: &DataCatalogSummary) -> Option<CatalogNode> {
    let name = summary.catalog_name()?;
    let kind = summary.r#type().map(|kind| match kind.as_str() {
        "GLUE" => "Glue".to_owned(),
        "LAMBDA" => "Lambda".to_owned(),
        "HIVE" => "Hive".to_owned(),
        "FEDERATED" => "federated".to_owned(),
        other => other.to_lowercase(),
    });
    Some(CatalogNode {
        path: parent.child(NodeKind::Database, name),
        label: name.to_owned(),
        detail: kind,
        has_children: true,
        has_definition: false,
        system: false,
    })
}

fn database_node(parent: &CatalogPath, name: &str) -> CatalogNode {
    CatalogNode {
        path: parent.child(NodeKind::Schema, name),
        label: name.to_owned(),
        detail: None,
        has_children: true,
        has_definition: false,
        system: name.eq_ignore_ascii_case("information_schema"),
    }
}

fn table_node(parent: &CatalogPath, table: &TableMetadata, definable: bool) -> CatalogNode {
    let kind = match table.table_type() {
        Some(VIEW_TYPE) => NodeKind::View,
        _ => NodeKind::Table,
    };
    CatalogNode {
        path: parent.child(kind, table.name()),
        label: table.name().to_owned(),
        detail: table_detail(table.parameters()),
        has_children: true,
        has_definition: definable,
        system: false,
    }
}

/// What a table is kept as: its table format (`Iceberg`), else its files' (`parquet`), as
/// Glue's parameters say.
fn table_detail(parameters: Option<&HashMap<String, String>>) -> Option<String> {
    let parameters = parameters?;
    let format = |key: &str| parameters.get(key).filter(|value| !value.is_empty());
    match format("table_type") {
        Some(table_type) if table_type.eq_ignore_ascii_case("iceberg") => {
            Some("Iceberg".to_owned())
        }
        Some(table_type) if table_type.eq_ignore_ascii_case("delta") => Some("Delta".to_owned()),
        _ => format("classification").map(|classification| classification.to_lowercase()),
    }
}

fn column_node(parent: &CatalogPath, column: &Column, partition_key: bool) -> CatalogNode {
    let type_name = column.r#type().unwrap_or("unknown");
    let detail = if partition_key {
        format!("{type_name}, partition key")
    } else {
        type_name.to_owned()
    };
    CatalogNode {
        path: parent.child(NodeKind::Column, column.name()),
        label: column.name().to_owned(),
        detail: Some(detail),
        has_children: false,
        has_definition: false,
        system: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_is_matched_inside_names_with_its_characters_as_they_are() {
        assert_eq!(name_expression("Order"), ".*order.*");
        assert_eq!(name_expression("a.b*"), r".*a\.b\*.*");
    }

    #[test]
    fn definitions_are_shown_for_the_data_sources_own_catalog() {
        let table = |kind, catalog: &str, name: &str| {
            CatalogPath::root()
                .child(NodeKind::Database, catalog)
                .child(NodeKind::Schema, "sales")
                .child(kind, name)
        };
        let own = "AwsDataCatalog";
        let cases = [
            (
                table(NodeKind::Table, own, "orders"),
                Some("SHOW CREATE TABLE sales.orders"),
            ),
            (
                table(NodeKind::View, own, "Monthly Totals"),
                Some("SHOW CREATE VIEW sales.`Monthly Totals`"),
            ),
            (table(NodeKind::Table, "lambda_ddb", "orders"), None),
            (CatalogPath::root().child(NodeKind::Database, own), None),
        ];
        for (path, expected) in cases {
            assert_eq!(
                definition_statement(&path, own).ok().as_deref(),
                expected,
                "{path}"
            );
        }
    }
}
