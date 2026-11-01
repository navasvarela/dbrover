//! What each dialect's DDL can do, for the object helpers: their menus and dialogs read these
//! facts instead of checking which engine they talk to, and the DDL builder refuses what a
//! dialect lacks.

use super::Dialect;
use crate::ddl::{IndexMethod, ReferentialAction};

/// What a dialect can create and change. See [`Dialect::capabilities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Whether DDL runs inside a transaction, so that several statements are applied all or
    /// none. MySQL commits each one as it runs; ClickHouse has no transactions.
    pub transactional_ddl: bool,
    /// `DROP TABLE … CASCADE` and `DROP VIEW … CASCADE`, which also drop the views and keys
    /// that depend on the object.
    pub drop_cascade: bool,
    pub unique_keys: bool,
    pub foreign_keys: bool,
    /// What a foreign key may do when the row it points at is deleted or updated. Empty
    /// without foreign keys; otherwise [`ReferentialAction::NoAction`] comes first, as the
    /// default every dialect has.
    pub referential_actions: &'static [ReferentialAction],
    /// `CREATE INDEX` and `DROP INDEX`. ClickHouse's data-skipping indexes are another kind
    /// of object, with a type and a granularity, so it has none here.
    pub indexes: bool,
    /// The methods `CREATE INDEX` may name; empty where the dialect has only one.
    pub index_methods: &'static [IndexMethod],
    /// The changes an existing table takes in place.
    pub table_changes: &'static [TableChange],
    /// Whether a change `table_changes` lacks is made by building the table again and
    /// copying its rows (SQLite), rather than refused.
    pub rebuilds_tables: bool,
    /// How DDL switches foreign keys off around its transaction, where it has to (SQLite).
    pub foreign_key_switch: Option<ForeignKeySwitch>,
    /// Whether a column's new type can say how to convert its values (`USING`).
    pub converts_with_using: bool,
    /// Whether `ADD COLUMN` only takes a column outside the primary key that takes `NULL` or
    /// has a default, for the rows already there (SQLite).
    pub restricted_add_column: bool,
    /// Whether Copy DDL rebuilds a table's `CREATE TABLE` from its description and the
    /// constraints in the catalog, because the database keeps no such statement (PostgreSQL,
    /// SQL Server). Elsewhere the catalog's table definition is the statement.
    pub rebuilds_table_ddl: bool,
    /// Column types the New table dialog suggests, in the order it lists them. Any other
    /// type can be typed in.
    pub common_types: &'static [&'static str],
    /// Materialized views DbRover creates, edits and refreshes. ClickHouse's materialized
    /// views are another kind of object (a trigger that writes each insert into a table),
    /// so it has none here.
    pub materialized_views: bool,
    /// How Edit view gives an existing view a new query.
    pub view_replacement: ViewReplacement,
    /// Whether a view can be renamed in place. SQLite's `ALTER TABLE` refuses views.
    pub renames_views: bool,
    /// What New schema creates.
    pub schema_creation: SchemaCreation,
    /// `CREATE SCHEMA … AUTHORIZATION`, which names the role that owns the new schema.
    pub schema_owner: bool,
    /// `DROP SCHEMA … CASCADE`, which also drops everything in the schema.
    pub drop_schema_cascade: bool,
    /// How the engine models the users who log in to it, which decides whether the explorer
    /// lists them and offers New user.
    pub users: UserModel,
}

/// The statements that switch foreign keys off for DDL and check them after, where a dialect
/// needs them (SQLite).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForeignKeySwitch {
    /// Answers one row, `1` while foreign keys are enforced.
    pub query: &'static str,
    pub off: &'static str,
    pub on: &'static str,
    /// Answers a row for each row whose foreign key points at nothing.
    pub check: &'static str,
}

