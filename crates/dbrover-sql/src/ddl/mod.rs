//! Statements that create and change database objects, written from plain descriptions of
//! what the user asked for: the object helpers' dialogs fill in a spec, and these pure
//! functions turn it into each dialect's SQL, so the SQL is tested without a database.
//!
//! Names in a spec are as the user typed them, never quoted. The builder quotes each one as
//! the dialect requires (see [`quote_name`]), and refuses what the dialect can't do, as its
//! [`Capabilities`](crate::Capabilities) say.

mod alter;
mod index;
mod reserved;
mod role;
mod schema;
mod table;
#[cfg(test)]
mod tests;
mod view;

pub use alter::rebuild_refusal;
pub use view::view_query;

use std::collections::{BTreeMap, BTreeSet};

pub use dbrover_core::ReferentialAction;
use dbrover_core::{
    ColumnDescription, Identity, RoleAttribute, RoleDescription, TableConstraints,
    TableDescription, TableName,
};

use crate::{Dialect, ScramVerifier};

/// A schema object's name. `schema` is `None` for an object in the connection's default
/// schema, which is what the database chooses for an unqualified name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectName {
    pub schema: Option<String>,
    pub name: String,
}

impl ObjectName {
    pub fn new(schema: Option<&str>, name: &str) -> Self {
        Self {
            schema: schema.map(str::to_owned),
            name: name.to_owned(),
        }
    }

    /// `schema.name`, quoted for `dialect`.
    pub fn to_sql(&self, dialect: Dialect) -> String {
        match &self.schema {
            Some(schema) => format!(
                "{}.{}",
                quote_name(dialect, schema),
                quote_name(dialect, &self.name)
            ),
            None => quote_name(dialect, &self.name),
        }
    }

    /// Another object in the same schema.
    fn sibling(&self, name: &str) -> Self {
        Self {
            schema: self.schema.clone(),
            name: name.to_owned(),
        }
    }
}

/// One column of a new table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSpec {
    pub name: String,
    /// As the dialect spells it, e.g. `varchar(255)` or `Nullable(String)`: written as typed.
    pub data_type: String,
    pub nullable: bool,
    /// An SQL expression, written as typed after `DEFAULT`.
    pub default: Option<String>,
    /// Whether the database numbers the column itself; `None` for an ordinary column.
    pub identity: Option<Identity>,
}

impl From<ColumnDescription> for ColumnSpec {
    fn from(column: ColumnDescription) -> Self {
        Self {
            name: column.name,
            data_type: column.data_type,
            nullable: column.nullable,
            default: column.default,
            identity: column.identity,
        }
    }
}

/// A new table: its columns and constraints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSpec {
    pub name: ObjectName,
    pub columns: Vec<ColumnSpec>,
    /// Column names, in key order. Empty for a table without a primary key.
    pub primary_key: Vec<String>,
    pub unique_keys: Vec<UniqueKey>,
    pub foreign_keys: Vec<ForeignKey>,
    pub checks: Vec<Check>,
}

/// An existing table as Modify table loads it: the spec of what it is, and what the database
/// keeps about it that a new table's spec doesn't say but changing it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingTable {
    /// Every constraint in it has the name the database gave it, where it has one.
    pub spec: TableSpec,
    /// The primary key constraint's name, which PostgreSQL and SQL Server drop it by.
    pub primary_key_name: Option<String>,
    /// SQL Server keeps each column's default as a constraint, dropped by name: by column.
    pub default_names: BTreeMap<String, String>,
    /// What a rebuild creates again once the table is back (SQLite): the `CREATE INDEX` and
    /// `CREATE TRIGGER` statements of the table's indexes and triggers. Empty elsewhere.
    pub dependents: Vec<String>,
    /// Why the table can't be rebuilt, where it can't (see [`rebuild_refusal`]).
    pub rebuild_refusal: Option<&'static str>,
}

impl TableAlteration {
    /// Whether the change rebuilds the table, copying its rows, rather than altering it in
    /// place: what SQLite's `ALTER TABLE` can't do.
    pub fn rebuilds(&self, dialect: Dialect) -> bool {
        alter::rebuilds(self, dialect)
    }
}

