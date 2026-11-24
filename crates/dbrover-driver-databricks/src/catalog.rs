//! The explorer's tree, from Unity Catalog's REST API, which is no statement and so starts no
//! warehouse (`docs/cloud-data.md`, C6, "Unity Catalog on top, without a warehouse"):
//! catalogs, a catalog's schemas, a schema's tables, views and volumes in a folder each, and a
//! table's or view's columns.

use dbrover_core::{
    CatalogMatches, CatalogNode, CatalogPath, CatalogSearch, ChildrenPage, ChildrenQuery,
    DriverError, FolderKind, NodeKind, PageToken,
};
use serde_json::Value;

use crate::api::Api;
use crate::errors;

/// Unity Catalog's API, below the workspace's address.
const UNITY_CATALOG: &str = "/api/2.1/unity-catalog";

/// The most catalogs, schemas or volumes one request asks for: the explorer's own page, within
/// Unity Catalog's thousand for catalogs and schemas.
const PAGE_SIZE: usize = 1000;

/// The most tables one request may ask for: Unity Catalog's limit for listing them.
const TABLES_PAGE_SIZE: usize = 50;

/// What a path names, by its shape.
enum Level<'a> {
    Catalogs,
    Schemas {
        catalog: &'a str,
    },
    /// A schema, whose children are its folders whatever it holds.
    Folders,
    Objects {
        catalog: &'a str,
        schema: &'a str,
        folder: FolderKind,
    },
    Columns {
        catalog: &'a str,
        schema: &'a str,
        relation: &'a str,
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
            [(NodeKind::Database, catalog)] => Self::Schemas { catalog },
            [(NodeKind::Database, _), (NodeKind::Schema, _)] => Self::Folders,
            [
                (NodeKind::Database, catalog),
                (NodeKind::Schema, schema),
                (NodeKind::Folder(folder), _),
            ] if FOLDERS.contains(folder) => Self::Objects {
                catalog,
                schema,
                folder: *folder,
            },
            [
                (NodeKind::Database, catalog),
                (NodeKind::Schema, schema),
                (NodeKind::Folder(_), _),
                (NodeKind::Table | NodeKind::View | NodeKind::MaterializedView, relation),
            ] => Self::Columns {
                catalog,
                schema,
                relation,
            },
            _ => return None,
        })
    }
}

/// A schema's folders, in the order the explorer shows them.
const FOLDERS: [FolderKind; 3] = [FolderKind::Tables, FolderKind::Views, FolderKind::Volumes];