/// How a dialect's users are modelled, for the users and roles helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserModel {
    /// None DbRover manages yet: SQLite has no users, and the other engines' kinds of users
    /// (MySQL's `'name'@'host'` accounts, SQL Server's logins) are variants of their own when
    /// they are built.
    Unsupported,
    /// PostgreSQL's roles, which belong to the whole server: a user is a role that may log
    /// in, so one kind of object serves both.
    Roles,
}

/// How a dialect replaces an existing view's query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewReplacement {
    /// `CREATE OR REPLACE VIEW`, which keeps what depends on the view.
    OrReplace,
    /// `ALTER VIEW … AS` (SQL Server), which keeps the view's permissions.
    Alter,
    /// `DROP VIEW` then `CREATE VIEW`, inside one transaction (SQLite, which has neither).
    DropAndCreate,
}

/// What a dialect's New schema creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaCreation {
    /// None: SQLite's schemas are database files attached to the connection.
    Unsupported,
    /// `CREATE SCHEMA`, inside the database connected to.
    CreateSchema,
    /// `CREATE DATABASE`: the schemas in the tree are the server's databases (MySQL, whose
    /// `CREATE SCHEMA` is another name for it, and ClickHouse). Dropping one drops everything
    /// in it.
    CreateDatabase,
}

/// A change to an existing table that the dialect makes without rebuilding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TableChange {
    RenameTable,
    RenameColumn,
    AddColumn,
    DropColumn,
    ChangeColumnType,
    ChangeNullability,
    ChangeDefault,
    /// Drops the primary key, adds one, or both.
    ChangePrimaryKey,
    /// Adds a unique key, foreign key or check.
    AddConstraint,
    /// Drops a unique key, foreign key or check.
    DropConstraint,
}

impl TableChange {
    /// What a dialect without it can't do, completing "{dialect} can't …".
    pub fn refusal(self) -> &'static str {
        match self {
            Self::RenameTable => "rename a table",
            Self::RenameColumn => "rename a column",
            Self::AddColumn => "add this column to an existing table",
            Self::DropColumn => "drop a column",
            Self::ChangeColumnType => "change an existing column's type",
            Self::ChangeNullability => "change whether an existing column takes NULL",
            Self::ChangeDefault => "change an existing column's default",
            Self::ChangePrimaryKey => "change an existing table's primary key",
            Self::AddConstraint => "add keys or checks to an existing table",
            Self::DropConstraint => "drop keys or checks from an existing table",
        }
    }
}

const ALL_TABLE_CHANGES: &[TableChange] = &[
    TableChange::RenameTable,
    TableChange::RenameColumn,
    TableChange::AddColumn,
    TableChange::DropColumn,
    TableChange::ChangeColumnType,
    TableChange::ChangeNullability,
    TableChange::ChangeDefault,
    TableChange::ChangePrimaryKey,
    TableChange::AddConstraint,
    TableChange::DropConstraint,
];

const ALL_REFERENTIAL_ACTIONS: &[ReferentialAction] = &[
    ReferentialAction::NoAction,
    ReferentialAction::Restrict,
    ReferentialAction::Cascade,
    ReferentialAction::SetNull,
    ReferentialAction::SetDefault,
];

const POSTGRES: Capabilities = Capabilities {
    transactional_ddl: true,
    drop_cascade: true,
    unique_keys: true,
    foreign_keys: true,
    referential_actions: ALL_REFERENTIAL_ACTIONS,
    indexes: true,
    index_methods: &[
        IndexMethod::BTree,
        IndexMethod::Hash,
        IndexMethod::Gin,
        IndexMethod::Gist,
        IndexMethod::Brin,
    ],
    table_changes: ALL_TABLE_CHANGES,
    rebuilds_tables: false,
    foreign_key_switch: None,
    converts_with_using: true,
    restricted_add_column: false,
    rebuilds_table_ddl: true,
    common_types: &[
        "integer",
        "bigint",
        "smallint",
        "serial",
        "bigserial",
        "numeric(12, 2)",
        "real",
        "double precision",
        "boolean",
        "text",
        "varchar(255)",
        "char(1)",
        "date",
        "time",
        "timestamp",
        "timestamptz",
        "interval",
        "uuid",
        "json",
        "jsonb",
        "bytea",
    ],
    materialized_views: true,
    view_replacement: ViewReplacement::OrReplace,
    renames_views: true,
    schema_creation: SchemaCreation::CreateSchema,
    schema_owner: true,
    drop_schema_cascade: true,
    users: UserModel::Roles,
};