impl ExistingTable {
    /// The table as the catalog describes it, in `dialect`'s terms: ClickHouse's nullable
    /// types lose their `Nullable(…)`, which says the same as the column's nullability; a
    /// key column is never nullable, as a spec's isn't (SQLite reports its `INTEGER PRIMARY
    /// KEY` as nullable, though it never holds NULL); and each constraint keeps the name the
    /// database gave it.
    pub fn from_catalog(
        description: TableDescription,
        constraints: TableConstraints,
        dialect: Dialect,
    ) -> Self {
        let schema = description.schema.clone();
        let columns = description
            .columns
            .into_iter()
            .map(|column| {
                let mut spec = ColumnSpec::from(column);
                if dialect == Dialect::ClickHouse
                    && let Some(inner) = spec
                        .data_type
                        .strip_prefix("Nullable(")
                        .and_then(|rest| rest.strip_suffix(')'))
                {
                    spec.data_type = inner.to_owned();
                }
                if description.primary_key.contains(&spec.name) {
                    spec.nullable = false;
                }
                spec
            })
            .collect();
        let target = |table: TableName| ObjectName {
            schema: Some(table.schema),
            name: table.name,
        };
        let spec = TableSpec {
            name: ObjectName::new(Some(&schema), &description.name),
            columns,
            primary_key: description.primary_key,
            unique_keys: constraints
                .unique_keys
                .into_iter()
                .map(|key| UniqueKey {
                    name: key.name,
                    columns: key.columns,
                })
                .collect(),
            foreign_keys: constraints
                .foreign_keys
                .into_iter()
                .map(|key| ForeignKey {
                    name: key.name,
                    columns: key.columns,
                    target: target(key.referenced),
                    target_columns: key.referenced_columns,
                    on_delete: key.on_delete,
                    on_update: key.on_update,
                })
                .collect(),
            checks: constraints
                .checks
                .into_iter()
                .map(|check| Check {
                    name: check.name,
                    expression: check.expression,
                })
                .collect(),
        };
        Self {
            spec,
            primary_key_name: constraints.primary_key_name,
            default_names: constraints.default_names,
            dependents: Vec::new(),
            rebuild_refusal: None,
        }
    }
}

impl From<TableSpec> for ExistingTable {
    fn from(spec: TableSpec) -> Self {
        Self {
            spec,
            primary_key_name: None,
            default_names: BTreeMap::new(),
            dependents: Vec::new(),
            rebuild_refusal: None,
        }
    }
}

/// An existing table and what the user wants it to become, which Modify table writes as the
/// statements that change one into the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableAlteration {
    pub table: ExistingTable,
    /// The new definition. A column keeps its data when it has the name it had, or is in
    /// `renamed`; any other column is new, and the table's columns missing here are dropped.
    pub after: TableSpec,
    /// Columns of `after` that had another name in the table: new name → old name. Only
    /// these are renames, so a column is never dropped and added again for having a new
    /// name.
    pub renamed: BTreeMap<String, String>,
    /// How to convert a retyped column's values, as an SQL expression of the old column
    /// (PostgreSQL's and DuckDB's `USING`), by the column's new name.
    pub conversions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniqueKey {
    /// `None` lets the database name it.
    pub name: Option<String>,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: Option<String>,
    pub columns: Vec<String>,
    /// The table it points at, which may be the new table itself.
    pub target: ObjectName,
    /// The columns of `target` each of `columns` refers to, in the same order.
    pub target_columns: Vec<String>,
    pub on_delete: ReferentialAction,
    pub on_update: ReferentialAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: Option<String>,
    /// An SQL condition, written as typed.
    pub expression: String,
}

/// A new index on a table, created in the table's schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSpec {
    pub name: String,
    pub table: ObjectName,
    /// Column names, in index order.
    pub columns: Vec<String>,
    pub unique: bool,
    /// `None` for the dialect's default method.
    pub method: Option<IndexMethod>,
}

/// How an index is organised, where a dialect offers a choice; see
/// [`Capabilities::index_methods`](crate::Capabilities::index_methods).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexMethod {
    BTree,
    Hash,
    /// PostgreSQL's inverted index, for arrays, `jsonb` and full-text search.
    Gin,
    /// PostgreSQL's generalised search tree, for geometry and ranges.
    Gist,
    /// PostgreSQL's block range index, small for large tables whose rows are stored in order.
    Brin,
}

