//! The catalog: the tree of objects a data source exposes, loaded one level at a time.

use std::fmt;

use crate::Quoting;

/// Where a node sits in the catalog tree, from the data source down. The empty path is the
/// data source itself.
///
/// Every segment carries its kind as well as its name, so drivers can match on the path's
/// shape (`[Database, Schema, Folder(Tables)]`) instead of guessing what a name refers to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct CatalogPath(Vec<PathSegment>);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathSegment {
    pub kind: NodeKind,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Database,
    Schema,
    Folder(FolderKind),
    Table,
    View,
    MaterializedView,
    Function,
    /// A stored procedure (MySQL), called rather than used in an expression.
    Procedure,
    Sequence,
    /// A scheduled event (MySQL).
    Event,
    /// Another name for an object, possibly in another database or server (SQL Server).
    Synonym,
    /// A key-value lookup kept in memory and loaded from a source (ClickHouse).
    Dictionary,
    Column,
    Index,
    Constraint,
    Trigger,
    /// A user or group of the whole server (PostgreSQL's roles), listed beside its databases.
    Role,
    /// A Redis key, with the type of its value. Its path segment's name is the key written as
    /// `dbrover_language::redis::quote` writes an argument, so a key that isn't text survives.
    Key(KeyType),
    /// Redis keys that share a prefix up to a separator (`user:` for `user:42` and `user:43`),
    /// grouped as a folder is. Its path segment's name is the whole prefix, separator included.
    KeyPrefix,
    /// A MongoDB collection of documents: a plain one, a view or a time-series collection, as
    /// the node's detail says.
    Collection,
    /// What an Elasticsearch or OpenSearch search reads documents from: an index, an alias or
    /// a data stream, by its name, the folder it is in saying which.
    SearchIndex,
    /// An Elasticsearch or OpenSearch index template, which new indices whose names match its
    /// patterns are made from.
    IndexTemplate,
    /// A user-defined type (Cassandra's `CREATE TYPE`), a set of named fields a column can hold.
    UserType,
    /// A user-defined aggregate (Cassandra's `CREATE AGGREGATE`), named with its argument types
    /// as a function is.
    Aggregate,
}

/// The type of a Redis key's value, as `TYPE` answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyType {
    String,
    Hash,
    List,
    Set,
    SortedSet,
    Stream,
    /// A JSON document of the JSON module (`ReJSON-RL`).
    Json,
    /// Any other module's type, such as a vector set or a time series.
    Other,
}

impl KeyType {
    /// The type `TYPE` names; `None` for `none`, a key that no longer exists.
    pub fn from_type_name(name: &str) -> Option<Self> {
        Some(match name {
            "none" => return None,
            "string" => Self::String,
            "hash" => Self::Hash,
            "list" => Self::List,
            "set" => Self::Set,
            "zset" => Self::SortedSet,
            "stream" => Self::Stream,
            "ReJSON-RL" => Self::Json,
            _ => Self::Other,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Hash => "hash",
            Self::List => "list",
            Self::Set => "set",
            Self::SortedSet => "sorted set",
            Self::Stream => "stream",
            Self::Json => "JSON",
            Self::Other => "other",
        }
    }
}

/// Groups children by kind ("tables", "views", and so on), as most database tools' trees do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FolderKind {
    Tables,
    Views,
    MaterializedViews,
    Functions,
    Procedures,
    Sequences,
    Events,
    Synonyms,
    Dictionaries,
    Columns,
    Indexes,
    Constraints,
    Triggers,
    Roles,
    /// Elasticsearch's and OpenSearch's indices.
    Indices,
    Aliases,
    DataStreams,
    IndexTemplates,
    /// Cassandra's user-defined types.
    Types,
    Aggregates,
}

impl FolderKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tables => "tables",
            Self::Views => "views",
            Self::MaterializedViews => "materialized views",
            Self::Functions => "functions",
            Self::Procedures => "procedures",
            Self::Sequences => "sequences",
            Self::Events => "events",
            Self::Synonyms => "synonyms",
            Self::Dictionaries => "dictionaries",
            Self::Columns => "columns",
            Self::Indexes => "indexes",
            Self::Constraints => "constraints",
            Self::Triggers => "triggers",
            Self::Roles => "roles",
            Self::Indices => "indices",
            Self::Aliases => "aliases",
            Self::DataStreams => "data streams",
            Self::IndexTemplates => "index templates",
            Self::Types => "types",
            Self::Aggregates => "aggregates",
        }
    }
}

/// One entry in the catalog tree, as returned by [`Connection::children`](crate::Connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogNode {
    pub path: CatalogPath,
    pub label: String,
    /// Short extra information shown next to the label, e.g. a column's type.
    pub detail: Option<String>,
    pub has_children: bool,
    /// Whether [`Connection::definition`](crate::Connection) can return source for it.
    pub has_definition: bool,
    /// Engine-internal objects (`pg_catalog`, `sqlite_sequence`, …) that the explorer hides
    /// unless asked to show them.
    pub system: bool,
}

impl CatalogNode {
    pub fn kind(&self) -> Option<NodeKind> {
        self.path.last().map(|segment| segment.kind)
    }

    /// A folder node under `parent`, which always has children and no definition.
    pub fn folder(parent: &CatalogPath, folder: FolderKind) -> Self {
        Self {
            path: parent.child(NodeKind::Folder(folder), folder.label()),
            label: folder.label().to_owned(),
            detail: None,
            has_children: true,
            has_definition: false,
            system: false,
        }
    }
}