/// `name` in backquotes, which Databricks reads as written whatever the name holds.
pub(crate) fn quoted(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// `text` as a query parameter's value or a path's segment.
fn encoded(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

/// A table's or volume's full name as Unity Catalog's paths take it: `catalog.schema.name`.
fn full_name(catalog: &str, schema: &str, name: &str) -> String {
    encoded(&format!("{catalog}.{schema}.{name}"))
}

/// Lists part of a level through a connection's API client.
pub(crate) struct Lister<'a> {
    pub api: &'a Api,
}

impl Lister<'_> {
    /// The nodes of `path`'s level that `query` asks for.
    ///
    /// # Errors
    ///
    /// [`DriverError::InvalidPath`] for a path this driver never lists, and Databricks's error
    /// when a request fails.
    pub(crate) async fn page(
        &self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        let level = Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
        match level {
            Level::Catalogs => {
                self.listed(
                    path,
                    "catalogs?",
                    "catalogs",
                    PAGE_SIZE,
                    query,
                    catalog_node,
                )
                .await
            }
            Level::Schemas { catalog } => {
                let endpoint = format!("schemas?catalog_name={}&", encoded(catalog));
                self.listed(path, &endpoint, "schemas", PAGE_SIZE, query, schema_node)
                    .await
            }
            Level::Folders => {
                let folders = FOLDERS
                    .iter()
                    .map(|folder| CatalogNode::folder(path, *folder))
                    .collect();
                Ok(ChildrenPage::whole(folders, query))
            }
            Level::Objects {
                catalog,
                schema,
                folder: FolderKind::Volumes,
            } => {
                let endpoint = format!(
                    "volumes?catalog_name={}&schema_name={}&",
                    encoded(catalog),
                    encoded(schema)
                );
                self.listed(path, &endpoint, "volumes", PAGE_SIZE, query, volume_node)
                    .await
            }
            Level::Objects {
                catalog, schema, ..
            } if query
                .filter
                .as_deref()
                .is_some_and(|filter| !filter.trim().is_empty()) =>
            {
                self.filtered_relations(path, catalog, schema, query).await
            }
            Level::Objects {
                catalog, schema, ..
            } => {
                let endpoint = format!(
                    "tables?catalog_name={}&schema_name={}&omit_columns=true&\
                     omit_properties=true&",
                    encoded(catalog),
                    encoded(schema)
                );
                self.listed(
                    path,
                    &endpoint,
                    "tables",
                    TABLES_PAGE_SIZE,
                    query,
                    relation_node,
                )
                .await
            }
            Level::Columns {
                catalog,
                schema,
                relation,
            } => {
                let table = self.table(catalog, schema, relation).await?;
                let nodes = table["columns"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|column| column_node(path, column))
                    .collect();
                Ok(ChildrenPage::whole(nodes, query))
            }
        }
    }

    /// A level Unity Catalog pages: `endpoint` (ending in `?` or `&`) asked for `max` items at
    /// most each time, from the page `query` continues, until `query`'s limit is reached or
    /// there are no more. `to_node` makes a node of an item of the answer's `key`, or none of
    /// one this level doesn't show (a view, in the tables folder). A page may be empty and
    /// still say there are more, as the reference warns.
    async fn listed(
        &self,
        path: &CatalogPath,
        endpoint: &str,
        key: &str,
        max: usize,
        query: &ChildrenQuery,
        to_node: fn(&CatalogPath, &Value) -> Option<CatalogNode>,
    ) -> Result<ChildrenPage, DriverError> {
        let limit = query.limit.unwrap_or(usize::MAX);
        let mut token = query.after.as_ref().map(|token| token.as_str().to_owned());
        let mut nodes = Vec::new();
        loop {
            let wanted = limit.saturating_sub(nodes.len()).clamp(1, max);
            let mut address = format!("{UNITY_CATALOG}/{endpoint}max_results={wanted}");
            if let Some(token) = &token {
                address.push_str(&format!("&page_token={}", encoded(token)));
            }
            let body = self.get(&address).await?;
            nodes.extend(
                body[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|item| to_node(path, item))
                    .filter(|node| query.matches(&node.label)),
            );
            token = body["next_page_token"]
                .as_str()
                .filter(|token| !token.is_empty())
                .map(str::to_owned);
            if token.is_none() || nodes.len() >= limit {
                break;
            }
        }
        Ok(ChildrenPage {
            nodes,
            next: token.map(PageToken::new),
        })
    }

    /// The tables or views of a schema whose names contain the filter, from Unity Catalog's
    /// table summaries, which take a `LIKE` pattern: the tables list takes none, and pages 50
    /// at a time. A summary says a table's type but not its format, so a filtered table's
    /// detail names less than an unfiltered one's. Whether the pattern escapes `%` and `_`
    /// isn't documented, so what it matches is narrowed back here (`summary_node` keeps only
    /// this schema's, and the query's filter the names).
    async fn filtered_relations(
        &self,
        path: &CatalogPath,
        catalog: &str,
        schema: &str,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        let filter = query.filter.as_deref().unwrap_or_default().trim();
        let endpoint = format!(
            "table-summaries?catalog_name={}&schema_name_pattern={}&table_name_pattern={}&",
            encoded(catalog),
            encoded(schema),
            encoded(&format!("%{filter}%"))
        );
        self.listed(path, &endpoint, "tables", PAGE_SIZE, query, summary_node)
            .await
    }

    async fn get(&self, address: &str) -> Result<Value, DriverError> {
        self.api.get(address).await.map_err(errors::connection)
    }

    /// A table's or view's whole description, its columns among it.
    async fn table(&self, catalog: &str, schema: &str, name: &str) -> Result<Value, DriverError> {
        self.get(&format!(
            "{UNITY_CATALOG}/tables/{}",
            full_name(catalog, schema, name)
        ))
        .await
    }

    /// What **Show definition** shows for the table or view at `path`: a view's, materialized
    /// view's or streaming table's own text, and for a table a `CREATE TABLE` written from its
    /// columns, format and partitioning. `SHOW CREATE TABLE` would be exact, but it is a
    /// statement, which would start the warehouse.
    ///
    /// # Errors
    ///
    /// [`DriverError::InvalidPath`] for anything but a table or view.
    pub(crate) async fn definition(&self, path: &CatalogPath) -> Result<String, DriverError> {
        let Some(Level::Columns {
            catalog,
            schema,
            relation,
        }) = Level::of(path)
        else {
            return Err(DriverError::InvalidPath(path.clone()));
        };
        let table = self.table(catalog, schema, relation).await?;
        let name = format!(
            "{}.{}.{}",
            quoted(catalog),
            quoted(schema),
            quoted(relation)
        );
        Ok(definition_of(&name, &table))
    }

    /// Tables and views anywhere in the workspace whose names contain the search's text: each
    /// catalog's table summaries, in the catalogs' order, until one more than the limit is
    /// found, to say whether there were more.
    pub(crate) async fn search(
        &self,
        search: &CatalogSearch,
    ) -> Result<CatalogMatches, DriverError> {
        let catalogs = self
            .listed(
                &CatalogPath::root(),
                "catalogs?",
                "catalogs",
                PAGE_SIZE,
                &ChildrenQuery {
                    filter: None,
                    after: None,
                    limit: None,
                },
                catalog_node,
            )
            .await?;
        let needle = search.text.to_lowercase();
        let mut nodes = Vec::new();
        for catalog in catalogs.nodes.iter().filter(|catalog| !catalog.system) {
            let wanted = search.limit + 1 - nodes.len();
            let endpoint = format!(
                "table-summaries?catalog_name={}&table_name_pattern={}&",
                encoded(&catalog.label),
                encoded(&format!("%{}%", search.text.trim()))
            );
            let found = self
                .listed(
                    &CatalogPath::root(),
                    &endpoint,
                    "tables",
                    wanted,
                    &ChildrenQuery {
                        filter: None,
                        after: None,
                        limit: Some(wanted),
                    },
                    found_node,
                )
                .await?;
            nodes.extend(
                found
                    .nodes
                    .into_iter()
                    .filter(|node| node.label.to_lowercase().contains(&needle)),
            );
            if nodes.len() > search.limit {
                break;
            }
        }
        Ok(CatalogMatches::cut(nodes, search.limit))
    }
}