impl IndexMethod {
    /// The method's name as the user picks it.
    pub fn label(self) -> &'static str {
        match self {
            Self::BTree => "B-tree",
            Self::Hash => "Hash",
            Self::Gin => "GIN",
            Self::Gist => "GiST",
            Self::Brin => "BRIN",
        }
    }

    fn keyword(self) -> &'static str {
        match self {
            Self::BTree => "btree",
            Self::Hash => "hash",
            Self::Gin => "gin",
            Self::Gist => "gist",
            Self::Brin => "brin",
        }
    }
}

/// A new view, or an existing view's new query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewSpec {
    pub name: ObjectName,
    pub kind: ViewKind,
    /// The `SELECT`, as typed; a `;` ending it is left out.
    pub query: String,
    /// For a materialized view, whether creating it also runs the query to fill it (the
    /// default), or leaves it empty until it is refreshed (`WITH NO DATA`). A plain view
    /// stores no rows, so this changes nothing for it.
    pub with_data: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewKind {
    /// A saved query, run whenever the view is read.
    View,
    /// A query whose rows are stored, and run again only when the view is refreshed; see
    /// [`Capabilities::materialized_views`](crate::Capabilities::materialized_views).
    Materialized,
}

impl ViewKind {
    /// How DDL names this kind of object: `VIEW` or `MATERIALIZED VIEW`.
    fn keyword(self) -> &'static str {
        match self {
            Self::View => "VIEW",
            Self::Materialized => "MATERIALIZED VIEW",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Materialized => "materialized view",
        }
    }
}

/// A new schema; see [`Capabilities::schema_creation`](crate::Capabilities::schema_creation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaSpec {
    pub name: String,
    /// The role that owns it, where the dialect has one; `None` leaves it to the user who
    /// creates it.
    pub owner: Option<String>,
}

/// A role's settings apart from its name and password: what New role creates, and what Edit
/// compares with the role's description to find what changed. See
/// [`Capabilities::users`](crate::Capabilities::users).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSpec {
    /// The attributes that are on; every other one is off.
    pub attributes: BTreeSet<RoleAttribute>,
    /// How many connections the role may have open at once; `None` for no limit.
    pub connection_limit: Option<u32>,
    /// When the role's password stops working, written as typed and read by the database
    /// as a timestamp (`2027-01-01`, `2027-01-01 12:00+02`); `None` or blank for never.
    pub valid_until: Option<String>,
    /// The roles it is a member of, whose privileges it has.
    pub member_of: BTreeSet<String>,
}

impl From<&RoleDescription> for RoleSpec {
    fn from(role: &RoleDescription) -> Self {
        Self {
            attributes: role.attributes.clone(),
            connection_limit: role.connection_limit,
            valid_until: role.valid_until.clone(),
            member_of: role.member_of.clone(),
        }
    }
}

/// What Edit does to a role's password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordChange {
    Keep,
    /// Replaces it with a verifier hashed on the client.
    Set(ScramVerifier),
    /// Leaves the role without a password (`PASSWORD NULL`), so password logins fail.
    Remove,
}

/// What dropping a role does with the objects it owns in the connected database, which
/// PostgreSQL won't drop a role for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedObjects {
    /// Nothing: the drop fails while the role owns anything.
    Keep,
    /// Gives them to another role.
    ReassignTo(String),
    /// Drops them with the role.
    Drop,
}

