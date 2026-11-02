//! Catalog introspection from `pragma_database_list`, `sqlite_schema` and the table-valued
//! pragmas. Blocking; called through `SqliteConnection::run_blocking`.
//!
//! The tree is `schema → tables/views → table → columns/indexes/constraints/triggers`, where
//! the schemas are `main` and any attached databases. SQLite keeps the original `CREATE`
//! statement of every table, view, index and trigger, which is what "definition" returns.

use dbrover_core::{
    CatalogMatches, CatalogNode, CatalogPath, CatalogSearch, ChildrenPage, ChildrenQuery,
    DriverError, FolderKind, NodeKind, PageToken, PathSegment, Quoting, like_contains,
};
use rusqlite::{Connection, Params, Row};

const SCHEMA_FOLDERS: &[FolderKind] = &[FolderKind::Tables, FolderKind::Views];

enum Level<'a> {
    Schemas,
    SchemaFolders,
    SchemaObjects {
        schema: &'a str,
        folder: FolderKind,
    },
    RelationFolders {
        relation_kind: NodeKind,
    },
    RelationChildren {
        schema: &'a str,
        relation: &'a str,
        folder: FolderKind,
    },
}

impl<'a> Level<'a> {
    fn of(path: &'a CatalogPath) -> Option<Self> {
        match path.segments() {
            [] => Some(Self::Schemas),
            [schema, rest @ ..] if schema.kind == NodeKind::Schema => {
                Self::within_schema(&schema.name, rest)
            }
            _ => None,
        }
    }

    fn within_schema(schema: &'a str, rest: &'a [PathSegment]) -> Option<Self> {
        match rest {
            [] => Some(Self::SchemaFolders),
            [folder] => folder_kind(folder)
                .filter(|folder| SCHEMA_FOLDERS.contains(folder))
                .map(|folder| Self::SchemaObjects { schema, folder }),
            [folder, relation] if is_relation_in(folder, relation) => Some(Self::RelationFolders {
                relation_kind: relation.kind,
            }),
            [folder, relation, child_folder] if is_relation_in(folder, relation) => {
                folder_kind(child_folder)
                    .filter(|child| relation_folders(relation.kind).contains(child))
                    .map(|folder| Self::RelationChildren {
                        schema,
                        relation: &relation.name,
                        folder,
                    })
            }
            _ => None,
        }
    }
}

fn folder_kind(segment: &PathSegment) -> Option<FolderKind> {
    match segment.kind {
        NodeKind::Folder(folder) => Some(folder),
        _ => None,
    }
}

fn relation_folder(kind: NodeKind) -> Option<FolderKind> {
    match kind {
        NodeKind::Table => Some(FolderKind::Tables),
        NodeKind::View => Some(FolderKind::Views),
        _ => None,
    }
}

fn is_relation_in(folder: &PathSegment, relation: &PathSegment) -> bool {
    relation_folder(relation.kind).is_some()
        && folder_kind(folder) == relation_folder(relation.kind)
}

fn relation_folders(kind: NodeKind) -> &'static [FolderKind] {
    match kind {
        NodeKind::Table => &[
            FolderKind::Columns,
            FolderKind::Indexes,
            FolderKind::Constraints,
            FolderKind::Triggers,
        ],
        NodeKind::View => &[FolderKind::Columns],
        _ => &[],
    }
}

pub(crate) fn children(
    connection: &Connection,
    path: &CatalogPath,
) -> Result<Vec<CatalogNode>, DriverError> {
    let level = Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    match level {
        Level::Schemas => schemas(connection, path),
        Level::SchemaFolders => Ok(folders(path, SCHEMA_FOLDERS)),
        Level::SchemaObjects { schema, folder } => {
            let everything = ChildrenQuery {
                filter: None,
                after: None,
                limit: None,
            };
            Ok(relations(connection, path, schema, folder, &everything)?.nodes)
        }
        Level::RelationFolders { relation_kind } => {
            Ok(folders(path, relation_folders(relation_kind)))
        }
        Level::RelationChildren {
            schema,
            relation,
            folder,
        } => match folder {
            FolderKind::Columns => columns(connection, path, schema, relation),
            FolderKind::Indexes => indexes(connection, path, schema, relation),
            FolderKind::Constraints => foreign_keys(connection, path, schema, relation),
            FolderKind::Triggers => triggers(connection, path, schema, relation),
            _ => Err(DriverError::InvalidPath(path.clone())),
        },
    }
}

fn folders(parent: &CatalogPath, folders: &[FolderKind]) -> Vec<CatalogNode> {
    folders
        .iter()
        .map(|folder| CatalogNode::folder(parent, *folder))
        .collect()
}

fn node(parent: &CatalogPath, kind: NodeKind, name: String) -> CatalogNode {
    CatalogNode {
        path: parent.child(kind, name.clone()),
        label: name,
        detail: None,
        has_children: false,
        has_definition: false,
        system: false,
    }
}

