//! The explorer's tree, from Snowflake's `SHOW` commands, which need no running warehouse and
//! which Snowflake doesn't bill (`docs/cloud-data.md`, C5, "What runs without a warehouse"):
//! databases, a database's schemas, a schema's tables, views and stages in a folder each, and a
//! table's or view's columns. Nothing the explorer does resumes a suspended warehouse.

use dbrover_core::{
    CatalogMatches, CatalogNode, CatalogPath, CatalogSearch, ChildrenPage, ChildrenQuery,
    DriverError, FolderKind, Jobs, NodeKind, PageToken, Row,
};

use crate::results::Column;

/// The most rows one `SHOW` is asked for. Snowflake takes at most 10,000 in a `LIMIT`; a
/// thousand is the explorer's own page.
const MAX_SHOWN: usize = 1000;

/// What a path names, by its shape.
enum Level<'a> {
    Databases,
    Schemas {
        database: &'a str,
    },
    /// A schema, whose children are its folders whatever it holds.
    Folders,
    Objects {
        database: &'a str,
        schema: &'a str,
        folder: FolderKind,
    },
    Columns {
        database: &'a str,
        schema: &'a str,
        relation: &'a str,
        kind: NodeKind,
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
            [] => Self::Databases,
            [(NodeKind::Database, database)] => Self::Schemas { database },
            [(NodeKind::Database, _), (NodeKind::Schema, _)] => Self::Folders,
            [
                (NodeKind::Database, database),
                (NodeKind::Schema, schema),
                (NodeKind::Folder(folder), _),
            ] if FOLDERS.contains(folder) => Self::Objects {
                database,
                schema,
                folder: *folder,
            },
            [
                (NodeKind::Database, database),
                (NodeKind::Schema, schema),
                (NodeKind::Folder(_), _),
                (kind @ (NodeKind::Table | NodeKind::View | NodeKind::MaterializedView), relation),
            ] => Self::Columns {
                database,
                schema,
                relation,
                kind: *kind,
            },
            _ => return None,
        })
    }
}

/// A schema's folders, in the order the explorer shows them.
const FOLDERS: [FolderKind; 3] = [FolderKind::Tables, FolderKind::Views, FolderKind::Stages];

/// `name` as a quoted identifier, which Snowflake reads exactly as written: always valid,
/// whatever case or characters the name has.
pub(crate) fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `text` as a string literal.
pub(crate) fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
}

/// The `LIKE` pattern of a filter: the text anywhere in the name. Snowflake's `SHOW … LIKE`
/// ignores case, as the explorer's filter does; a `%` or `_` in the filter widens it, and
/// [`ChildrenQuery::matches`] narrows the answer back.
fn like(filter: Option<&str>) -> String {
    match filter.map(str::trim).filter(|filter| !filter.is_empty()) {
        Some(filter) => format!(" LIKE {}", literal(&format!("%{filter}%"))),
        None => String::new(),
    }
}

/// A `SHOW` answer's rows, with its columns found by name.
pub(crate) struct Shown {
    names: Vec<String>,
    pub rows: Vec<Row>,
}

impl Shown {
    /// `row`'s value of the column `name`; empty where the answer has no such column.
    pub(crate) fn get<'r>(&self, row: &'r Row, name: &str) -> &'r str {
        self.names
            .iter()
            .position(|column| column == name)
            .and_then(|index| row.get(index))
            .and_then(Option::as_deref)
            .unwrap_or_default()
    }
}

/// Runs `statement`, a `SHOW`, and reads all its rows.
pub(crate) async fn show(
    jobs: &std::sync::Arc<Jobs>,
    statement: &str,
) -> Result<Shown, DriverError> {
    let mut cursor = jobs.execute(statement).await?;
    let names = cursor
        .columns()
        .iter()
        .map(|column| column.name.clone())
        .collect();
    let mut rows = Vec::new();
    loop {
        let page = cursor.next_page(MAX_SHOWN).await?;
        if page.is_empty() {
            break;
        }
        rows.extend(page);
    }
    Ok(Shown { names, rows })
}

/// Lists part of a level through a connection's jobs.
pub(crate) struct Lister<'a> {
    pub jobs: &'a std::sync::Arc<Jobs>,
}