/// `catalog.schema.name` split at its dots; Unity Catalog allows no dot in a catalog's or a
/// schema's name, so a table's name keeps any of its own.
fn split_full_name(full: &str) -> Option<(&str, &str, &str)> {
    let (catalog, rest) = full.split_once('.')?;
    let (schema, name) = rest.split_once('.')?;
    Some((catalog, schema, name))
}

fn text<'v>(item: &'v Value, key: &str) -> &'v str {
    item[key].as_str().unwrap_or_default()
}

/// A catalog, marked when it isn't the workspace's own: shared with it, a foreign database's,
/// or Databricks's. The `system` catalog is hidden unless the explorer shows system objects,
/// as `information_schema` is.
fn catalog_node(parent: &CatalogPath, item: &Value) -> Option<CatalogNode> {
    let name = item["name"].as_str()?;
    let kind = text(item, "catalog_type");
    let detail = match kind {
        "" | "MANAGED_CATALOG" => None,
        "DELTASHARING_CATALOG" => Some("shared".to_owned()),
        "FOREIGN_CATALOG" => Some("foreign".to_owned()),
        "SYSTEM_CATALOG" => Some("system".to_owned()),
        other => Some(
            other
                .trim_end_matches("_CATALOG")
                .replace('_', " ")
                .to_lowercase(),
        ),
    };
    Some(CatalogNode {
        path: parent.child(NodeKind::Database, name),
        label: name.to_owned(),
        detail,
        has_children: true,
        has_definition: false,
        system: kind == "SYSTEM_CATALOG",
    })
}