fn sql_error(error: rusqlite::Error) -> DriverError {
    DriverError::Connection(Box::new(error))
}

/// Runs `sql` and maps every row with `read`.
fn rows<T>(
    connection: &Connection,
    sql: &str,
    params: impl Params,
    read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, DriverError> {
    let mut statement = connection.prepare(sql).map_err(sql_error)?;
    statement
        .query_map(params, read)
        .map_err(sql_error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql_error)
}

fn schemas(connection: &Connection, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
    let names = rows(
        connection,
        "SELECT name FROM pragma_database_list ORDER BY seq",
        [],
        |row| row.get::<_, String>(0),
    )?;
    Ok(names
        .into_iter()
        .map(|name| CatalogNode {
            has_children: true,
            system: name == "temp",
            ..node(path, NodeKind::Schema, name)
        })
        .collect())
}

/// Part of the level at `path`, as `query` asks: a schema's tables and views are paged by
/// name, with the filter applied in the query; every other level is listed whole and filtered
/// here.
pub(crate) fn children_page(
    connection: &Connection,
    path: &CatalogPath,
    query: &ChildrenQuery,
) -> Result<ChildrenPage, DriverError> {
    match Level::of(path) {
        Some(Level::SchemaObjects { schema, folder }) => {
            relations(connection, path, schema, folder, query)
        }
        _ => Ok(ChildrenPage::whole(children(connection, path)?, query)),
    }
}

/// A schema's tables or views named after `query`'s token and containing its filter, by name.
/// Keyset paging, as the PostgreSQL driver's: each page continues after the last name of the
/// one before, and one row more than the limit tells whether there is a next page. Names
/// compare and sort with SQLite's default `BINARY` collation, so the two agree; `LIKE` ignores
/// the case of ASCII letters.
fn relations(
    connection: &Connection,
    path: &CatalogPath,
    schema: &str,
    folder: FolderKind,
    query: &ChildrenQuery,
) -> Result<ChildrenPage, DriverError> {
    let (kind, object_type) = match folder {
        FolderKind::Tables => (NodeKind::Table, "table"),
        FolderKind::Views => (NodeKind::View, "view"),
        _ => return Err(DriverError::InvalidPath(path.clone())),
    };
    let sql = format!(
        "SELECT name FROM {}.sqlite_schema WHERE type = ?1 \
         AND (?2 IS NULL OR name > ?2) AND (?3 IS NULL OR name LIKE ?3 ESCAPE '\\') \
         ORDER BY name LIMIT ?4",
        Quoting::STANDARD.identifier_if_needed(schema)
    );
    let after = query.after.as_ref().map(PageToken::as_str);
    let pattern = query.filter.as_deref().map(like_contains);
    // A negative limit is no limit, in SQLite.
    let read = query.limit.map_or(-1, |limit| {
        i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX)
    });
    let mut names = rows(
        connection,
        &sql,
        rusqlite::params![object_type, after, pattern, read],
        |row| row.get::<_, String>(0),
    )?;
    let next = match query.limit {
        Some(limit) if names.len() > limit => {
            names.truncate(limit);
            names.last().cloned().map(PageToken::new)
        }
        _ => None,
    };
    let nodes = names
        .into_iter()
        .map(|name| CatalogNode {
            has_children: true,
            has_definition: true,
            // sqlite_sequence, sqlite_stat1, …: SQLite's own bookkeeping tables.
            system: name.starts_with("sqlite_"),
            ..node(path, kind, name)
        })
        .collect();
    Ok(ChildrenPage { nodes, next })
}