/// SQLite enforces foreign keys only while switched on, and can't switch them inside a
/// transaction. With them on, dropping the table a rebuild replaces would run the `ON DELETE`
/// actions of other tables' keys on their rows.
const SQLITE_FOREIGN_KEYS: ForeignKeySwitch = ForeignKeySwitch {
    query: "PRAGMA foreign_keys",
    off: "PRAGMA foreign_keys = OFF",
    on: "PRAGMA foreign_keys = ON",
    check: "PRAGMA foreign_key_check",
};

/// SQLite's `ALTER TABLE` renames tables and columns, adds a column that is nullable or has a
/// default and isn't in a key, and drops columns; anything else rebuilds the table. `TRUNCATE` is written as `DELETE FROM`.
const SQLITE: Capabilities = Capabilities {
    transactional_ddl: true,
    drop_cascade: false,
    unique_keys: true,
    foreign_keys: true,
    referential_actions: ALL_REFERENTIAL_ACTIONS,
    indexes: true,
    index_methods: &[],
    table_changes: &[
        TableChange::RenameTable,
        TableChange::RenameColumn,
        TableChange::AddColumn,
        TableChange::DropColumn,
    ],
    rebuilds_tables: true,
    foreign_key_switch: Some(SQLITE_FOREIGN_KEYS),
    converts_with_using: false,
    restricted_add_column: true,
    rebuilds_table_ddl: false,
    common_types: &["integer", "real", "text", "blob", "numeric"],
    materialized_views: false,
    view_replacement: ViewReplacement::DropAndCreate,
    renames_views: false,
    schema_creation: SchemaCreation::Unsupported,
    schema_owner: false,
    drop_schema_cascade: false,
    users: UserModel::Unsupported,
};

/// InnoDB parses `SET DEFAULT` but refuses the table, so it isn't offered. `CASCADE` after
/// `DROP TABLE` is accepted and does nothing.
const MYSQL: Capabilities = Capabilities {
    transactional_ddl: false,
    drop_cascade: false,
    unique_keys: true,
    foreign_keys: true,
    referential_actions: &[
        ReferentialAction::NoAction,
        ReferentialAction::Restrict,
        ReferentialAction::Cascade,
        ReferentialAction::SetNull,
    ],
    indexes: true,
    index_methods: &[IndexMethod::BTree, IndexMethod::Hash],
    table_changes: ALL_TABLE_CHANGES,
    rebuilds_tables: false,
    foreign_key_switch: None,
    converts_with_using: false,
    restricted_add_column: false,
    rebuilds_table_ddl: false,
    common_types: &[
        "int",
        "bigint",
        "smallint",
        "tinyint",
        "decimal(12, 2)",
        "float",
        "double",
        "boolean",
        "varchar(255)",
        "char(1)",
        "text",
        "longtext",
        "date",
        "datetime",
        "timestamp",
        "time",
        "json",
        "blob",
        "binary(16)",
    ],
    materialized_views: false,
    view_replacement: ViewReplacement::OrReplace,
    renames_views: true,
    schema_creation: SchemaCreation::CreateDatabase,
    schema_owner: false,
    drop_schema_cascade: false,
    users: UserModel::Unsupported,
};