fn schema_node(parent: &CatalogPath, item: &Value) -> Option<CatalogNode> {
    let name = item["name"].as_str()?;
    Some(CatalogNode {
        path: parent.child(NodeKind::Schema, name),
        label: name.to_owned(),
        detail: None,
        has_children: true,
        has_definition: false,
        system: name.eq_ignore_ascii_case("information_schema"),
    })
}

/// What a table type is in the explorer, and the folder it is listed in.
fn relation_kind(table_type: &str) -> (NodeKind, FolderKind) {
    match table_type {
        "VIEW" | "METRIC_VIEW" => (NodeKind::View, FolderKind::Views),
        "MATERIALIZED_VIEW" => (NodeKind::MaterializedView, FolderKind::Views),
        _ => (NodeKind::Table, FolderKind::Tables),
    }
}

/// What a table's type says of it, besides being a table or a view.
fn type_mark(table_type: &str) -> Option<&'static str> {
    match table_type {
        "EXTERNAL" => Some("external"),
        "FOREIGN" => Some("foreign"),
        "STREAMING_TABLE" => Some("streaming"),
        "METRIC_VIEW" => Some("metric"),
        "MANAGED_SHALLOW_CLONE" | "EXTERNAL_SHALLOW_CLONE" => Some("shallow clone"),
        _ => None,
    }
}

/// A table's storage format as Databricks names it: `Delta`, `Iceberg`, `Parquet`; none for a
/// view's or a foreign table's.
fn format_mark(format: &str) -> Option<String> {
    match format {
        "" | "UNITY_CATALOG" | "DATABRICKS_FORMAT" => None,
        "DELTA" => Some("Delta".to_owned()),
        "ICEBERG" => Some("Iceberg".to_owned()),
        "PARQUET" => Some("Parquet".to_owned()),
        "DELTA_UNIFORM_ICEBERG" => Some("Delta, Iceberg readable".to_owned()),
        format if format.ends_with("_FORMAT") => None,
        other => Some(other.to_owned()),
    }
}

/// A table or view in `parent`, a tables or views folder, from the tables list; `None` for one
/// the other folder lists.
fn relation_node(parent: &CatalogPath, item: &Value) -> Option<CatalogNode> {
    let name = item["name"].as_str()?;
    let table_type = text(item, "table_type");
    let (kind, folder) = relation_kind(table_type);
    if parent.last().map(|segment| segment.kind) != Some(NodeKind::Folder(folder)) {
        return None;
    }
    let marks: Vec<String> = format_mark(text(item, "data_source_format"))
        .into_iter()
        .chain(type_mark(table_type).map(str::to_owned))
        .collect();
    Some(relation(
        parent,
        kind,
        name,
        (!marks.is_empty()).then(|| marks.join(", ")),
    ))
}