/// The tables and views of every attached schema whose names contain the search's text
/// (C0g), sorted by name, each with the path the explorer lists it under.
pub(crate) fn search(
    connection: &Connection,
    search: &CatalogSearch,
) -> Result<CatalogMatches, DriverError> {
    let schemas = rows(
        connection,
        "SELECT name FROM pragma_database_list ORDER BY seq",
        [],
        |row| row.get::<_, String>(0),
    )?;
    let pattern = like_contains(&search.text);
    let read = i64::try_from(search.limit.saturating_add(1)).unwrap_or(i64::MAX);
    let mut nodes = Vec::new();
    for schema in schemas {
        let sql = format!(
            "SELECT name, type FROM {}.sqlite_schema WHERE type IN ('table', 'view') \
             AND name LIKE ?1 ESCAPE '\\' ORDER BY name LIMIT ?2",
            Quoting::STANDARD.identifier_if_needed(&schema)
        );
        let found = rows(connection, &sql, rusqlite::params![pattern, read], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for (name, object_type) in found {
            let (kind, folder) = match object_type.as_str() {
                "view" => (NodeKind::View, FolderKind::Views),
                _ => (NodeKind::Table, FolderKind::Tables),
            };
            let folder_path = CatalogPath::root()
                .child(NodeKind::Schema, schema.clone())
                .child(NodeKind::Folder(folder), folder.label());
            nodes.push(CatalogNode {
                detail: Some(schema.clone()),
                has_children: true,
                has_definition: true,
                system: schema == "temp" || name.starts_with("sqlite_"),
                ..node(&folder_path, kind, name)
            });
        }
    }
    nodes.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(CatalogMatches::cut(nodes, search.limit))
}

fn columns(
    connection: &Connection,
    path: &CatalogPath,
    schema: &str,
    relation: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let sql = "SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1, ?2) ORDER BY cid";
    rows(connection, sql, [relation, schema], |row| {
        let column_type: String = row.get(1)?;
        let not_null: bool = row.get(2)?;
        let primary_key: bool = row.get::<_, i64>(3)? > 0;
        let mut detail = vec![if column_type.is_empty() {
            "any".to_owned()
        } else {
            column_type.to_lowercase()
        }];
        if primary_key {
            detail.push("primary key".to_owned());
        }
        if not_null {
            detail.push("not null".to_owned());
        }
        Ok(CatalogNode {
            detail: Some(detail.join(", ")),
            ..node(path, NodeKind::Column, row.get(0)?)
        })
    })
}

fn indexes(
    connection: &Connection,
    path: &CatalogPath,
    schema: &str,
    relation: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    // Indexes SQLite creates for PRIMARY KEY and UNIQUE constraints have no SQL of their own.
    let sql = format!(
        "SELECT il.name, il.\"unique\", il.origin, s.sql IS NOT NULL \
         FROM pragma_index_list(?1, ?2) il \
         LEFT JOIN {}.sqlite_schema s ON s.type = 'index' AND s.name = il.name \
         ORDER BY il.name",
        Quoting::STANDARD.identifier_if_needed(schema)
    );
    rows(connection, &sql, [relation, schema], |row| {
        let unique: bool = row.get(1)?;
        let origin: String = row.get(2)?;
        let detail = match (origin.as_str(), unique) {
            ("pk", _) => Some("primary key"),
            (_, true) => Some("unique"),
            _ => None,
        };
        Ok(CatalogNode {
            detail: detail.map(str::to_owned),
            has_definition: row.get(3)?,
            ..node(path, NodeKind::Index, row.get(0)?)
        })
    })
}

/// SQLite's foreign keys have no names, so each is identified by its id within the table and
/// labelled with what it links: `(customer_id) → customers(id)`.
fn foreign_keys(
    connection: &Connection,
    path: &CatalogPath,
    schema: &str,
    relation: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let sql = "SELECT id, \"table\", group_concat(\"from\", ', '), group_concat(\"to\", ', ') \
               FROM pragma_foreign_key_list(?1, ?2) GROUP BY id ORDER BY id";
    rows(connection, sql, [relation, schema], |row| {
        let id: i64 = row.get(0)?;
        let target: String = row.get(1)?;
        let from: String = row.get(2)?;
        // `to` is NULL when the key references the target's primary key implicitly.
        let to: Option<String> = row.get(3)?;
        Ok(CatalogNode {
            label: format!("({from}) → {target}({})", to.unwrap_or_default()),
            detail: Some("foreign key".to_owned()),
            ..node(path, NodeKind::Constraint, id.to_string())
        })
    })
}

fn triggers(
    connection: &Connection,
    path: &CatalogPath,
    schema: &str,
    relation: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let sql = format!(
        "SELECT name FROM {}.sqlite_schema WHERE type = 'trigger' AND tbl_name = ?1 ORDER BY name",
        Quoting::STANDARD.identifier_if_needed(schema)
    );
    let names = rows(connection, &sql, [relation], |row| row.get::<_, String>(0))?;
    Ok(names
        .into_iter()
        .map(|name| CatalogNode {
            has_definition: true,
            ..node(path, NodeKind::Trigger, name)
        })
        .collect())
}

pub(crate) fn definition(
    connection: &Connection,
    path: &CatalogPath,
) -> Result<String, DriverError> {
    let invalid = || DriverError::InvalidPath(path.clone());
    let leaf = path.last().ok_or_else(invalid)?;
    let schema = path.name_of(NodeKind::Schema).ok_or_else(invalid)?;
    let object_type = match leaf.kind {
        NodeKind::Table => "table",
        NodeKind::View => "view",
        NodeKind::Index => "index",
        NodeKind::Trigger => "trigger",
        _ => return Err(invalid()),
    };
    let sql = format!(
        "SELECT sql FROM {}.sqlite_schema WHERE type = ?1 AND name = ?2 AND sql IS NOT NULL",
        Quoting::STANDARD.identifier_if_needed(schema)
    );
    let found = rows(connection, &sql, [object_type, leaf.name.as_str()], |row| {
        row.get::<_, String>(0)
    })?;
    let sql = found.into_iter().next().ok_or_else(invalid)?;
    Ok(format!("{sql};"))
}