/// SQL Server has no `RESTRICT`: `NO ACTION` is what refuses. Its renames are `sp_rename`
/// calls rather than `ALTER TABLE`, but they change the table in place all the same.
const SQL_SERVER: Capabilities = Capabilities {
    transactional_ddl: true,
    drop_cascade: false,
    unique_keys: true,
    foreign_keys: true,
    referential_actions: &[
        ReferentialAction::NoAction,
        ReferentialAction::Cascade,
        ReferentialAction::SetNull,
        ReferentialAction::SetDefault,
    ],
    indexes: true,
    index_methods: &[],
    table_changes: ALL_TABLE_CHANGES,
    rebuilds_tables: false,
    foreign_key_switch: None,
    converts_with_using: false,
    restricted_add_column: false,
    rebuilds_table_ddl: true,
    common_types: &[
        "int",
        "bigint",
        "smallint",
        "tinyint",
        "bit",
        "decimal(12, 2)",
        "float",
        "real",
        "money",
        "nvarchar(255)",
        "nvarchar(max)",
        "varchar(255)",
        "char(1)",
        "date",
        "datetime2",
        "datetimeoffset",
        "time",
        "uniqueidentifier",
        "varbinary(max)",
    ],
    materialized_views: false,
    view_replacement: ViewReplacement::Alter,
    renames_views: true,
    schema_creation: SchemaCreation::CreateSchema,
    schema_owner: true,
    drop_schema_cascade: false,
    users: UserModel::Unsupported,
};

/// DuckDB's foreign keys only refuse: it has no `ON DELETE` or `ON UPDATE` actions. Its
/// `ALTER TABLE` can't add or drop constraints, nor change the primary key.
const DUCKDB: Capabilities = Capabilities {
    transactional_ddl: true,
    drop_cascade: false,
    unique_keys: true,
    foreign_keys: true,
    referential_actions: &[ReferentialAction::NoAction],
    indexes: true,
    index_methods: &[],
    table_changes: &[
        TableChange::RenameTable,
        TableChange::RenameColumn,
        TableChange::AddColumn,
        TableChange::DropColumn,
        TableChange::ChangeColumnType,
        TableChange::ChangeNullability,
        TableChange::ChangeDefault,
    ],
    rebuilds_tables: false,
    foreign_key_switch: None,
    converts_with_using: true,
    restricted_add_column: false,
    rebuilds_table_ddl: false,
    common_types: &[
        "integer",
        "bigint",
        "smallint",
        "hugeint",
        "decimal(18, 3)",
        "double",
        "real",
        "boolean",
        "varchar",
        "date",
        "time",
        "timestamp",
        "timestamptz",
        "interval",
        "uuid",
        "json",
        "blob",
    ],
    materialized_views: false,
    view_replacement: ViewReplacement::OrReplace,
    renames_views: true,
    schema_creation: SchemaCreation::CreateSchema,
    schema_owner: false,
    drop_schema_cascade: true,
    users: UserModel::Unsupported,
};

/// ClickHouse has no unique or foreign keys, only checks; its `PRIMARY KEY` is the sort
/// order, which the builder writes as `ORDER BY`.
const CLICKHOUSE: Capabilities = Capabilities {
    transactional_ddl: false,
    drop_cascade: false,
    unique_keys: false,
    foreign_keys: false,
    referential_actions: &[],
    indexes: false,
    index_methods: &[],
    // Checks are the only constraints it has; its key is the order rows are stored in.
    table_changes: &[
        TableChange::RenameTable,
        TableChange::RenameColumn,
        TableChange::AddColumn,
        TableChange::DropColumn,
        TableChange::ChangeColumnType,
        TableChange::ChangeNullability,
        TableChange::ChangeDefault,
        TableChange::AddConstraint,
        TableChange::DropConstraint,
    ],
    rebuilds_tables: false,
    foreign_key_switch: None,
    converts_with_using: false,
    restricted_add_column: false,
    rebuilds_table_ddl: false,
    common_types: &[
        "UInt8",
        "UInt32",
        "UInt64",
        "Int32",
        "Int64",
        "Float32",
        "Float64",
        "Decimal(18, 4)",
        "Bool",
        "String",
        "LowCardinality(String)",
        "FixedString(16)",
        "UUID",
        "Date",
        "Date32",
        "DateTime",
        "DateTime64(3)",
        "Array(String)",
        "Map(String, String)",
    ],
    materialized_views: false,
    view_replacement: ViewReplacement::OrReplace,
    renames_views: true,
    schema_creation: SchemaCreation::CreateDatabase,
    schema_owner: false,
    drop_schema_cascade: false,
    users: UserModel::Unsupported,
};