/// A table or view in `parent` from a table summary, which names it by its full name and says
/// its type but not its format.
fn summary_node(parent: &CatalogPath, item: &Value) -> Option<CatalogNode> {
    let (catalog, schema, name) = split_full_name(item["full_name"].as_str()?)?;
    let table_type = text(item, "table_type");
    let (kind, folder) = relation_kind(table_type);
    if parent.last().map(|segment| segment.kind) != Some(NodeKind::Folder(folder))
        || parent.name_of(NodeKind::Database) != Some(catalog)
        || parent.name_of(NodeKind::Schema) != Some(schema)
    {
        return None;
    }
    Some(relation(
        parent,
        kind,
        name,
        type_mark(table_type).map(str::to_owned),
    ))
}

/// A table or view found by a search, under its catalog, schema and folder, its detail saying
/// where.
fn found_node(_root: &CatalogPath, item: &Value) -> Option<CatalogNode> {
    let (catalog, schema, name) = split_full_name(item["full_name"].as_str()?)?;
    let (kind, folder) = relation_kind(text(item, "table_type"));
    let parent = CatalogPath::root()
        .child(NodeKind::Database, catalog)
        .child(NodeKind::Schema, schema)
        .child(NodeKind::Folder(folder), folder.label());
    Some(relation(
        &parent,
        kind,
        name,
        Some(format!("{catalog}.{schema}")),
    ))
}

fn relation(
    parent: &CatalogPath,
    kind: NodeKind,
    name: &str,
    detail: Option<String>,
) -> CatalogNode {
    CatalogNode {
        path: parent.child(kind, name),
        label: name.to_owned(),
        detail,
        has_children: true,
        has_definition: true,
        system: false,
    }
}

/// A volume: Databricks's own storage, or a bucket's address for an external one.
fn volume_node(parent: &CatalogPath, item: &Value) -> Option<CatalogNode> {
    let name = item["name"].as_str()?;
    let detail = match (text(item, "volume_type"), text(item, "storage_location")) {
        ("EXTERNAL", location) if !location.is_empty() => Some(location.to_owned()),
        ("", _) => None,
        (kind, _) => Some(kind.to_lowercase()),
    };
    Some(CatalogNode {
        path: parent.child(NodeKind::Volume, name),
        label: name.to_owned(),
        detail,
        has_children: false,
        has_definition: false,
        system: false,
    })
}

/// A column, its type as Databricks writes it.
fn column_node(parent: &CatalogPath, item: &Value) -> Option<CatalogNode> {
    let name = item["name"].as_str()?;
    Some(CatalogNode {
        path: parent.child(NodeKind::Column, name),
        label: name.to_owned(),
        detail: Some(column_type(item)),
        has_children: false,
        has_definition: false,
        system: false,
    })
}

/// A column's type in upper case, as Databricks's own DDL writes it: `type_text` is lower case
/// (`decimal(10,2)`, `array<string>`).
fn column_type(column: &Value) -> String {
    match text(column, "type_text") {
        "" => text(column, "type_name").to_owned(),
        text => text.to_uppercase(),
    }
}

/// `table`'s definition, named `name`: a view's text in its `CREATE`, or a table's `CREATE
/// TABLE` written from its columns, format, partitioning and comment.
fn definition_of(name: &str, table: &Value) -> String {
    let table_type = text(table, "table_type");
    let body = text(table, "view_definition");
    let created = match table_type {
        "VIEW" => Some("VIEW"),
        "MATERIALIZED_VIEW" => Some("MATERIALIZED VIEW"),
        "STREAMING_TABLE" => Some("STREAMING TABLE"),
        _ => None,
    };
    if table_type == "METRIC_VIEW" && !body.is_empty() {
        return format!(
            "CREATE VIEW {name}\nWITH METRICS\nLANGUAGE YAML\nAS $$\n{}\n$$;",
            body.trim()
        );
    }
    if let Some(created) = created
        && !body.is_empty()
    {
        return format!(
            "CREATE {created} {name} AS\n{};",
            body.trim().trim_end_matches(';')
        );
    }
    create_table(name, table)
}