/// One change to the database, which may take several statements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DdlOperation {
    CreateTable(TableSpec),
    /// Changes an existing table into a new definition, in place where the dialect can; see
    /// [`Capabilities::table_changes`](crate::Capabilities::table_changes).
    AlterTable(Box<TableAlteration>),
    RenameTable {
        table: ObjectName,
        /// The table stays in its schema.
        new_name: String,
    },
    /// Deletes every row, keeping the table.
    TruncateTable(ObjectName),
    DropTable {
        table: ObjectName,
        /// Also drop what depends on the table, where the dialect can.
        cascade: bool,
    },
    CreateIndex(IndexSpec),
    DropIndex {
        /// The table the index is on, which MySQL and SQL Server name in `DROP INDEX`.
        table: ObjectName,
        /// In the table's schema.
        index: String,
    },
    CreateView(ViewSpec),
    /// Gives an existing view the spec's query; see
    /// [`Capabilities::view_replacement`](crate::Capabilities::view_replacement).
    ReplaceView(ViewSpec),
    RenameView {
        view: ObjectName,
        kind: ViewKind,
        /// The view stays in its schema.
        new_name: String,
    },
    /// Runs a materialized view's query again and stores its rows. `concurrently` keeps the
    /// old rows readable meanwhile, which PostgreSQL allows only for a view with a unique
    /// index.
    RefreshMaterializedView {
        view: ObjectName,
        concurrently: bool,
    },
    DropView {
        view: ObjectName,
        kind: ViewKind,
        /// Also drop what depends on the view, where the dialect can.
        cascade: bool,
    },
    CreateSchema(SchemaSpec),
    DropSchema {
        schema: String,
        /// Also drop what the schema holds, where the dialect can; it refuses a schema that
        /// isn't empty otherwise.
        cascade: bool,
    },
    CreateRole {
        name: String,
        spec: RoleSpec,
        password: Option<ScramVerifier>,
    },
    /// Changes a role from `before`, as its description reads, to `after`.
    AlterRole {
        name: String,
        before: RoleSpec,
        after: RoleSpec,
        password: PasswordChange,
    },
    RenameRole {
        name: String,
        new_name: String,
    },
    DropRole {
        name: String,
        owned: OwnedObjects,
    },
}

