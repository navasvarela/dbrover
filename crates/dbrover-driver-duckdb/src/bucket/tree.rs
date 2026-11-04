//! The bucket's part of the explorer's tree: the bucket beside the database's own catalogs,
//! a level of its keys as folders and files, and a file's columns.

use dbrover_cloud::storage::Level;
use dbrover_core::{
    ByteCount, CatalogNode, CatalogPath, ChildrenQuery, NodeKind, ObjectKind, ObjectStorage,
    object_url, reads_as_table,
};

/// Which part of a bucket a path is.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BucketPath {
    /// The bucket or one of its folders, whose keys start with `prefix` (empty, or ending in
    /// `/`).
    Level { prefix: String },
    /// A file DuckDB reads as a table, at `url`.
    File { url: String },
}

impl BucketPath {
    /// What `path` is in the bucket at `root` (`s3://sales`); `None` for a path outside it,
    /// or a file DuckDB can't read as a table.
    pub(crate) fn of(path: &CatalogPath, root: &str) -> Option<Self> {
        let first = path.segments().first()?;
        if first.kind != NodeKind::Object(ObjectKind::Bucket) || first.name != root {
            return None;
        }
        let url = object_url(path)?;
        let last = path.last()?;
        match last.kind {
            NodeKind::Object(ObjectKind::File) if reads_as_table(&last.name) => {
                Some(Self::File { url })
            }
            NodeKind::Object(ObjectKind::Bucket | ObjectKind::Folder) => Some(Self::Level {
                prefix: url.strip_prefix(root)?.trim_start_matches('/').to_owned(),
            }),
            _ => None,
        }
    }
}

/// The bucket's node, at the tree's top level beside the database's catalogs.
pub(crate) fn bucket_node(storage: &ObjectStorage) -> CatalogNode {
    CatalogNode {
        path: CatalogPath::root().child(NodeKind::Object(ObjectKind::Bucket), storage.url()),
        label: storage.bucket().to_owned(),
        detail: Some(storage.provider().to_string()),
        has_children: true,
        has_definition: false,
        system: false,
    }
}

/// `level`'s folders, then its files, as children of `parent`, whose keys start with
/// `prefix`: those whose label passes `query`'s filter.
///
/// A store may list a level's own prefix among its files (a key ending in `/`, as consoles
/// make for an empty folder), which isn't shown.
pub(crate) fn level_nodes(
    parent: &CatalogPath,
    prefix: &str,
    level: &Level,
    query: &ChildrenQuery,
) -> Vec<CatalogNode> {
    let folders = level.folders.iter().filter_map(|folder| {
        let name = folder.strip_prefix(prefix)?.trim_end_matches('/');
        (!name.is_empty()).then(|| CatalogNode {
            path: parent.child(NodeKind::Object(ObjectKind::Folder), name),
            label: name.to_owned(),
            detail: None,
            has_children: true,
            has_definition: false,
            system: false,
        })
    });
    let files = level.files.iter().filter_map(|file| {
        let name = file.key.strip_prefix(prefix)?;
        (!name.is_empty() && !name.contains('/')).then(|| CatalogNode {
            path: parent.child(NodeKind::Object(ObjectKind::File), name),
            label: name.to_owned(),
            detail: Some(ByteCount(file.size).to_string()),
            has_children: reads_as_table(name),
            has_definition: false,
            system: false,
        })
    });
    folders
        .chain(files)
        .filter(|node| query.matches(&node.label))
        .collect()
}

/// A column of a file, as DuckDB's `DESCRIBE` names and types it.
pub(crate) fn column_node(file: &CatalogPath, name: &str, data_type: &str) -> CatalogNode {
    CatalogNode {
        path: file.child(NodeKind::Column, name),
        label: name.to_owned(),
        detail: Some(data_type.to_owned()),
        has_children: false,
        has_definition: false,
        system: false,
    }
}