/// CQL's tables: a primary key that can't change, no other keys or constraints, and secondary
/// indexes. The object helpers don't write CQL (see [`Dialect::writes_ddl`]), so these facts
/// are only CQL's own.
const CQL: Capabilities = Capabilities {
    transactional_ddl: false,
    drop_cascade: false,
    unique_keys: false,
    foreign_keys: false,
    referential_actions: &[],
    indexes: true,
    index_methods: &[],
    table_changes: &[
        TableChange::RenameColumn,
        TableChange::AddColumn,
        TableChange::DropColumn,
    ],
    rebuilds_tables: false,
    foreign_key_switch: None,
    converts_with_using: false,
    restricted_add_column: false,
    rebuilds_table_ddl: false,
    common_types: &[
        "int",
        "bigint",
        "text",
        "boolean",
        "double",
        "decimal",
        "timestamp",
        "date",
        "uuid",
        "timeuuid",
        "blob",
        "list<text>",
        "set<text>",
        "map<text, text>",
    ],
    materialized_views: true,
    view_replacement: ViewReplacement::DropAndCreate,
    renames_views: false,
    schema_creation: SchemaCreation::Unsupported,
    schema_owner: false,
    drop_schema_cascade: false,
    users: UserModel::Unsupported,
};

impl Dialect {
    /// What the dialect's DDL can do.
    pub fn capabilities(self) -> &'static Capabilities {
        match self {
            Self::Postgres => &POSTGRES,
            Self::Sqlite => &SQLITE,
            Self::Mysql => &MYSQL,
            Self::SqlServer => &SQL_SERVER,
            Self::DuckDb => &DUCKDB,
            Self::ClickHouse => &CLICKHOUSE,
            Self::Cassandra => &CQL,
        }
    }

    /// Whether the object helpers write this dialect's DDL. Not CQL's: a table there is
    /// designed around the queries that read it, its primary key can't change afterwards, and
    /// a keyspace's replication is chosen per cluster, none of which the helpers' forms ask.
    pub fn writes_ddl(self) -> bool {
        self != Self::Cassandra
    }
}

impl Capabilities {
    pub fn changes_in_place(&self, change: TableChange) -> bool {
        self.table_changes.contains(&change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_keys_offer_no_action_first_and_only_where_they_exist() {
        for dialect in Dialect::ALL {
            let capabilities = dialect.capabilities();
            if capabilities.foreign_keys {
                assert_eq!(
                    capabilities.referential_actions.first(),
                    Some(&ReferentialAction::NoAction),
                    "{dialect:?}"
                );
            } else {
                assert!(capabilities.referential_actions.is_empty(), "{dialect:?}");
            }
        }
    }

    #[test]
    fn schema_facts_need_create_schema() {
        for dialect in Dialect::ALL {
            let capabilities = dialect.capabilities();
            if capabilities.schema_creation != SchemaCreation::CreateSchema {
                assert!(!capabilities.schema_owner, "{dialect:?}");
                assert!(!capabilities.drop_schema_cascade, "{dialect:?}");
            }
        }
    }

    #[test]
    fn index_methods_need_indexes() {
        for dialect in Dialect::ALL {
            let capabilities = dialect.capabilities();
            assert!(
                capabilities.indexes || capabilities.index_methods.is_empty(),
                "{dialect:?}"
            );
            assert!(!capabilities.common_types.is_empty(), "{dialect:?}");
        }
    }
}
