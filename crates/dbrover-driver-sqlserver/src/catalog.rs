//! Catalog introspection: one query on the `sys` catalog views per tree level, and
//! definitions from `sys.sql_modules`.
//!
//! The tree is databases, their schemas, and each schema's objects. A database's catalog
//! views are read through its name, `[sales].sys.objects`, so every database the login may
//! open can be browsed from one connection; that name is quoted into the SQL, and every
//! other name is a parameter. Definitions come from `sys.sql_modules`, which holds what
//! `OBJECT_DEFINITION` returns but, unlike it, can be read from another database; a
//! constraint's is written from its catalog view (see [`constraint`]).

use dbrover_core::{CatalogNode, CatalogPath, DriverError, FolderKind, NodeKind, PathSegment};
use tiberius::ToSql;

use crate::warehouse::{CatalogSupport, ConstraintKind};
use crate::{SQL_SERVER_QUOTING, SqlClient, column, connection_error, constraint};

/// A listing query's detail column when it has none: a typed `NULL`, because a bare one is an
/// `int` that can't be read as text.
const NO_DETAIL: &str = "CAST(NULL AS nvarchar(1))";

/// A schema's folders, as far as the server has them (C1e).
fn schema_folders(support: &CatalogSupport) -> Vec<FolderKind> {
    [
        (FolderKind::Tables, true),
        (FolderKind::Views, true),
        (FolderKind::Functions, true),
        (FolderKind::Procedures, true),
        (FolderKind::Sequences, support.sequences),
        (FolderKind::Synonyms, support.synonyms),
    ]
    .into_iter()
    .filter_map(|(folder, kept)| kept.then_some(folder))
    .collect()
}

/// Which level of the tree a path points at, with the names its query needs.
enum Level<'a> {
    Databases,
    Schemas {
        database: &'a str,
    },
    SchemaFolders,
    SchemaObjects {
        database: &'a str,
        schema: &'a str,
        folder: FolderKind,
    },
    RelationFolders {
        relation_kind: NodeKind,
    },
    RelationChildren {
        database: &'a str,
        schema: &'a str,
        relation: &'a str,
        folder: FolderKind,
    },
}

impl<'a> Level<'a> {
    fn of(path: &'a CatalogPath, support: &CatalogSupport) -> Option<Self> {
        match path.segments() {
            [] => Some(Self::Databases),
            [database] if database.kind == NodeKind::Database => Some(Self::Schemas {
                database: &database.name,
            }),
            [database, schema, rest @ ..]
                if database.kind == NodeKind::Database && schema.kind == NodeKind::Schema =>
            {
                Self::within_schema(&database.name, &schema.name, rest, support)
            }
            _ => None,
        }
    }