impl Lister<'_> {
    /// The nodes of `path`'s level that `query` asks for.
    ///
    /// # Errors
    ///
    /// [`DriverError::InvalidPath`] for a path this driver never lists, and Snowflake's error
    /// when a `SHOW` fails.
    pub(crate) async fn page(
        &self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        let level = Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
        let scope = match level {
            Level::Databases => None,
            Level::Schemas { database } => Some(format!("IN DATABASE {}", quoted(database))),
            Level::Folders => {
                let folders = FOLDERS
                    .iter()
                    .map(|folder| CatalogNode::folder(path, *folder))
                    .collect();
                return Ok(ChildrenPage::whole(folders, query));
            }
            Level::Objects {
                database,
                schema,
                folder: FolderKind::Stages,
            } => {
                // `SHOW STAGES` takes no `LIMIT`, and answers 10,000 at most.
                let statement = format!(
                    "SHOW STAGES{} IN SCHEMA {}.{}",
                    like(query.filter.as_deref()),
                    quoted(database),
                    quoted(schema)
                );
                let shown = show(self.jobs, &statement).await?;
                let nodes = shown
                    .rows
                    .iter()
                    .map(|row| stage_node(path, &shown, row))
                    .collect();
                return Ok(ChildrenPage::whole(nodes, query));
            }
            Level::Objects {
                database, schema, ..
            } => Some(format!("IN SCHEMA {}.{}", quoted(database), quoted(schema))),
            Level::Columns {
                database,
                schema,
                relation,
                kind,
            } => {
                let shown = self.columns(database, schema, relation, kind).await?;
                let nodes = shown
                    .rows
                    .iter()
                    .map(|row| column_node(path, &shown, row))
                    .collect();
                return Ok(ChildrenPage::whole(nodes, query));
            }
        };
        let command = match &level {
            Level::Databases => "DATABASES",
            Level::Schemas { .. } => "SCHEMAS",
            Level::Objects {
                folder: FolderKind::Views,
                ..
            } => "VIEWS",
            _ => "TABLES",
        };
        self.paged(path, command, scope.as_deref(), query).await
    }

    /// A level that `SHOW <command>` pages through with `LIMIT … FROM '<last name>'`.
    async fn paged(
        &self,
        path: &CatalogPath,
        command: &str,
        scope: Option<&str>,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        let limit = query.limit.unwrap_or(usize::MAX);
        let mut after = query.after.as_ref().map(|token| token.as_str().to_owned());
        let mut nodes = Vec::new();
        loop {
            let wanted = limit.saturating_sub(nodes.len()).clamp(1, MAX_SHOWN);
            // Whether `FROM` starts at the name itself or after it isn't documented, so a page
            // after another asks for one row more and drops a row repeating the last name.
            let asked = wanted + usize::from(after.is_some());
            let mut statement = format!("SHOW {command}{}", like(query.filter.as_deref()));
            if let Some(scope) = scope {
                statement = format!("{statement} {scope}");
            }
            statement = format!("{statement} LIMIT {asked}");
            if let Some(after) = &after {
                statement = format!("{statement} FROM {}", literal(after));
            }
            let shown = show(self.jobs, &statement).await?;
            let new: Vec<&Row> = shown
                .rows
                .iter()
                .filter(|row| Some(shown.get(row, "name")) != after.as_deref())
                .collect();
            let taken = &new[..new.len().min(wanted)];
            let more = shown.rows.len() >= asked || taken.len() < new.len();
            let last = taken.last().map(|row| shown.get(row, "name").to_owned());
            nodes.extend(
                taken
                    .iter()
                    .map(|row| object_node(path, &shown, row))
                    .filter(|node| query.matches(&node.label)),
            );
            after = last.filter(|_| more);
            if after.is_none() || nodes.len() >= limit {
                break;
            }
        }
        Ok(ChildrenPage {
            nodes,
            next: after.map(PageToken::new),
        })
    }

    /// `SHOW COLUMNS` of a table or view.
    async fn columns(
        &self,
        database: &str,
        schema: &str,
        relation: &str,
        kind: NodeKind,
    ) -> Result<Shown, DriverError> {
        let object = if kind == NodeKind::Table {
            "TABLE"
        } else {
            "VIEW"
        };
        let statement = format!(
            "SHOW COLUMNS IN {object} {}.{}.{}",
            quoted(database),
            quoted(schema),
            quoted(relation)
        );
        show(self.jobs, &statement).await
    }

    /// What **Show definition** shows for the table or view at `path`, from `SHOW` commands
    /// only: a view's own text, and for a table a `CREATE TABLE` written from its columns and
    /// its clustering key. `GET_DDL` would be exact, but it is a function in a `SELECT`, whose
    /// documentation doesn't say whether it needs a warehouse.
    ///
    /// # Errors
    ///
    /// [`DriverError::InvalidPath`] for anything but a table or view.
    pub(crate) async fn definition(&self, path: &CatalogPath) -> Result<String, DriverError> {
        let Some(Level::Columns {
            database,
            schema,
            relation,
            kind,
        }) = Level::of(path)
        else {
            return Err(DriverError::InvalidPath(path.clone()));
        };
        let command = if kind == NodeKind::Table {
            "TABLES"
        } else {
            "VIEWS"
        };
        let statement = format!(
            "SHOW {command} LIKE {} IN SCHEMA {}.{}",
            literal(&like_exactly(relation)),
            quoted(database),
            quoted(schema)
        );
        let shown = show(self.jobs, &statement).await?;
        let Some(found) = shown
            .rows
            .iter()
            .find(|row| shown.get(row, "name") == relation)
        else {
            return Err(DriverError::InvalidPath(path.clone()));
        };
        let name = format!(
            "{}.{}.{}",
            quoted(database),
            quoted(schema),
            quoted(relation)
        );
        if kind != NodeKind::Table {
            return Ok(shown.get(found, "text").to_owned());
        }
        let columns = self.columns(database, schema, relation, kind).await?;
        Ok(create_table(
            &name,
            &columns,
            shown.get(found, "cluster_by"),
        ))
    }

    /// Tables and views anywhere in the account whose names contain the search's text:
    /// `SHOW OBJECTS … IN ACCOUNT`, one more than the limit, to say whether there were more.
    pub(crate) async fn search(
        &self,
        search: &CatalogSearch,
    ) -> Result<CatalogMatches, DriverError> {
        let statement = format!(
            "SHOW OBJECTS{} IN ACCOUNT LIMIT {}",
            like(Some(&search.text)),
            search.limit + 1
        );
        let shown = show(self.jobs, &statement).await?;
        let needle = search.text.to_lowercase();
        let nodes = shown
            .rows
            .iter()
            .filter(|row| shown.get(row, "name").to_lowercase().contains(&needle))
            .map(|row| {
                let database = shown.get(row, "database_name");
                let schema = shown.get(row, "schema_name");
                let folder = if shown.get(row, "kind").eq_ignore_ascii_case("VIEW") {
                    FolderKind::Views
                } else {
                    FolderKind::Tables
                };
                let parent = CatalogPath::root()
                    .child(NodeKind::Database, database)
                    .child(NodeKind::Schema, schema)
                    .child(NodeKind::Folder(folder), folder.label());
                let mut node = object_node(&parent, &shown, row);
                node.detail = Some(format!("{database}.{schema}"));
                node
            })
            .collect();
        Ok(CatalogMatches::cut(nodes, search.limit))
    }
}