impl CatalogPath {
    pub fn root() -> Self {
        Self::default()
    }

    pub fn child(&self, kind: NodeKind, name: impl Into<String>) -> Self {
        let mut segments = self.0.clone();
        segments.push(PathSegment {
            kind,
            name: name.into(),
        });
        Self(segments)
    }

    pub fn segments(&self) -> &[PathSegment] {
        &self.0
    }

    pub fn last(&self) -> Option<&PathSegment> {
        self.0.last()
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The path one level up, or `None` for the root.
    pub fn parent(&self) -> Option<Self> {
        let (_, parent) = self.0.split_last()?;
        Some(Self(parent.to_vec()))
    }

    /// Whether `self` is `other` or one of its descendants.
    pub fn starts_with(&self, other: &Self) -> bool {
        self.0.starts_with(&other.0)
    }

    /// The name of the nearest segment of `kind`, searching from the leaf up.
    pub fn name_of(&self, kind: NodeKind) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|segment| segment.kind == kind)
            .map(|segment| segment.name.as_str())
    }

    /// The name to use in SQL for the object at this path: schema-qualified for schema objects
    /// (`public.users`, `"My Schema"."Order Items"`), bare for columns, constraints and
    /// triggers, and for roles, which belong to no schema. `None` for databases, schemas and
    /// folders.
    pub fn qualified_name(&self, quoting: Quoting) -> Option<String> {
        self.qualified_with(&|name| quoting.identifier_if_needed(name))
    }

    /// [`Self::qualified_name`] with every name quoted, even a plain lowercase one, so it holds
    /// in generated SQL whatever the name spells, a keyword such as `order` included.
    pub fn quoted_name(&self, quoting: Quoting) -> Option<String> {
        self.qualified_with(&|name| quoting.identifier(name))
    }

    fn qualified_with(&self, quote: &dyn Fn(&str) -> String) -> Option<String> {
        let object = self.last()?;
        match object.kind {
            NodeKind::Table
            | NodeKind::View
            | NodeKind::MaterializedView
            | NodeKind::Sequence
            | NodeKind::Procedure
            | NodeKind::Event
            | NodeKind::Synonym
            | NodeKind::Dictionary
            | NodeKind::Index
            | NodeKind::UserType => Some(self.schema_qualified(&quote(&object.name), quote)),
            // Function names already carry their argument list, e.g. `add(integer, integer)`.
            NodeKind::Function | NodeKind::Aggregate => {
                Some(self.schema_qualified(&object.name, quote))
            }
            NodeKind::Column | NodeKind::Constraint | NodeKind::Trigger | NodeKind::Role => {
                Some(quote(&object.name))
            }
            // A key's, a collection's or an index's name as a command takes it: it belongs to
            // no schema.
            NodeKind::Key(_)
            | NodeKind::Collection
            | NodeKind::SearchIndex
            | NodeKind::IndexTemplate => Some(object.name.clone()),
            NodeKind::Database | NodeKind::Schema | NodeKind::Folder(_) | NodeKind::KeyPrefix => {
                None
            }
        }
    }

    fn schema_qualified(&self, object_name: &str, quote: &dyn Fn(&str) -> String) -> String {
        match self.name_of(NodeKind::Schema) {
            Some(schema) => format!("{}.{object_name}", quote(schema)),
            None => object_name.to_owned(),
        }
    }
}

impl fmt::Display for CatalogPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.0.iter().map(|s| s.name.as_str()).collect();
        write!(f, "/{}", names.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_path(schema: &str, table: &str) -> CatalogPath {
        CatalogPath::root()
            .child(NodeKind::Database, "app")
            .child(NodeKind::Schema, schema)
            .child(NodeKind::Folder(FolderKind::Tables), "tables")
            .child(NodeKind::Table, table)
    }

    #[test]
    fn qualifies_and_quotes_object_names() {
        let cases = [
            (table_path("public", "users"), Some("public.users")),
            (
                table_path("Sales", "order \"items\""),
                Some("\"Sales\".\"order \"\"items\"\"\""),
            ),
            (
                table_path("public", "users").child(NodeKind::Column, "Email"),
                Some("\"Email\""),
            ),
            (table_path("public", "users").parent().unwrap(), None),
            (CatalogPath::root().child(NodeKind::Database, "app"), None),
        ];

        for (path, expected) in cases {
            assert_eq!(
                path.qualified_name(Quoting::STANDARD).as_deref(),
                expected,
                "{path}"
            );
        }
    }

    #[test]
    fn quoted_names_quote_even_plain_ones() {
        assert_eq!(
            table_path("public", "order").quoted_name(Quoting::STANDARD),
            Some("\"public\".\"order\"".to_owned())
        );
        assert_eq!(
            CatalogPath::root()
                .child(NodeKind::Schema, "public")
                .child(NodeKind::Function, "add(integer, integer)")
                .quoted_name(Quoting::STANDARD),
            Some("\"public\".add(integer, integer)".to_owned())
        );
    }

    #[test]
    fn navigates_up_and_recognises_descendants() {
        let table = table_path("public", "users");
        let schema = table.parent().unwrap().parent().unwrap();

        assert!(table.starts_with(&schema));
        assert!(!schema.starts_with(&table));
        assert_eq!(table.name_of(NodeKind::Schema), Some("public"));
        assert_eq!(CatalogPath::root().parent(), None);
        assert_eq!(table.to_string(), "/app/public/tables/users");
    }
}
