//! Domain types and the [`Connection`] trait shared by every DbRover crate.
//!
//! Nothing here does I/O. Driver crates implement [`Connection`]; the engine owns the
//! connections and the UI only ever sees the plain data types.

mod catalog;
mod catalog_page;
mod command_template;
mod connection;
mod data_source;
mod error;
mod execution;
mod mongo_uri;
mod password;
mod password_command;
mod placeholder;
mod quoting;
mod role;
mod sql_template;
mod table;

#[cfg(feature = "conformance")]
pub mod conformance;

pub use catalog::{CatalogNode, CatalogPath, FolderKind, KeyType, NodeKind, PathSegment};
pub use catalog_page::{CATALOG_PAGE_SIZE, ChildrenPage, ChildrenQuery, PageToken, like_contains};
pub use command_template::{CommandTemplate, DataSourceFields};
pub use connection::{Connection, Rejection, RejectionKind, ServerInfo};
pub use data_source::{
    CassandraConfig, ClickHouseConfig, ConnectionConfig, DataSource, DataSourceId, DriverKind,
    DuckDbConfig, DuckDbDatabase, ElasticsearchAuth, ElasticsearchConfig, McpLogin, MongoConfig,
    MysqlConfig, Port, PostgresConfig, ProjectId, RedisConfig, Route, SafetyLevel, ServerConfig,
    SourceColor, SqlServerConfig, SqliteConfig, SshConfig, SshLogin, TlsMode,
};
pub use error::{BoxError, DriverError, describe_error};
pub use execution::{
    CancelHandle, Cell, ColumnInfo, ResultCursor, ResultShape, Row, TransactionStatus, ValueKind,
};
pub use mongo_uri::MongoUri;
pub use password::Password;
pub use password_command::PasswordCommand;
pub use placeholder::{
    ExpandError, PlaceholderError, Setting, Variables, expand, expand_path, has_placeholder,
};
pub use quoting::Quoting;
pub use role::{RoleAttribute, RoleDescription};
pub use sql_template::{
    Placeholder, RenderError, SelectedCells, SqlTemplate, TemplateError, TemplateValues,
};
pub use table::{
    CheckConstraint, ColumnDescription, ForeignKey, ForeignKeyConstraint, Identifier, Identity,
    IdentityGeneration, ReferentialAction, RelationName, TableConstraints, TableDescription,
    TableName, UniqueConstraint,
};
