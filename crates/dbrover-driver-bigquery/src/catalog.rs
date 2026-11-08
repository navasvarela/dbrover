//! The explorer's tree, from BigQuery's own listing calls, which cost nothing and run no job:
//! projects (the data source's own, the others it names, then `projects.list`), a project's
//! datasets (`datasets.list`), a dataset's tables and views (`tables.list`), and a table's
//! columns (`tables.get`), a record's fields flattened under it as `record.field`.

use std::collections::HashSet;

use dbrover_core::{
    CatalogNode, CatalogPath, ChildrenPage, ChildrenQuery, DriverError, NodeKind, PageToken,
};
use serde::Deserialize;

use crate::api::{Api, segment};
use crate::errors;
use crate::results::{Field, Schema};

/// The most BigQuery's list calls are asked for at once. They may answer with fewer.
const MAX_LISTED: usize = 1000;

/// What a path names, by its shape: `[Database]` is a project, `[Database, Schema]` one of its
/// datasets, and `[Database, Schema, Table | View | MaterializedView]` a table or view.
enum Level<'a> {
    Projects,
    Datasets {
        project: &'a str,
    },
    Tables {
        project: &'a str,
        dataset: &'a str,
    },
    Columns {
        project: &'a str,
        dataset: &'a str,
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
            [] => Self::Projects,
            [(NodeKind::Database, project)] => Self::Datasets { project },
            [(NodeKind::Database, project), (NodeKind::Schema, dataset)] => {
                Self::Tables { project, dataset }
            }
            [
                (NodeKind::Database, project),
                (NodeKind::Schema, dataset),
                (NodeKind::Table | NodeKind::View | NodeKind::MaterializedView, table),
            ] => Self::Columns {
                project,
                dataset,
                table,
            },
            _ => return None,
        })
    }
}

/// The projects the explorer shows above `projects.list`'s: the data source's own, then those
/// it names.
pub(crate) struct NamedProjects<'a> {
    pub own: &'a str,
    pub others: &'a [String],
}

impl NamedProjects<'_> {
    fn names(&self) -> Vec<&str> {
        let mut seen = HashSet::new();
        std::iter::once(self.own)
            .chain(self.others.iter().map(String::as_str))
            .filter(|name| !name.is_empty() && seen.insert(*name))
            .collect()
    }
}

/// Lists part of a level: what the explorer asks for, a thousand at a time.
pub(crate) struct Lister<'a> {
    pub api: &'a Api,
    pub projects: NamedProjects<'a>,
}