/// A `LIKE` pattern that matches `name` alone: its `%`, `_` and `\` escaped.
pub(crate) fn like_exactly(name: &str) -> String {
    name.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn flag(shown: &Shown, row: &Row, name: &str) -> bool {
    matches!(shown.get(row, name), "Y" | "true" | "ON")
}

/// A database, schema, table or view from its `SHOW` row.
fn object_node(parent: &CatalogPath, shown: &Shown, row: &Row) -> CatalogNode {
    let name = shown.get(row, "name");
    let level = parent.segments().len();
    let (kind, detail, has_children, system) = match level {
        0 => {
            let kind = shown.get(row, "kind");
            let detail = (!kind.is_empty() && !kind.eq_ignore_ascii_case("STANDARD"))
                .then(|| kind.to_lowercase());
            (NodeKind::Database, detail, true, false)
        }
        1 => (
            NodeKind::Schema,
            None,
            true,
            name.eq_ignore_ascii_case("INFORMATION_SCHEMA"),
        ),
        _ => relation(shown, row),
    };
    CatalogNode {
        path: parent.child(kind, name),
        label: name.to_owned(),
        detail,
        has_children,
        has_definition: matches!(
            kind,
            NodeKind::Table | NodeKind::View | NodeKind::MaterializedView
        ),
        system,
    }
}

/// A table's or view's kind and what its row says of it: transient, dynamic, Iceberg,
/// external or secure.
fn relation(shown: &Shown, row: &Row) -> (NodeKind, Option<String>, bool, bool) {
    let kind = shown.get(row, "kind").to_ascii_uppercase();
    let node = match kind.as_str() {
        "VIEW" => NodeKind::View,
        "MATERIALIZED_VIEW" | "MATERIALIZED VIEW" => NodeKind::MaterializedView,
        _ => NodeKind::Table,
    };
    let marks: Vec<&str> = [
        (kind == "TRANSIENT", "transient"),
        (kind == "TEMPORARY", "temporary"),
        (flag(shown, row, "is_dynamic"), "dynamic"),
        (flag(shown, row, "is_iceberg"), "Iceberg"),
        (flag(shown, row, "is_external"), "external"),
        (flag(shown, row, "is_secure"), "secure"),
    ]
    .into_iter()
    .filter_map(|(set, mark)| set.then_some(mark))
    .collect();
    let detail = (!marks.is_empty()).then(|| marks.join(", "));
    (node, detail, true, false)
}

/// A stage: Snowflake's own (internal) or a bucket's (external), with its address.
fn stage_node(parent: &CatalogPath, shown: &Shown, row: &Row) -> CatalogNode {
    let name = shown.get(row, "name");
    let url = shown.get(row, "url");
    let kind = shown.get(row, "type").to_lowercase();
    let detail = match (kind.as_str(), url) {
        ("", "") => None,
        (kind, "") => Some(kind.to_owned()),
        (_, url) => Some(url.to_owned()),
    };
    CatalogNode {
        path: parent.child(NodeKind::Stage, name),
        label: name.to_owned(),
        detail,
        has_children: false,
        has_definition: false,
        system: false,
    }
}

/// A column, its type as Snowflake writes it.
fn column_node(parent: &CatalogPath, shown: &Shown, row: &Row) -> CatalogNode {
    let name = shown.get(row, "column_name");
    CatalogNode {
        path: parent.child(NodeKind::Column, name),
        label: name.to_owned(),
        detail: Some(column_type(shown.get(row, "data_type"))),
        has_children: false,
        has_definition: false,
        system: false,
    }
}

/// `SHOW COLUMNS`' `data_type`, a JSON object (`{"type":"FIXED","precision":38,"scale":0}`), as
/// Snowflake writes the type; the text itself where it isn't one.
fn column_type(data_type: &str) -> String {
    #[derive(serde::Deserialize)]
    struct DataType {
        #[serde(rename = "type")]
        kind: String,
        precision: Option<u32>,
        scale: Option<u32>,
        length: Option<u64>,
    }
    let Ok(parsed) = serde_json::from_str::<DataType>(data_type) else {
        return data_type.to_owned();
    };
    let column = Column {
        name: String::new(),
        kind: parsed.kind.clone(),
        precision: parsed.precision,
        scale: parsed.scale,
    };
    match (parsed.kind.to_ascii_uppercase().as_str(), parsed.length) {
        ("TEXT", Some(length)) => format!("VARCHAR({length})"),
        _ => column.type_name(),
    }
}

fn nullable(data_type: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(data_type)
        .ok()
        .and_then(|value| value["nullable"].as_bool())
        .unwrap_or(true)
}

/// The `CREATE TABLE` a table's columns and clustering key make; Snowflake's own `GET_DDL`
/// would also give its comments and options.
fn create_table(name: &str, columns: &Shown, cluster_by: &str) -> String {
    let lines: Vec<String> = columns
        .rows
        .iter()
        .map(|row| {
            let data_type = columns.get(row, "data_type");
            let mut line = format!(
                "  {} {}",
                quoted(columns.get(row, "column_name")),
                column_type(data_type)
            );
            if !nullable(data_type) {
                line.push_str(" NOT NULL");
            }
            let default = columns.get(row, "default");
            if !default.is_empty() {
                line.push_str(&format!(" DEFAULT {default}"));
            }
            line
        })
        .collect();
    let mut definition = format!("CREATE TABLE {name} (\n{}\n)", lines.join(",\n"));
    // `SHOW TABLES` writes the key as `LINEAR(C1, C2)`.
    let key = cluster_by.trim();
    if !key.is_empty() {
        let key = key.strip_prefix("LINEAR").unwrap_or(key);
        definition.push_str(&format!("\nCLUSTER BY {key}"));
    }
    definition.push(';');
    definition
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_quoted_and_filters_become_patterns() {
        assert_eq!(quoted("Sales \"EU\""), "\"Sales \"\"EU\"\"\"");
        assert_eq!(like(Some("ord")), " LIKE '%ord%'");
        assert_eq!(like(Some(" ")), "");
        assert_eq!(like(Some("o'k")), " LIKE '%o''k%'");
        assert_eq!(like_exactly("A_B%"), "A\\_B\\%");
    }

    #[test]
    fn column_types_read_as_snowflake_writes_them() {
        for (data_type, written) in [
            (
                r#"{"type":"FIXED","precision":38,"scale":0,"nullable":true}"#,
                "NUMBER(38,0)",
            ),
            (
                r#"{"type":"TEXT","length":16777216,"byteLength":16777216,"nullable":true}"#,
                "VARCHAR(16777216)",
            ),
            (
                r#"{"type":"TIMESTAMP_NTZ","precision":0,"scale":9,"nullable":true}"#,
                "TIMESTAMP_NTZ(9)",
            ),
            (r#"{"type":"VARIANT","nullable":true}"#, "VARIANT"),
            ("NUMBER(10,2)", "NUMBER(10,2)"),
        ] {
            assert_eq!(column_type(data_type), written);
        }
    }
}
