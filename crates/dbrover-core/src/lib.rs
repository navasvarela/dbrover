//! Domain types and the [`Connection`] trait shared by every DbRover crate.
//!
//! Nothing here does I/O. Driver crates implement [`Connection`]; the engine owns the
//! connections and the UI only ever sees the plain data types.

mod batch;
mod blob_id;
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
mod schema;
mod sql_template;
mod table;

#[cfg(feature = "conformance")]
pub mod conformance;

pub use batch::{BatchCursor, bulk_kind};
pub use blob_id::{BlobId, InvalidBlobId};
pub use byte_size::ByteSize;
pub use catalog::{
    CatalogNode, CatalogPath, FolderKind, KeyType, NodeKind, ObjectKind, PathSegment, TableFormat,
};
pub use catalog_page::{
    CATALOG_PAGE_SIZE, CATALOG_SEARCH_LIMIT, CatalogMatches, CatalogSearch, ChildrenPage,
    ChildrenQuery, PageToken, like_contains,
};
pub use cloud_sign_in::{AzureTool, CloudProvider, CloudSignIn};
pub use command_template::{CommandTemplate, DataSourceFields};
pub use connection::{Connection, Rejection, RejectionKind, ServerInfo};
pub use cost::{ByteCount, CostEstimate, CostGuard, FullScan, ReadPace, ScanEstimate, ScanGuard};
pub use data_source::{
    AthenaConfig, AwsSignInKind, BigQueryConfig, CassandraConfig, ClickHouseConfig,
    ConnectionConfig, DataSource, DataSourceId, DriverKind, DuckDbConfig, DuckDbDatabase,
    DynamoDbConfig, ElasticsearchAuth, ElasticsearchConfig, McpLogin, MongoConfig, MysqlConfig,
    Port, PostgresConfig, ProjectId, RedisConfig, Route, SafetyLevel, ServerConfig, SourceColor,
    SqlServerConfig, SqliteConfig, SshConfig, SshLogin, TlsMode, TrinoConfig, TrinoProtocol,
};
pub use error::{BoxError, DriverError, describe_error, position_at};
pub use execution::{
    CancelHandle, Cell, ColumnInfo, ResultCursor, ResultShape, Row, TransactionStatus, ValueKind,
};
pub use job::{
    JobFailure, JobId, JobPage, JobProgress, JobService, JobState, Jobs, ProgressHandle,
};
pub use managed_service::{
    ManagedService, ServiceAddress, ServiceEngine, ServicePreset, ServiceSignIn, ServiceTls,
    ServiceVendor,
};
pub use mongo_uri::MongoUri;
pub use object_storage::{
    AzureContainer, AzureSignIn, GcsBucket, GoogleCloudSignIn, ICEBERG_METADATA_SUFFIX,
    ObjectStorage, S3Bucket, S3SignIn, StorageProvider, object_source, object_url, reads_as_table,
};
pub use password::Password;
pub use password_command::PasswordCommand;
pub use placeholder::{
    ExpandError, PlaceholderError, Setting, Variables, expand, expand_path, has_placeholder,
};
pub use quoting::Quoting;
pub use role::{RoleAttribute, RoleDescription};
pub use schema::{IndexDescription, SchemaDescription, SchemaTable, ViewDescription};
pub use sql_template::{
    Placeholder, RenderError, SelectedCells, SqlTemplate, TemplateError, TemplateValues,
};
pub use table::{
    CheckConstraint, ColumnDescription, ForeignKey, ForeignKeyConstraint, Identifier, Identity,
    IdentityGeneration, ReferentialAction, RelationName, TableConstraints, TableDescription,
    TableName, UniqueConstraint,
};
