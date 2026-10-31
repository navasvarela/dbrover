//! Catalog paths and nodes as JSON. A node's `path` is what the next `list_objects` or
//! `get_definition` call takes back, so a client walks the tree without knowing its shape.

use dbrover_core::{CatalogNode, CatalogPath, FolderKind, KeyType, NodeKind};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// One step down the catalog: a schema, a folder such as `tables`, a table…
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(crate) struct PathStep {
    /// `database`, `schema`, `folder`, `table`, `view`, `materialized_view`, `function`,
    /// `procedure`, `sequence`, `event`, `column`, `index`, `constraint`, `trigger`, `role`,
    /// `key_prefix` (Redis keys sharing a prefix) or `key` (a Redis key).
    pub(crate) kind: String,
    /// The object's name; for a folder, what it holds (`tables`, `views`, `columns`…).
    pub(crate) name: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct NodeJson {
    name: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    has_children: bool,
    has_definition: bool,
    path: Vec<PathStep>,
}

impl From<&CatalogNode> for NodeJson {
    fn from(node: &CatalogNode) -> Self {
        Self {
            name: node.label.clone(),
            kind: node.kind().map_or("database", kind_name),
            detail: node.detail.clone(),
            has_children: node.has_children,
            has_definition: node.has_definition,
            path: steps(&node.path),
        }
    }
}

const FOLDERS: [FolderKind; 16] = [
    FolderKind::Tables,
    FolderKind::Views,
    FolderKind::MaterializedViews,
    FolderKind::Functions,
    FolderKind::Procedures,
    FolderKind::Sequences,
    FolderKind::Events,
    FolderKind::Columns,
    FolderKind::Indexes,
    FolderKind::Constraints,
    FolderKind::Triggers,
    FolderKind::Roles,
    FolderKind::Indices,
    FolderKind::Aliases,
    FolderKind::DataStreams,
    FolderKind::IndexTemplates,
];

const KINDS: [(NodeKind, &str); 18] = [
    (NodeKind::Database, "database"),
    (NodeKind::Schema, "schema"),
    (NodeKind::Table, "table"),
    (NodeKind::View, "view"),
    (NodeKind::MaterializedView, "materialized_view"),
    (NodeKind::Function, "function"),
    (NodeKind::Procedure, "procedure"),
    (NodeKind::Sequence, "sequence"),
    (NodeKind::Event, "event"),
    (NodeKind::Column, "column"),
    (NodeKind::Index, "index"),
    (NodeKind::Constraint, "constraint"),
    (NodeKind::Trigger, "trigger"),
    (NodeKind::Role, "role"),
    (NodeKind::KeyPrefix, "key_prefix"),
    (NodeKind::Collection, "collection"),
    (NodeKind::SearchIndex, "search_index"),
    (NodeKind::IndexTemplate, "index_template"),
];

/// A Redis key's kind in a path. Its value's type is in the node's `detail`, and isn't needed
/// to find the key again, so a key read back from a path is of type `Other`.
const KEY: &str = "key";

fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Folder(_) => "folder",
        NodeKind::Key(_) => KEY,
        other => KINDS
            .iter()
            .find(|(known, _)| *known == other)
            .map_or("object", |(_, name)| name),
    }
}

pub(crate) fn steps(path: &CatalogPath) -> Vec<PathStep> {
    path.segments()
        .iter()
        .map(|segment| PathStep {
            kind: kind_name(segment.kind).to_owned(),
            name: segment.name.clone(),
        })
        .collect()
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0:?} is not a kind of catalog object; use the path list_objects returned")]
pub(crate) struct UnknownStep(String);

/// The catalog path `steps` describe, as `list_objects` returned it.
pub(crate) fn path(steps: &[PathStep]) -> Result<CatalogPath, UnknownStep> {
    steps.iter().try_fold(CatalogPath::root(), |path, step| {
        let kind = match step.kind.as_str() {
            "folder" => FOLDERS
                .into_iter()
                .find(|folder| folder.label() == step.name)
                .map(NodeKind::Folder),
            KEY => Some(NodeKind::Key(KeyType::Other)),
            name => KINDS
                .iter()
                .find(|(_, known)| *known == name)
                .map(|(kind, _)| *kind),
        };
        let kind = kind.ok_or_else(|| UnknownStep(format!("{}:{}", step.kind, step.name)))?;
        Ok(path.child(kind, &step.name))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_round_trip_through_json_steps() {
        let path = CatalogPath::root()
            .child(NodeKind::Database, "shop")
            .child(NodeKind::Schema, "public")
            .child(
                NodeKind::Folder(FolderKind::MaterializedViews),
                "materialized views",
            )
            .child(NodeKind::MaterializedView, "totals");

        let steps = steps(&path);

        assert_eq!(steps[2].kind, "folder");
        assert_eq!(steps[3].kind, "materialized_view");
        assert_eq!(super::path(&steps), Ok(path));
        assert!(
            super::path(&[PathStep {
                kind: "folder".to_owned(),
                name: "secrets".to_owned()
            }])
            .is_err()
        );
    }

    #[test]
    fn redis_groups_round_trip_and_keys_keep_their_names() {
        let group = CatalogPath::root()
            .child(NodeKind::Database, "0")
            .child(NodeKind::KeyPrefix, "user:");
        assert_eq!(super::path(&steps(&group)), Ok(group.clone()));
        let key = group.child(NodeKind::Key(KeyType::Hash), "user:42");
        let read = super::path(&steps(&key)).unwrap();
        assert_eq!(read.last().map(|step| step.name.as_str()), Some("user:42"));
        assert_eq!(steps(&key)[2].kind, "key");
    }
}