/// Why a spec can't be written as SQL. The message is shown in the dialog, which stays open to
/// fix it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DdlError {
    #[error("The {0} needs a name.")]
    MissingName(&'static str),
    #[error("A table needs at least one column.")]
    NoColumns,
    #[error("Column {position} needs a name.")]
    UnnamedColumn { position: usize },
    #[error("Column \u{201c}{column}\u{201d} needs a type.")]
    UntypedColumn { column: String },
    #[error("There are two columns named \u{201c}{0}\u{201d}.")]
    DuplicateColumn(String),
    #[error("The {used_in} names \u{201c}{column}\u{201d}, which isn't a column of the table.")]
    UnknownColumn {
        column: String,
        used_in: &'static str,
    },
    #[error("The {0} needs at least one column.")]
    NoKeyColumns(&'static str),
    #[error("A foreign key needs as many target columns as columns.")]
    ForeignKeyColumnCount,
    #[error("A check needs a condition.")]
    EmptyCheck,
    #[error("The {0} needs a query.")]
    EmptyQuery(&'static str),
    #[error("Nothing has changed yet.")]
    NothingChanged,
    #[error("The two passwords differ.")]
    PasswordsDiffer,
    #[error("Pick the role to give its objects to.")]
    NoNewOwner,
    #[error("A role's objects can't be given to the role itself.")]
    ReassignToItself,
    #[error("{dialect} can't {what}.", dialect = .dialect.display_name())]
    CantAlter {
        what: &'static str,
        dialect: Dialect,
    },
    #[error(
        "SQLite rebuilds the table to make this change, and the rebuilt table would lose that \
         it {0}."
    )]
    Unrebuildable(&'static str),
    #[error("The column \u{201c}{0}\u{201d} is renamed twice.")]
    RenamedTwice(String),
    #[error("The {0} has no name in the database, so it can't be dropped.")]
    UnnamedConstraint(&'static str),
    #[error("{dialect} has no {what}.", dialect = .dialect.display_name())]
    Unsupported {
        what: &'static str,
        dialect: Dialect,
    },
}

impl DdlOperation {
    /// The statements that make this change on `dialect`, in the order to run them, without
    /// a terminating `;`.
    ///
    /// # Errors
    ///
    /// [`DdlError`] for a spec that is incomplete, names a column that isn't there, or asks
    /// for something the dialect can't do.
    pub fn statements(&self, dialect: Dialect) -> Result<Vec<String>, DdlError> {
        match self {
            Self::CreateTable(spec) => table::create(spec, dialect).map(|sql| vec![sql]),
            Self::AlterTable(alteration) => alter::alter(alteration, dialect),
            Self::RenameTable { table, new_name } => {
                table::rename(table, new_name, dialect).map(|sql| vec![sql])
            }
            Self::TruncateTable(table) => Ok(vec![table::truncate(table, dialect)]),
            Self::DropTable { table, cascade } => {
                table::drop(table, *cascade, dialect).map(|sql| vec![sql])
            }
            Self::CreateIndex(spec) => index::create(spec, dialect).map(|sql| vec![sql]),
            Self::DropIndex { table, index } => {
                index::drop(table, index, dialect).map(|sql| vec![sql])
            }
            Self::CreateView(spec) => view::create(spec, dialect).map(|sql| vec![sql]),
            Self::ReplaceView(spec) => view::replace(spec, dialect),
            Self::RenameView {
                view,
                kind,
                new_name,
            } => view::rename_view(view, *kind, new_name, dialect).map(|sql| vec![sql]),
            Self::RefreshMaterializedView { view, concurrently } => {
                view::refresh(view, *concurrently, dialect).map(|sql| vec![sql])
            }
            Self::DropView {
                view,
                kind,
                cascade,
            } => view::drop(view, *kind, *cascade, dialect).map(|sql| vec![sql]),
            Self::CreateSchema(spec) => schema::create(spec, dialect).map(|sql| vec![sql]),
            Self::DropSchema { schema, cascade } => {
                schema::drop(schema, *cascade, dialect).map(|sql| vec![sql])
            }
            Self::CreateRole {
                name,
                spec,
                password,
            } => role::create(name, spec, password.as_ref(), dialect).map(|sql| vec![sql]),
            Self::AlterRole {
                name,
                before,
                after,
                password,
            } => role::alter(name, before, after, password, dialect),
            Self::RenameRole { name, new_name } => {
                role::rename(name, new_name, dialect).map(|sql| vec![sql])
            }
            Self::DropRole { name, owned } => role::drop(name, owned, dialect),
        }
    }
}

/// The statements for `operations`, one after the other.
///
/// # Errors
///
/// The first operation's [`DdlError`].
pub fn ddl_statements(
    operations: &[DdlOperation],
    dialect: Dialect,
) -> Result<Vec<String>, DdlError> {
    if !dialect.writes_ddl() {
        return Err(DdlError::Unsupported {
            what: "DDL DbRover writes",
            dialect,
        });
    }
    let mut statements = Vec::new();
    for operation in operations {
        statements.extend(operation.statements(dialect)?);
    }
    Ok(statements)
}

/// A name as generated DDL writes it: bare when it reads the same bare in every dialect
/// (lowercase, and reserved in none of them), quoted otherwise. Readable SQL matters here,
/// since the review panel teaches it; quoting every name would hide the statement's shape.
pub fn quote_name(dialect: Dialect, name: &str) -> String {
    let quoting = dialect.quoting();
    let bare = quoting.identifier_if_needed(name);
    if bare == name && !reserved::is_reserved(name) {
        bare
    } else {
        quoting.identifier(name)
    }
}

/// Renames a table or view in its schema. `object` is how `ALTER` names the kind of object
/// (`TABLE`, `VIEW`), on the dialects that rename with `ALTER … RENAME TO`.
fn rename(object: &str, old: &ObjectName, new_name: &str, dialect: Dialect) -> String {
    let old_sql = old.to_sql(dialect);
    match dialect {
        // CQL can't rename a table; `ddl_statements` refuses its DDL before this.
        Dialect::Postgres | Dialect::Sqlite | Dialect::DuckDb | Dialect::Cassandra => format!(
            "ALTER {object} {old_sql} RENAME TO {}",
            quote_name(dialect, new_name)
        ),
        Dialect::Mysql | Dialect::ClickHouse => format!(
            "RENAME TABLE {old_sql} TO {}",
            old.sibling(new_name).to_sql(dialect)
        ),
        // `sp_rename` reads its first argument as a (possibly quoted) name, and takes the
        // second literally: brackets there would become part of the name.
        Dialect::SqlServer => {
            let quoting = dialect.quoting();
            format!(
                "EXEC sp_rename {}, {}",
                quoting.string_literal(&old_sql),
                quoting.string_literal(new_name)
            )
        }
    }
}

fn require_name(name: &str, what: &'static str) -> Result<(), DdlError> {
    if name.trim().is_empty() {
        Err(DdlError::MissingName(what))
    } else {
        Ok(())
    }
}

fn column_list(dialect: Dialect, columns: &[String]) -> String {
    let names: Vec<String> = columns
        .iter()
        .map(|column| quote_name(dialect, column))
        .collect();
    format!("({})", names.join(", "))
}