fn create_table(name: &str, table: &Value) -> String {
    let columns: Vec<&Value> = table["columns"].as_array().into_iter().flatten().collect();
    let lines: Vec<String> = columns
        .iter()
        .map(|column| {
            let mut line = format!("  {} {}", quoted(text(column, "name")), column_type(column));
            if column["nullable"].as_bool() == Some(false) {
                line.push_str(" NOT NULL");
            }
            let comment = text(column, "comment");
            if !comment.is_empty() {
                line.push_str(&format!(" COMMENT {}", literal(comment)));
            }
            line
        })
        .collect();
    let mut definition = format!("CREATE TABLE {name} (\n{}\n)", lines.join(",\n"));
    let format = text(table, "data_source_format");
    if matches!(
        format,
        "DELTA" | "ICEBERG" | "PARQUET" | "CSV" | "JSON" | "AVRO" | "ORC"
    ) {
        definition.push_str(&format!("\nUSING {}", format.to_uppercase()));
    }
    let mut partitions: Vec<(u64, &str)> = columns
        .iter()
        .filter_map(|column| Some((column["partition_index"].as_u64()?, text(column, "name"))))
        .collect();
    partitions.sort_unstable();
    if !partitions.is_empty() {
        let names: Vec<String> = partitions.iter().map(|(_, name)| quoted(name)).collect();
        definition.push_str(&format!("\nPARTITIONED BY ({})", names.join(", ")));
    }
    let location = text(table, "storage_location");
    if table_type_is_external(table) && !location.is_empty() {
        definition.push_str(&format!("\nLOCATION {}", literal(location)));
    }
    let comment = text(table, "comment");
    if !comment.is_empty() {
        definition.push_str(&format!("\nCOMMENT {}", literal(comment)));
    }
    definition.push(';');
    definition
}

fn table_type_is_external(table: &Value) -> bool {
    text(table, "table_type") == "EXTERNAL"
}

/// `text` as a Databricks string literal: a quote and a backslash each after a backslash, since
/// Spark joins `'it''s'` into `its`.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_backquoted_and_literals_escaped_with_backslashes() {
        assert_eq!(quoted("my `odd` table"), "`my ``odd`` table`");
        assert_eq!(literal(r"it's C:\temp"), r"'it\'s C:\\temp'");
        assert_eq!(
            full_name("main", "sales", "eu orders"),
            "main.sales.eu+orders"
        );
        assert_eq!(
            split_full_name("main.sales.a.b"),
            Some(("main", "sales", "a.b"))
        );
    }

    #[test]
    fn a_tables_definition_is_written_from_its_columns() {
        let table = serde_json::json!({
            "table_type": "EXTERNAL",
            "data_source_format": "DELTA",
            "storage_location": "s3://lake/orders",
            "comment": "it's the orders",
            "columns": [
                {"name": "id", "type_text": "bigint", "nullable": false, "position": 0},
                {"name": "total", "type_text": "decimal(10,2)", "comment": "in euros",
                 "position": 1},
                {"name": "day", "type_text": "date", "partition_index": 0, "position": 2}
            ]
        });
        assert_eq!(
            definition_of("`main`.`sales`.`orders`", &table),
            "CREATE TABLE `main`.`sales`.`orders` (\n  `id` BIGINT NOT NULL,\n  `total` \
             DECIMAL(10,2) COMMENT 'in euros',\n  `day` DATE\n)\nUSING DELTA\nPARTITIONED BY \
             (`day`)\nLOCATION 's3://lake/orders'\nCOMMENT 'it\\'s the orders';"
        );
        let view = serde_json::json!({
            "table_type": "MATERIALIZED_VIEW",
            "view_definition": "SELECT customer, sum(total) FROM orders GROUP BY customer"
        });
        assert_eq!(
            definition_of("`m`.`s`.`daily`", &view),
            "CREATE MATERIALIZED VIEW `m`.`s`.`daily` AS\nSELECT customer, sum(total) FROM \
             orders GROUP BY customer;"
        );
    }
}
