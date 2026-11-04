//! Domain types and the [`Connection`] trait shared by every DbRover crate.
//!
//! Nothing here does I/O. Driver crates implement [`Connection`]; the engine owns the
//! connections and the UI only ever sees the plain data types.

mod byte_size;
mod catalog;
mod catalog_page;
mod cloud_sign_in;
mod command_template;
mod connection;
mod cost;
mod data_source;
mod error;
mod execution;
mod job;
mod managed_service;
mod mongo_uri;
mod object_storage;
mod password;
mod password_command;
mod placeholder;
mod quoting;
mod role;
mod sql_template;
mod table;

#[cfg(feature = "conformance")]
pub mod conformance;

pub use byte_size::ByteSize;
pub use catalog::{
    CatalogNode, CatalogPath, FolderKind, KeyType, NodeKind, ObjectKind, PathSegment,
};
pub use catalog_page::{
    CATALOG_PAGE_SIZE, CATALOG_SEARCH_LIMIT, CatalogMatches, CatalogSearch, ChildrenPage,
    ChildrenQuery, PageToken, like_contains,
};
pub use cloud_sign_in::{AzureTool, CloudProvider, CloudSignIn};
pub use command_template::{CommandTemplate, DataSourceFields};
pub use connection::{Connection, Rejection, RejectionKind, ServerInfo};
pub use cost::{ByteCount, CostEstimate, CostGuard, FullScan, ReadPace, ScanGuard};
pub use data_source::{
    CassandraConfig, ClickHouseConfig, ConnectionConfig, DataSource, DataSourceId, DriverKind,
    DuckDbConfig, DuckDbDatabase, DynamoDbConfig, DynamoDbSignIn, ElasticsearchAuth,
    ElasticsearchConfig, McpLogin, MongoConfig, MysqlConfig, Port, PostgresConfig, ProjectId,
    RedisConfig, Route, SafetyLevel, ServerConfig, SourceColor, SqlServerConfig, SqliteConfig,
    SshConfig, SshLogin, TlsMode,
};
pub use error::{BoxError, DriverError, describe_error};
pub use execution::{
    CancelHandle, Cell, ColumnInfo, ResultCursor, ResultShape, Row, TransactionStatus, ValueKind,
};
pub use job::{JobId, JobPage, JobProgress, JobService, JobState, Jobs, ProgressHandle};
pub use managed_service::{
    ManagedService, ServiceAddress, ServiceEngine, ServicePreset, ServiceSignIn, ServiceTls,
    ServiceVendor,
};
pub use mongo_uri::MongoUri;
pub use object_storage::{
    AzureContainer, AzureSignIn, GcsBucket, GcsSignIn, ObjectStorage, S3Bucket, S3SignIn,
    StorageProvider, object_url, reads_as_table,
};
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