    fn within_schema(
        database: &'a str,
        schema: &'a str,
        rest: &'a [PathSegment],
        support: &CatalogSupport,
    ) -> Option<Self> {
        match rest {
            [] => Some(Self::SchemaFolders),
            [folder] => folder_kind(folder)
                .filter(|folder| schema_folders(support).contains(folder))
                .map(|folder| Self::SchemaObjects {
                    database,
                    schema,
                    folder,
                }),
            [folder, relation] if is_relation_in(folder, relation) => Some(Self::RelationFolders {
                relation_kind: relation.kind,
            }),
            [folder, relation, child_folder] if is_relation_in(folder, relation) => {
                folder_kind(child_folder)
                    .filter(|child| relation_folders(relation.kind, support).contains(child))
                    .map(|folder| Self::RelationChildren {
                        database,
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

fn is_relation_in(folder: &PathSegment, relation: &PathSegment) -> bool {
    let expected = match relation.kind {
        NodeKind::Table => FolderKind::Tables,
        NodeKind::View => FolderKind::Views,
        _ => return false,
    };
    folder_kind(folder) == Some(expected)
}

/// A table's or view's folders, as far as the server has them: a view's indexes are an
/// indexed view's.
fn relation_folders(kind: NodeKind, support: &CatalogSupport) -> Vec<FolderKind> {
    let candidates = match kind {
        NodeKind::Table => [
            (FolderKind::Columns, true),
            (FolderKind::Indexes, support.indexes),
            (FolderKind::Constraints, !support.constraints.is_empty()),
            (FolderKind::Triggers, support.triggers),
        ],
        NodeKind::View => [
            (FolderKind::Columns, true),
            (FolderKind::Indexes, support.indexed_views),
            (FolderKind::Constraints, false),
            (FolderKind::Triggers, false),
        ],
        _ => return Vec::new(),
    };
    candidates
        .into_iter()
        .filter_map(|(folder, kept)| kept.then_some(folder))
        .collect()
}

pub(crate) async fn children(
    client: &mut SqlClient,
    path: &CatalogPath,
    support: &CatalogSupport,
) -> Result<Vec<CatalogNode>, DriverError> {
    let level = Level::of(path, support).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
    match level {
        Level::Databases => databases(client, path, support).await,
        Level::Schemas { database } => schemas(client, path, &catalog_of(database, support)).await,
        Level::SchemaFolders => Ok(folders(path, &schema_folders(support))),
        Level::SchemaObjects {
            database,
            schema,
            folder,
        } => {
            let catalog = catalog_of(database, support);
            schema_objects(client, path, &catalog, schema, folder).await
        }
        Level::RelationFolders { relation_kind } => {
            Ok(folders(path, &relation_folders(relation_kind, support)))
        }
        Level::RelationChildren {
            database,
            schema,
            relation,
            folder,
        } => {
            let object = object_name(database, schema, relation, support);
            let catalog = catalog_of(database, support);
            relation_children(client, path, &catalog, &object, folder, support).await
        }
    }
}

/// Where `database`'s catalog views are: `[sales].sys`, or `sys` on a server whose other
/// databases can't be read by name, where the tree lists only the connected one.
fn catalog_of(database: &str, support: &CatalogSupport) -> String {
    if support.other_databases {
        format!("{}.sys", SQL_SERVER_QUOTING.identifier(database))
    } else {
        "sys".to_owned()
    }
}

/// An object's name for `OBJECT_ID`: three parts, or two where only the connected database
/// can be read.
fn object_name(database: &str, schema: &str, name: &str, support: &CatalogSupport) -> String {
    if support.other_databases {
        three_part_name(database, schema, name)
    } else {
        format!(
            "{}.{}",
            SQL_SERVER_QUOTING.identifier(schema),
            SQL_SERVER_QUOTING.identifier(name)
        )
    }
}

fn folders(parent: &CatalogPath, folders: &[FolderKind]) -> Vec<CatalogNode> {
    folders
        .iter()
        .map(|folder| CatalogNode::folder(parent, *folder))
        .collect()
}

/// `[database].[schema].[name]`, for `OBJECT_ID`.
fn three_part_name(database: &str, schema: &str, name: &str) -> String {
    format!(
        "{}.{}.{}",
        SQL_SERVER_QUOTING.identifier(database),
        SQL_SERVER_QUOTING.identifier(schema),
        SQL_SERVER_QUOTING.identifier(name)
    )
}

/// One catalog row, as every listing query returns it: the name, whether it is a system
/// object (`1` or `0`), whether it has a definition (`1` or `0`), and a detail or
/// [`NO_DETAIL`].
struct Listed {
    name: String,
    system: bool,
    has_definition: bool,
    detail: Option<String>,
}

async fn list(
    client: &mut SqlClient,
    sql: &str,
    params: &[&dyn ToSql],
) -> Result<Vec<Listed>, DriverError> {
    let rows = client
        .query(sql, params)
        .await
        .map_err(connection_error)?
        .into_first_result()
        .await
        .map_err(connection_error)?;
    rows.iter()
        .map(|row| {
            let name: Option<&str> = row.try_get(0).map_err(connection_error)?;
            let flag = |index: usize| -> Result<bool, DriverError> {
                let value: Option<i32> = row.try_get(index).map_err(connection_error)?;
                Ok(value == Some(1))
            };
            let detail: Option<&str> = row.try_get(3).map_err(connection_error)?;
            Ok(Listed {
                name: name
                    .ok_or_else(|| connection_error("catalog row without a name"))?
                    .to_owned(),
                system: flag(1)?,
                has_definition: flag(2)?,
                detail: detail.map(str::to_owned),
            })
        })
        .collect()
}

fn nodes(
    parent: &CatalogPath,
    kind: NodeKind,
    has_children: bool,
    listed: Vec<Listed>,
) -> Vec<CatalogNode> {
    listed
        .into_iter()
        .map(|listed| CatalogNode {
            path: parent.child(kind, listed.name.clone()),
            label: listed.name,
            detail: listed.detail,
            has_children,
            has_definition: listed.has_definition,
            system: listed.system,
        })
        .collect()
}

/// The databases this login may open, which are online. `master`, `model`, `msdb` and
/// `tempdb` (ids 1 to 4) are the server's own.
async fn databases(
    client: &mut SqlClient,
    path: &CatalogPath,
    support: &CatalogSupport,
) -> Result<Vec<CatalogNode>, DriverError> {
    let which = if support.other_databases {
        "HAS_DBACCESS(name) = 1"
    } else {
        "name = DB_NAME()"
    };
    let sql = format!(
        "SELECT name, CASE WHEN database_id <= 4 THEN 1 ELSE 0 END, 0, {NO_DETAIL} \
         FROM sys.databases WHERE state = 0 AND {which} ORDER BY name"
    );
    let listed = list(client, &sql, &[]).await?;
    Ok(nodes(path, NodeKind::Database, true, listed))
}

/// A database's schemas. `sys`, `INFORMATION_SCHEMA` and `guest` belong to the server, and
/// so do the schemas of the fixed database roles (`db_owner` and the like), whose ids start
/// at 16384.
async fn schemas(
    client: &mut SqlClient,
    path: &CatalogPath,
    catalog: &str,
) -> Result<Vec<CatalogNode>, DriverError> {
    let sql = format!(
        "SELECT name, CASE WHEN name IN ('sys', 'INFORMATION_SCHEMA', 'guest') \
         OR schema_id >= 16384 THEN 1 ELSE 0 END, 0, {NO_DETAIL} \
         FROM {catalog}.schemas ORDER BY name"
    );
    let listed = list(client, &sql, &[]).await?;
    Ok(nodes(path, NodeKind::Schema, true, listed))
}

async fn schema_objects(
    client: &mut SqlClient,
    path: &CatalogPath,
    catalog: &str,
    schema: &str,
    folder: FolderKind,
) -> Result<Vec<CatalogNode>, DriverError> {
    let objects = |types: &str| {
        format!(
            "SELECT o.name, CASE WHEN o.is_ms_shipped = 1 THEN 1 ELSE 0 END, \
             CASE WHEN m.definition IS NULL THEN 0 ELSE 1 END, {NO_DETAIL} \
             FROM {catalog}.objects o \
             JOIN {catalog}.schemas s ON s.schema_id = o.schema_id \
             LEFT JOIN {catalog}.sql_modules m ON m.object_id = o.object_id \
             WHERE s.name = @P1 AND o.type IN ({types}) ORDER BY o.name"
        )
    };
    let (kind, sql) = match folder {
        FolderKind::Tables => (NodeKind::Table, objects("'U'")),
        FolderKind::Views => (NodeKind::View, objects("'V'")),
        // Scalar, inline and multi-statement table functions, then their CLR versions.
        FolderKind::Functions => (NodeKind::Function, objects("'FN', 'IF', 'TF', 'FS', 'FT'")),
        FolderKind::Procedures => (NodeKind::Procedure, objects("'P', 'PC'")),
        FolderKind::Sequences => (
            NodeKind::Sequence,
            format!(
                "SELECT q.name, 0, 0, t.name FROM {catalog}.sequences q \
                 JOIN {catalog}.schemas s ON s.schema_id = q.schema_id \
                 JOIN {catalog}.types t ON t.user_type_id = q.user_type_id \
                 WHERE s.name = @P1 ORDER BY q.name"
            ),
        ),
        FolderKind::Synonyms => (
            NodeKind::Synonym,
            format!(
                "SELECT y.name, 0, 1, y.base_object_name FROM {catalog}.synonyms y \
                 JOIN {catalog}.schemas s ON s.schema_id = y.schema_id \
                 WHERE s.name = @P1 ORDER BY y.name"
            ),
        ),
        _ => return Err(DriverError::InvalidPath(path.clone())),
    };
    let is_relation = matches!(kind, NodeKind::Table | NodeKind::View);
    let listed = list(client, &sql, &[&schema]).await?;
    Ok(nodes(path, kind, is_relation, listed))
}

async fn relation_children(
    client: &mut SqlClient,
    path: &CatalogPath,
    catalog: &str,
    object: &str,
    folder: FolderKind,
    support: &CatalogSupport,
) -> Result<Vec<CatalogNode>, DriverError> {
    let (kind, sql) = match folder {
        FolderKind::Columns => (NodeKind::Column, columns_query(catalog)),
        // Included columns are left out: they are stored in the index, but not keys of it.
        // `type_desc` has a collation of its own, which can't be joined to names without
        // naming one.
        FolderKind::Indexes => (
            NodeKind::Index,
            format!(
                "SELECT i.name, 0, 0, CASE WHEN i.is_unique = 1 THEN 'unique ' ELSE '' END \
                 + LOWER(i.type_desc) COLLATE DATABASE_DEFAULT + ' (' + STRING_AGG(c.name, ', ') \
                 WITHIN GROUP (ORDER BY ic.key_ordinal) + ')' \
                 FROM {catalog}.indexes i \
                 JOIN {catalog}.index_columns ic \
                 ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
                 AND ic.is_included_column = 0 \
                 JOIN {catalog}.columns c \
                 ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
                 WHERE i.object_id = OBJECT_ID(@P1) AND i.name IS NOT NULL \
                 GROUP BY i.name, i.is_unique, i.type_desc ORDER BY i.name"
            ),
        ),
        FolderKind::Constraints => (
            NodeKind::Constraint,
            constraints_query(catalog, support.constraints),
        ),
        FolderKind::Triggers => (
            NodeKind::Trigger,
            format!(
                "SELECT tr.name, 0, 1, \
                 CASE WHEN tr.is_instead_of_trigger = 1 THEN 'instead of ' ELSE 'after ' END \
                 + LOWER(STRING_AGG(te.type_desc, ' or ')) COLLATE DATABASE_DEFAULT \
                 FROM {catalog}.triggers tr \
                 JOIN {catalog}.trigger_events te ON te.object_id = tr.object_id \
                 WHERE tr.parent_id = OBJECT_ID(@P1) \
                 GROUP BY tr.name, tr.is_instead_of_trigger ORDER BY tr.name"
            ),
        ),
        _ => return Err(DriverError::InvalidPath(path.clone())),
    };
    let listed = list(client, &sql, &[&object]).await?;
    Ok(nodes(path, kind, false, listed))
}

/// A table's constraints of the kinds the server keeps, each with what kind it is.
fn constraints_query(catalog: &str, kinds: &[ConstraintKind]) -> String {
    let selects: Vec<String> = kinds
        .iter()
        .map(|kind| match kind {
            ConstraintKind::Key => format!(
                "SELECT name, CASE type WHEN 'PK' THEN 'primary key' ELSE 'unique' END AS kind \
                 FROM {catalog}.key_constraints WHERE parent_object_id = OBJECT_ID(@P1)"
            ),
            ConstraintKind::Foreign => format!(
                "SELECT name, 'foreign key' FROM {catalog}.foreign_keys \
                 WHERE parent_object_id = OBJECT_ID(@P1)"
            ),
            ConstraintKind::Check => format!(
                "SELECT name, 'check' FROM {catalog}.check_constraints \
                 WHERE parent_object_id = OBJECT_ID(@P1)"
            ),
            ConstraintKind::Default => format!(
                "SELECT name, 'default' FROM {catalog}.default_constraints \
                 WHERE parent_object_id = OBJECT_ID(@P1)"
            ),
        })
        .collect();
    format!(
        "SELECT name, 0, 1, kind FROM ({}) constraints ORDER BY name",
        selects.join(" UNION ALL ")
    )
}

/// Each column with its type as `CREATE TABLE` writes it (`nvarchar(50)`, `decimal(10, 2)`,
/// `varchar(max)`), and `not null` where it applies. `max_length` counts bytes, two per
/// character in the `n` types.
fn columns_query(catalog: &str) -> String {
    format!(
        "SELECT c.name, 0, 0, t.name + CASE \
         WHEN t.name IN ('varchar', 'char', 'varbinary', 'binary') THEN '(' + \
         CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length AS varchar(10)) END + ')' \
         WHEN t.name IN ('nvarchar', 'nchar') THEN '(' + \
         CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length / 2 AS varchar(10)) END \
         + ')' \
         WHEN t.name IN ('decimal', 'numeric') THEN '(' + CAST(c.precision AS varchar(10)) \
         + ', ' + CAST(c.scale AS varchar(10)) + ')' \
         WHEN t.name IN ('datetime2', 'time', 'datetimeoffset') THEN '(' \
         + CAST(c.scale AS varchar(10)) + ')' \
         ELSE '' END + CASE WHEN c.is_nullable = 0 THEN ', not null' ELSE '' END \
         FROM {catalog}.columns c JOIN {catalog}.types t ON t.user_type_id = c.user_type_id \
         WHERE c.object_id = OBJECT_ID(@P1) ORDER BY c.column_id"
    )
}

pub(crate) async fn definition(
    client: &mut SqlClient,
    path: &CatalogPath,
    support: &CatalogSupport,
) -> Result<String, DriverError> {
    // Only paths `children` returns: the level above must be a valid one.
    let valid_parent = path
        .parent()
        .is_some_and(|parent| Level::of(&parent, support).is_some());
    let invalid = || DriverError::InvalidPath(path.clone());
    let (Some(object), Some(database), Some(schema)) = (
        path.last(),
        path.name_of(NodeKind::Database),
        path.name_of(NodeKind::Schema),
    ) else {
        return Err(invalid());
    };
    if !valid_parent {
        return Err(invalid());
    }
    let catalog = catalog_of(database, support);
    let name = object_name(database, schema, &object.name, support);
    if object.kind == NodeKind::Constraint {
        return constraint::definition(client, &catalog, &name).await;
    }
    let sql = match object.kind {
        NodeKind::View | NodeKind::Function | NodeKind::Procedure | NodeKind::Trigger => {
            format!("SELECT definition FROM {catalog}.sql_modules WHERE object_id = OBJECT_ID(@P1)")
        }
        NodeKind::Synonym => format!(
            "SELECT 'CREATE SYNONYM ' + QUOTENAME(s.name) + '.' + QUOTENAME(y.name) \
             + ' FOR ' + y.base_object_name + ';' FROM {catalog}.synonyms y \
             JOIN {catalog}.schemas s ON s.schema_id = y.schema_id \
             WHERE y.object_id = OBJECT_ID(@P1)"
        ),
        _ => return Err(invalid()),
    };
    let row = client
        .query(sql, &[&name])
        .await
        .map_err(connection_error)?
        .into_row()
        .await
        .map_err(connection_error)?;
    let definition: Option<&str> = row.as_ref().and_then(|row| column(row, 0));
    // `NULL` for a module created `WITH ENCRYPTION`, or that the login may run but not read.
    definition.map(str::to_owned).ok_or_else(|| {
        connection_error(format!(
            "the server shows no definition of {} to this login",
            object.name
        ))
    })
}