#[cfg(test)]
mod tests {
    use dbrover_cloud::storage::StoredFile;
    use dbrover_core::{S3Bucket, S3SignIn};

    use super::*;

    fn bucket() -> CatalogPath {
        CatalogPath::root().child(NodeKind::Object(ObjectKind::Bucket), "s3://dbrover")
    }

    #[test]
    fn tells_a_level_from_a_readable_file() {
        let sales = bucket().child(NodeKind::Object(ObjectKind::Folder), "sales");
        let cases = [
            (
                bucket(),
                Some(BucketPath::Level {
                    prefix: String::new(),
                }),
            ),
            (
                sales.clone(),
                Some(BucketPath::Level {
                    prefix: "sales/".to_owned(),
                }),
            ),
            (
                sales.child(NodeKind::Object(ObjectKind::File), "part-0.parquet"),
                Some(BucketPath::File {
                    url: "s3://dbrover/sales/part-0.parquet".to_owned(),
                }),
            ),
            (
                sales.child(NodeKind::Object(ObjectKind::File), "_SUCCESS"),
                None,
            ),
            (
                CatalogPath::root().child(NodeKind::Object(ObjectKind::Bucket), "s3://other"),
                None,
            ),
            (
                CatalogPath::root().child(NodeKind::Database, "memory"),
                None,
            ),
        ];
        for (path, expected) in cases {
            assert_eq!(BucketPath::of(&path, "s3://dbrover"), expected, "{path}");
        }
    }

    #[test]
    fn a_levels_folders_come_before_its_files_with_their_sizes() {
        let sales = bucket().child(NodeKind::Object(ObjectKind::Folder), "sales");
        let level = Level {
            folders: vec!["sales/year=2025/".to_owned(), "sales/year=2026/".to_owned()],
            files: vec![
                StoredFile {
                    key: "sales/".to_owned(),
                    size: 0,
                },
                StoredFile {
                    key: "sales/_SUCCESS".to_owned(),
                    size: 0,
                },
                StoredFile {
                    key: "sales/summary.csv".to_owned(),
                    size: 12_500,
                },
            ],
            next: None,
        };

        let nodes = level_nodes(&sales, "sales/", &level, &ChildrenQuery::default());

        let shown: Vec<_> = nodes
            .iter()
            .map(|node| {
                (
                    node.label.as_str(),
                    node.detail.as_deref(),
                    node.has_children,
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("year=2025", None, true),
                ("year=2026", None, true),
                ("_SUCCESS", Some("0 bytes"), false),
                ("summary.csv", Some("12.5 KB"), true),
            ]
        );
        assert_eq!(
            object_url(&nodes[3].path).as_deref(),
            Some("s3://dbrover/sales/summary.csv")
        );
    }

    #[test]
    fn the_filter_keeps_what_contains_it_ignoring_case() {
        let level = Level {
            folders: vec!["Orders/".to_owned(), "returns/".to_owned()],
            files: vec![StoredFile {
                key: "orders.csv".to_owned(),
                size: 1,
            }],
            next: None,
        };
        let query = ChildrenQuery::first_page(Some("ORDER".to_owned()));

        let nodes = level_nodes(&bucket(), "", &level, &query);

        let labels: Vec<_> = nodes.iter().map(|node| node.label.as_str()).collect();
        assert_eq!(labels, ["Orders", "orders.csv"]);
    }

    #[test]
    fn the_bucket_is_named_by_its_address_and_labelled_by_its_name() {
        let node = bucket_node(&ObjectStorage::S3(S3Bucket {
            bucket: "dbrover".to_owned(),
            region: String::new(),
            endpoint: String::new(),
            sign_in: S3SignIn::Anonymous,
        }));

        assert_eq!(node.path, bucket());
        assert_eq!(node.label, "dbrover");
        assert_eq!(node.detail.as_deref(), Some("Amazon S3"));
    }
}