/// One answer of a list call: its nodes, and the token of the next.
struct Listed {
    nodes: Vec<CatalogNode>,
    next: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectList {
    #[serde(default)]
    projects: Vec<ProjectEntry>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectEntry {
    id: Option<String>,
    friendly_name: Option<String>,
    project_reference: Option<ProjectReference>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectReference {
    project_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DatasetList {
    #[serde(default)]
    datasets: Vec<DatasetEntry>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DatasetEntry {
    dataset_reference: DatasetReference,
    location: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DatasetReference {
    dataset_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TableList {
    #[serde(default)]
    tables: Vec<TableEntry>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TableEntry {
    table_reference: TableReference,
    #[serde(rename = "type", default)]
    kind: String,
    time_partitioning: Option<TimePartitioning>,
    range_partitioning: Option<RangePartitioning>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TableReference {
    table_id: String,
}

/// A table resource from `tables.get`: its columns, how it is partitioned and clustered, and a
/// view's query.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Table {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    schema: Schema,
    time_partitioning: Option<TimePartitioning>,
    range_partitioning: Option<RangePartitioning>,
    clustering: Option<Clustering>,
    view: Option<ViewDefinition>,
    materialized_view: Option<ViewDefinition>,
}

#[derive(Deserialize)]
struct TimePartitioning {
    /// `DAY`, `HOUR`, `MONTH` or `YEAR`.
    #[serde(rename = "type", default)]
    unit: String,
    /// `None` for ingestion time.
    field: Option<String>,
}

#[derive(Deserialize)]
struct RangePartitioning {
    field: String,
    range: Option<RangeBounds>,
}

/// An integer range's bounds, which the REST API writes as strings.
#[derive(Deserialize)]
struct RangeBounds {
    start: Option<String>,
    end: Option<String>,
    interval: Option<String>,
}

#[derive(Deserialize)]
struct Clustering {
    #[serde(default)]
    fields: Vec<String>,
}

#[derive(Deserialize)]
struct ViewDefinition {
    query: String,
}

impl Lister<'_> {
    /// The nodes of `path`'s level that `query` asks for.
    ///
    /// # Errors
    ///
    /// [`DriverError::InvalidPath`] for a path this driver never lists, and
    /// [`DriverError::Connection`] when BigQuery refuses a request.
    pub(crate) async fn page(
        &self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        let level = Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
        if let Level::Columns {
            project,
            dataset,
            table,
        } = level
        {
            let table = self.table(project, dataset, table).await?;
            return Ok(ChildrenPage::whole(table.columns(path), query));
        }
        let limit = query.limit.unwrap_or(usize::MAX);
        let mut nodes = Vec::new();
        let mut token = query.after.as_ref().map(|token| token.as_str().to_owned());
        // The named projects come first, on the level's first page only.
        if matches!(level, Level::Projects) && token.is_none() {
            nodes.extend(
                self.projects
                    .names()
                    .into_iter()
                    .map(|name| project_node(name, None))
                    .filter(|node| query.matches(&node.label)),
            );
        }
        loop {
            let wanted = limit.saturating_sub(nodes.len()).clamp(1, MAX_LISTED);
            let listed = self.list(path, &level, token.take(), wanted).await?;
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
        token: Option<String>,
        wanted: usize,
    ) -> Result<Listed, DriverError> {
        let wanted = wanted.to_string();
        let mut query = vec![("maxResults", wanted.as_str())];
        if let Some(token) = &token {
            query.push(("pageToken", token));
        }
        match level {
            Level::Projects => {
                let listed: ProjectList = self
                    .api
                    .get("/projects", &query)
                    .await
                    .map_err(errors::connection)?;
                let named = self.projects.names();
                Ok(Listed {
                    nodes: listed
                        .projects
                        .iter()
                        .filter_map(|project| {
                            let id = project
                                .project_reference
                                .as_ref()
                                .map(|reference| reference.project_id.as_str())
                                .or(project.id.as_deref())?;
                            (!named.contains(&id))
                                .then(|| project_node(id, project.friendly_name.as_deref()))
                        })
                        .collect(),
                    next: listed.next_page_token,
                })
            }
            Level::Datasets { project } => {
                let listed: DatasetList = self
                    .api
                    .get(&format!("/projects/{}/datasets", segment(project)), &query)
                    .await
                    .map_err(errors::connection)?;
                Ok(Listed {
                    nodes: listed
                        .datasets
                        .iter()
                        .map(|dataset| dataset_node(path, dataset))
                        .collect(),
                    next: listed.next_page_token,
                })
            }
            Level::Tables { project, dataset } => {
                let listed: TableList = self
                    .api
                    .get(
                        &format!(
                            "/projects/{}/datasets/{}/tables",
                            segment(project),
                            segment(dataset)
                        ),
                        &query,
                    )
                    .await
                    .map_err(errors::connection)?;
                Ok(Listed {
                    nodes: listed
                        .tables
                        .iter()
                        .map(|table| table_node(path, table))
                        .collect(),
                    next: listed.next_page_token,
                })
            }
            Level::Columns { .. } => Err(DriverError::InvalidPath(path.clone())),
        }
    }

    /// `tables.get` of one table or view, which is free.
    pub(crate) async fn table(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
    ) -> Result<Table, DriverError> {
        self.api
            .get(
                &format!(
                    "/projects/{}/datasets/{}/tables/{}",
                    segment(project),
                    segment(dataset),
                    segment(table)
                ),
                &[],
            )
            .await
            .map_err(errors::connection)
    }

    /// What **Show definition** shows for the table or view at `path`, from `tables.get`, which
    /// is free: a view's own query as the `CREATE VIEW` BigQuery would take, and for a table a
    /// `CREATE TABLE` written from its columns, partitioning and clustering.
    ///
    /// # Errors
    ///
    /// [`DriverError::InvalidPath`] for anything but a table or view.
    pub(crate) async fn definition(&self, path: &CatalogPath) -> Result<String, DriverError> {
        let Some(Level::Columns {
            project,
            dataset,
            table,
        }) = Level::of(path)
        else {
            return Err(DriverError::InvalidPath(path.clone()));
        };
        let found = self.table(project, dataset, table).await?;
        Ok(found.definition(&qualified(project, dataset, table)))
    }
}

/// `` `project.dataset.name` ``: a whole path in one pair of backquotes, as BigQuery's own
/// console writes names, a backquote or backslash in it escaped.
pub(crate) fn qualified(project: &str, dataset: &str, name: &str) -> String {
    let path = format!("{project}.{dataset}.{name}");
    format!("`{}`", path.replace('\\', "\\\\").replace('`', "\\`"))
}

impl Table {
    /// Its schema, when its rows can be read with `tabledata.list`: a table's, not a view's or
    /// an external table's, whose rows only a query reads.
    pub(crate) fn previewable(self) -> Option<Schema> {
        (self.kind == "TABLE").then_some(self.schema)
    }

    fn definition(&self, name: &str) -> String {
        if let Some(view) = &self.view {
            return format!("CREATE VIEW {name} AS\n{};", view.query);
        }
        if let Some(view) = &self.materialized_view {
            return format!("CREATE MATERIALIZED VIEW {name} AS\n{};", view.query);
        }
        let columns: Vec<String> = self
            .schema
            .fields
            .iter()
            .map(|field| {
                let required = if field.mode.as_deref() == Some("REQUIRED") {
                    " NOT NULL"
                } else {
                    ""
                };
                format!(
                    "  {} {}{required}",
                    column_name(&field.name),
                    field.type_name()
                )
            })
            .collect();
        let external = if self.kind == "EXTERNAL" {
            "EXTERNAL "
        } else {
            ""
        };
        let mut text = format!(
            "CREATE {external}TABLE {name}\n(\n{}\n)",
            columns.join(",\n")
        );
        if let Some(partition) = self.partition_expression() {
            text.push_str("\nPARTITION BY ");
            text.push_str(&partition);
        }
        if let Some(clustering) = self
            .clustering
            .as_ref()
            .filter(|clustering| !clustering.fields.is_empty())
        {
            let fields: Vec<String> = clustering
                .fields
                .iter()
                .map(|field| column_name(field))
                .collect();
            text.push_str("\nCLUSTER BY ");
            text.push_str(&fields.join(", "));
        }
        text.push(';');
        text
    }

    /// `PARTITION BY`'s expression, as BigQuery's DDL writes it for each kind of partitioning:
    /// a `DATE` column by day as itself, others truncated to their unit, ingestion time through
    /// `_PARTITIONTIME`, and integer ranges with `RANGE_BUCKET`.
    fn partition_expression(&self) -> Option<String> {
        if let Some(range) = &self.range_partitioning {
            let bounds = range.range.as_ref();
            let bound = |value: Option<&String>| value.map_or("0", String::as_str).to_owned();
            return Some(format!(
                "RANGE_BUCKET({}, GENERATE_ARRAY({}, {}, {}))",
                column_name(&range.field),
                bound(bounds.and_then(|bounds| bounds.start.as_ref())),
                bound(bounds.and_then(|bounds| bounds.end.as_ref())),
                bound(bounds.and_then(|bounds| bounds.interval.as_ref())),
            ));
        }
        let time = self.time_partitioning.as_ref()?;
        let unit = match time.unit.as_str() {
            "" => "DAY",
            unit => unit,
        };
        let Some(field) = &time.field else {
            return Some(match unit {
                "DAY" => "_PARTITIONDATE".to_owned(),
                unit => format!("TIMESTAMP_TRUNC(_PARTITIONTIME, {unit})"),
            });
        };
        let kind = self
            .schema
            .fields
            .iter()
            .find(|column| column.name == *field)
            .map_or("TIMESTAMP".to_owned(), Field::type_name);
        let name = column_name(field);
        Some(match (kind.as_str(), unit) {
            ("DATE", "DAY") => name,
            ("DATE", unit) => format!("DATE_TRUNC({name}, {unit})"),
            ("DATETIME", unit) => format!("DATETIME_TRUNC({name}, {unit})"),
            ("TIMESTAMP", "DAY") => format!("DATE({name})"),
            (_, unit) => format!("TIMESTAMP_TRUNC({name}, {unit})"),
        })
    }

    /// Its columns, a record's fields after it as `record.field`, each with its type and
    /// whether the table is partitioned or clustered on it.
    fn columns(&self, parent: &CatalogPath) -> Vec<CatalogNode> {
        let mut nodes = Vec::new();
        for field in &self.schema.fields {
            self.push_field(parent, field, "", &mut nodes);
        }
        nodes
    }

    fn push_field(
        &self,
        parent: &CatalogPath,
        field: &Field,
        prefix: &str,
        nodes: &mut Vec<CatalogNode>,
    ) {
        let name = format!("{prefix}{}", field.name);
        let mut detail = field.type_name();
        if let Some(role) = self.role_of(&name) {
            detail.push_str(", ");
            detail.push_str(&role);
        }
        nodes.push(CatalogNode {
            path: parent.child(NodeKind::Column, &name),
            label: name.clone(),
            detail: Some(detail),
            has_children: false,
            has_definition: false,
            system: false,
        });
        for inner in &field.fields {
            self.push_field(parent, inner, &format!("{name}."), nodes);
        }
    }

    /// Whether the table is partitioned or clustered on the column `name`.
    fn role_of(&self, name: &str) -> Option<String> {
        let partitioned = self
            .time_partitioning
            .as_ref()
            .and_then(|partitioning| partitioning.field.as_deref())
            .or(self
                .range_partitioning
                .as_ref()
                .map(|partitioning| partitioning.field.as_str()));
        if partitioned == Some(name) {
            return Some("partitioning".to_owned());
        }
        let clustering = self.clustering.as_ref()?;
        let place = clustering.fields.iter().position(|field| field == name)?;
        Some(format!("clustering {}", place + 1))
    }
}

/// A column's name as GoogleSQL reads it: bare when it is letters, digits and underscores not
/// starting with a digit, else in backquotes.
fn column_name(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        name.to_owned()
    } else {
        format!("`{}`", name.replace('\\', "\\\\").replace('`', "\\`"))
    }
}

fn project_node(id: &str, friendly_name: Option<&str>) -> CatalogNode {
    CatalogNode {
        path: CatalogPath::root().child(NodeKind::Database, id),
        label: id.to_owned(),
        detail: friendly_name
            .filter(|name| !name.is_empty() && *name != id)
            .map(str::to_owned),
        has_children: true,
        has_definition: false,
        system: false,
    }
}

fn dataset_node(parent: &CatalogPath, dataset: &DatasetEntry) -> CatalogNode {
    let name = &dataset.dataset_reference.dataset_id;
    CatalogNode {
        path: parent.child(NodeKind::Schema, name),
        label: name.clone(),
        detail: dataset.location.clone(),
        has_children: true,
        has_definition: false,
        system: false,
    }
}

fn table_node(parent: &CatalogPath, table: &TableEntry) -> CatalogNode {
    let (kind, what) = match table.kind.as_str() {
        "VIEW" => (NodeKind::View, None),
        "MATERIALIZED_VIEW" => (NodeKind::MaterializedView, None),
        "EXTERNAL" => (NodeKind::Table, Some("external")),
        "SNAPSHOT" => (NodeKind::Table, Some("snapshot")),
        _ => (NodeKind::Table, None),
    };
    let partitioned = table
        .time_partitioning
        .as_ref()
        .map(|partitioning| match &partitioning.field {
            Some(field) => format!(
                "partitioned by {} on {field}",
                partitioning.unit.to_lowercase()
            ),
            None => format!(
                "partitioned by {} of ingestion",
                partitioning.unit.to_lowercase()
            ),
        })
        .or_else(|| {
            table
                .range_partitioning
                .as_ref()
                .map(|partitioning| format!("partitioned by range on {}", partitioning.field))
        });
    let detail: Vec<String> = what
        .map(str::to_owned)
        .into_iter()
        .chain(partitioned)
        .collect();
    let name = &table.table_reference.table_id;
    CatalogNode {
        path: parent.child(kind, name),
        label: name.clone(),
        detail: (!detail.is_empty()).then(|| detail.join(", ")),
        has_children: true,
        has_definition: true,
        system: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(json: &str) -> Table {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_tables_definition_is_written_from_its_schema() {
        let orders = table(
            r#"{"type": "TABLE", "schema": {"fields": [
                {"name": "id", "type": "INTEGER", "mode": "REQUIRED"},
                {"name": "placed", "type": "TIMESTAMP"},
                {"name": "customer", "type": "STRING"},
                {"name": "unit price", "type": "NUMERIC"},
                {"name": "lines", "type": "RECORD", "mode": "REPEATED", "fields": [
                    {"name": "sku", "type": "STRING"}]}]},
               "timePartitioning": {"type": "DAY", "field": "placed"},
               "clustering": {"fields": ["customer", "unit price"]}}"#,
        );
        assert_eq!(
            orders.definition("`analytics.sales.orders`"),
            "CREATE TABLE `analytics.sales.orders`\n(\n  id INT64 NOT NULL,\n  \
             placed TIMESTAMP,\n  customer STRING,\n  `unit price` NUMERIC,\n  \
             lines ARRAY<STRUCT<sku STRING>>\n)\nPARTITION BY DATE(placed)\n\
             CLUSTER BY customer, `unit price`;"
        );
    }

    #[test]
    fn each_kind_of_partitioning_is_written_as_bigquerys_ddl() {
        let partitioned = |partitioning: &str, kind: &str| {
            table(&format!(
                r#"{{"type": "TABLE", "schema": {{"fields": [{{"name": "at", "type": "{kind}"}}]}},
                   {partitioning}}}"#
            ))
            .partition_expression()
        };
        for (partitioning, kind, expression) in [
            (
                r#""timePartitioning": {"type": "DAY", "field": "at"}"#,
                "DATE",
                "at",
            ),
            (
                r#""timePartitioning": {"type": "MONTH", "field": "at"}"#,
                "DATE",
                "DATE_TRUNC(at, MONTH)",
            ),
            (
                r#""timePartitioning": {"type": "HOUR", "field": "at"}"#,
                "TIMESTAMP",
                "TIMESTAMP_TRUNC(at, HOUR)",
            ),
            (
                r#""timePartitioning": {"type": "DAY", "field": "at"}"#,
                "DATETIME",
                "DATETIME_TRUNC(at, DAY)",
            ),
            (
                r#""timePartitioning": {"type": "DAY"}"#,
                "DATE",
                "_PARTITIONDATE",
            ),
            (
                r#""timePartitioning": {"type": "HOUR"}"#,
                "DATE",
                "TIMESTAMP_TRUNC(_PARTITIONTIME, HOUR)",
            ),
            (
                r#""rangePartitioning": {"field": "at", "range": {"start": "0", "end": "100", "interval": "10"}}"#,
                "INTEGER",
                "RANGE_BUCKET(at, GENERATE_ARRAY(0, 100, 10))",
            ),
        ] {
            assert_eq!(
                partitioned(partitioning, kind).as_deref(),
                Some(expression),
                "{partitioning} on {kind}"
            );
        }
    }

    #[test]
    fn views_show_their_own_query() {
        let view = table(r#"{"type": "VIEW", "view": {"query": "SELECT 1 AS one"}}"#);
        assert_eq!(
            view.definition("`p.d.v`"),
            "CREATE VIEW `p.d.v` AS\nSELECT 1 AS one;"
        );
        let materialized =
            table(r#"{"type": "MATERIALIZED_VIEW", "materializedView": {"query": "SELECT 2"}}"#);
        assert_eq!(
            materialized.definition("`p.d.m`"),
            "CREATE MATERIALIZED VIEW `p.d.m` AS\nSELECT 2;"
        );
    }

    #[test]
    fn names_are_quoted_as_googlesql_reads_them() {
        assert_eq!(
            qualified("my-project", "sales", "orders"),
            "`my-project.sales.orders`"
        );
        assert_eq!(qualified("p", "d", "it`s"), "`p.d.it\\`s`");
        for (name, written) in [
            ("total", "total"),
            ("_hidden", "_hidden"),
            ("2024 total", "`2024 total`"),
            ("été", "`été`"),
        ] {
            assert_eq!(column_name(name), written);
        }
    }

    #[test]
    fn named_projects_come_once_and_in_order() {
        let others = [
            "bigquery-public-data".to_owned(),
            "analytics".to_owned(),
            String::new(),
        ];
        let named = NamedProjects {
            own: "analytics",
            others: &others,
        };
        assert_eq!(named.names(), ["analytics", "bigquery-public-data"]);
    }
}
