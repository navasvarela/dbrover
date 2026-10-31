//! The explorer's cached catalog, read for completion, and the levels completion still needs
//! loaded.

use dbrover_core::{CatalogNode, CatalogPath, ConnectionConfig, FolderKind, NodeKind};

use super::completion::{CatalogSource, ColumnEntry, FunctionEntry, TableEntry, TableRef};
use crate::catalog_tree::{CatalogTree, Children};

/// Folders whose objects complete as tables (views among them).
const RELATION_FOLDERS: [FolderKind; 3] = [
    FolderKind::Tables,
    FolderKind::Views,
    FolderKind::MaterializedViews,
];

/// The catalog levels completion still waits for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Needed {
    /// Never asked for: to request now.
    pub unrequested: Vec<CatalogPath>,
    /// Asked for and not answered yet.
    pub loading: bool,
}

impl Needed {
    pub fn is_pending(&self) -> bool {
        self.loading || !self.unrequested.is_empty()
    }
}

/// One data source's cached catalog, from the level that holds its schemas down.
pub struct TreeCatalog<'a> {
    tree: &'a CatalogTree,
    /// The node whose children are the schemas: the configured database on PostgreSQL (the
    /// one a console connects to), the root on SQLite.
    schemas_parent: CatalogPath,
    default_schema: String,
}

impl<'a> TreeCatalog<'a> {
    pub fn new(tree: &'a CatalogTree, config: &ConnectionConfig) -> Self {
        let schemas_parent = match config.database() {
            Some(database) => CatalogPath::root().child(NodeKind::Database, database),
            None => CatalogPath::root(),
        };
        Self {
            tree,
            schemas_parent,
            default_schema: config.default_schema().to_owned(),
        }
    }

    /// The levels completion needs that haven't loaded: the schemas, each schema's folders
    /// and its tables and views, and the columns of `references`. Each level is only known
    /// once the one above has loaded, so this is asked again as answers arrive. Levels that
    /// failed to load count as done; expanding them in the explorer retries.
    pub fn missing(&self, references: &[TableRef]) -> Needed {
        let mut missing = Needed::default();
        let mut need = |path: CatalogPath| match self.tree.children(&path) {
            None => missing.unrequested.push(path),
            Some(Children::Loading) => missing.loading = true,
            Some(Children::Loaded(_) | Children::Failed(_)) => {}
        };
        if !self.schemas_parent.is_root() {
            need(CatalogPath::root());
        }
        need(self.schemas_parent.clone());
        for schema in self.schema_nodes() {
            need(schema.path.clone());
            for folder in self.relation_folders(&schema.path) {
                need(folder.path.clone());
            }
        }
        for reference in references {
            let Some(relation) = self.relation_node(reference) else {
                continue;
            };
            need(relation.path.clone());
            if let Some(columns) = self.folder(&relation.path, FolderKind::Columns) {
                need(columns.path.clone());
            }
        }
        missing
    }

    fn schema_nodes(&self) -> impl Iterator<Item = &CatalogNode> {
        self.tree
            .loaded_children(&self.schemas_parent)
            .unwrap_or_default()
            .iter()
            .filter(|node| node.kind() == Some(NodeKind::Schema) && !node.system)
    }

    fn folder(&self, parent: &CatalogPath, kind: FolderKind) -> Option<&CatalogNode> {
        self.tree
            .loaded_children(parent)?
            .iter()
            .find(|node| node.kind() == Some(NodeKind::Folder(kind)))
    }

    fn relation_folders(&self, schema: &CatalogPath) -> impl Iterator<Item = &CatalogNode> {
        RELATION_FOLDERS
            .into_iter()
            .filter_map(|kind| self.folder(schema, kind))
    }

    fn relations(&self) -> impl Iterator<Item = (&CatalogNode, &CatalogNode)> {
        self.schema_nodes().flat_map(move |schema| {
            self.relation_folders(&schema.path)
                .flat_map(|folder| self.tree.loaded_children(&folder.path).unwrap_or_default())
                .filter(|node| !node.system)
                .map(move |relation| (schema, relation))
        })
    }

    /// The loaded table or view a reference names, by the same rules completion resolves
    /// names: its schema if given, else the default schema, else any.
    fn relation_node(&self, reference: &TableRef) -> Option<&CatalogNode> {
        let named = |(_, relation): &(&CatalogNode, &CatalogNode)| {
            relation.label.eq_ignore_ascii_case(&reference.table)
        };
        let in_schema =
            |schema: &str, owner: &CatalogNode| owner.label.eq_ignore_ascii_case(schema);
        let found = match &reference.schema {
            Some(schema) => self
                .relations()
                .filter(named)
                .find(|(owner, _)| in_schema(schema, owner)),
            None => self
                .relations()
                .filter(named)
                .find(|(owner, _)| in_schema(&self.default_schema, owner))
                .or_else(|| self.relations().find(named)),
        };
        found.map(|(_, relation)| relation)
    }

    /// The names of the loaded objects of `kinds` at the top of the tree or in a folder there,
    /// system ones left out: what a language without SQL's clauses names in its commands, such
    /// as an Elasticsearch cluster's indices, aliases and data streams.
    pub fn top_level_names(&self, kinds: &[NodeKind]) -> Vec<String> {
        let top = self
            .tree
            .loaded_children(&CatalogPath::root())
            .unwrap_or_default();
        let in_folders = top
            .iter()
            .filter(|node| matches!(node.kind(), Some(NodeKind::Folder(_))))
            .flat_map(|folder| self.tree.loaded_children(&folder.path).unwrap_or_default());
        top.iter()
            .chain(in_folders)
            .filter(|node| !node.system && node.kind().is_some_and(|kind| kinds.contains(&kind)))
            .map(|node| node.label.clone())
            .collect()
    }

    /// The top of the tree and its folders, while they haven't loaded: what
    /// [`Self::top_level_names`] reads.
    pub fn missing_top_level(&self) -> Needed {
        let mut missing = Needed::default();
        let mut need = |path: &CatalogPath| match self.tree.children(path) {
            None => missing.unrequested.push(path.clone()),
            Some(Children::Loading) => missing.loading = true,
            Some(Children::Loaded(_) | Children::Failed(_)) => {}
        };
        let root = CatalogPath::root();
        need(&root);
        for folder in self.tree.loaded_children(&root).unwrap_or_default() {
            if matches!(folder.kind(), Some(NodeKind::Folder(_))) {
                need(&folder.path);
            }
        }
        missing
    }
}

impl CatalogSource for TreeCatalog<'_> {
    fn default_schema(&self) -> &str {
        &self.default_schema
    }

    fn schemas(&self) -> Vec<String> {
        self.schema_nodes()
            .map(|schema| schema.label.clone())
            .collect()
    }

    fn tables(&self) -> Vec<TableEntry> {
        self.relations()
            .map(|(schema, relation)| TableEntry {
                schema: schema.label.clone(),
                name: relation.label.clone(),
                is_view: relation.kind() != Some(NodeKind::Table),
            })
            .collect()
    }

    fn columns(&self, table: &TableEntry) -> Option<Vec<ColumnEntry>> {
        let reference = TableRef {
            schema: Some(table.schema.clone()),
            table: table.name.clone(),
            alias: None,
            range: 0..0,
        };
        let relation = self.relation_node(&reference)?;
        let columns = self.folder(&relation.path, FolderKind::Columns)?;
        let nodes = self.tree.loaded_children(&columns.path)?;
        Some(
            nodes
                .iter()
                .map(|column| ColumnEntry {
                    name: column.label.clone(),
                    detail: column.detail.clone(),
                })
                .collect(),
        )
    }

    /// Functions are labelled with their arguments, `add(integer, integer)`, on PostgreSQL.
    fn functions(&self) -> Vec<FunctionEntry> {
        self.schema_nodes()
            .filter_map(|schema| {
                let folder = self.folder(&schema.path, FolderKind::Functions)?;
                let functions = self.tree.loaded_children(&folder.path)?;
                Some(functions.iter().map(|function| {
                    let (name, arguments) = match function.label.find('(') {
                        Some(open) => (
                            function.label[..open].to_owned(),
                            Some(function.label[open..].to_owned()),
                        ),
                        None => (function.label.clone(), None),
                    };
                    FunctionEntry {
                        schema: schema.label.clone(),
                        name,
                        arguments,
                    }
                }))
            })
            .flatten()
            .collect()
    }

    fn tables_loaded(&self, schema: Option<&str>) -> bool {
        let loaded =
            |path: &CatalogPath| matches!(self.tree.children(path), Some(Children::Loaded(_)));
        if !loaded(&self.schemas_parent) {
            return false;
        }
        let mut schemas = self
            .schema_nodes()
            .filter(|node| schema.is_none_or(|name| node.label.eq_ignore_ascii_case(name)))
            .peekable();
        let any = schemas.peek().is_some();
        let all_loaded = schemas.all(|schema| {
            loaded(&schema.path)
                && self
                    .relation_folders(&schema.path)
                    .all(|folder| loaded(&folder.path))
        });
        any && all_loaded
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::SqliteConfig;

    use super::*;

    fn node(path: CatalogPath, system: bool) -> CatalogNode {
        CatalogNode {
            label: path.last().map(|s| s.name.clone()).unwrap_or_default(),
            path,
            detail: None,
            has_children: true,
            has_definition: false,
            system,
        }
    }

    fn load(tree: &mut CatalogTree, path: &CatalogPath, children: Vec<CatalogNode>) {
        assert!(tree.request(path));
        tree.loaded_whole(path, Ok(children));
    }

    #[test]
    fn asks_for_one_level_at_a_time_and_reads_what_loaded() {
        let config = ConnectionConfig::Sqlite(SqliteConfig {
            path: "/data/shop.db".into(),
            read_only: false,
        });
        let mut tree = CatalogTree::default();
        let root = CatalogPath::root();
        let main = root.child(NodeKind::Schema, "main");
        let tables = CatalogNode::folder(&main, FolderKind::Tables).path;
        let orders = tables.child(NodeKind::Table, "orders");
        let columns = CatalogNode::folder(&orders, FolderKind::Columns).path;
        let references = [TableRef {
            schema: None,
            table: "orders".to_owned(),
            alias: None,
            range: 0..0,
        }];

        let unrequested = |tree: &CatalogTree| {
            TreeCatalog::new(tree, &config)
                .missing(&references)
                .unrequested
        };
        assert_eq!(unrequested(&tree), std::slice::from_ref(&root));
        assert!(tree.request(&root));
        assert!(
            TreeCatalog::new(&tree, &config)
                .missing(&references)
                .loading
        );
        tree.loaded_whole(
            &root,
            Ok(vec![
                node(main.clone(), false),
                node(root.child(NodeKind::Schema, "temp"), true),
            ]),
        );
        assert_eq!(unrequested(&tree), std::slice::from_ref(&main));
        load(
            &mut tree,
            &main,
            vec![CatalogNode::folder(&main, FolderKind::Tables)],
        );
        load(&mut tree, &tables, vec![node(orders.clone(), false)]);
        assert_eq!(unrequested(&tree), std::slice::from_ref(&orders));
        load(
            &mut tree,
            &orders,
            vec![CatalogNode::folder(&orders, FolderKind::Columns)],
        );
        load(
            &mut tree,
            &columns,
            vec![node(columns.child(NodeKind::Column, "id"), false)],
        );

        let catalog = TreeCatalog::new(&tree, &config);
        assert!(!catalog.missing(&references).is_pending());
        assert_eq!(catalog.schemas(), ["main"]);
        let found = catalog.tables();
        assert_eq!(found.len(), 1);
        assert_eq!(
            catalog.columns(&found[0]).unwrap(),
            [ColumnEntry {
                name: "id".to_owned(),
                detail: None
            }]
        );
    }

    /// A search console names the cluster's indices, aliases and data streams, one folder
    /// down: completion asks for the folders once the top has loaded, and leaves out system
    /// ones and other kinds.
    #[test]
    fn reads_the_names_at_the_top_of_the_tree() {
        let config = ConnectionConfig::Elasticsearch(dbrover_core::ElasticsearchConfig {
            server: dbrover_core::ServerConfig {
                host: "localhost".to_owned(),
                port: 9200.into(),
                database: String::new(),
                user: String::new(),
                tls: dbrover_core::TlsMode::Disable,
                ssh: None,
                password_command: None,
            },
            auth: dbrover_core::ElasticsearchAuth::Password,
        });
        let mut tree = CatalogTree::default();
        let root = CatalogPath::root();
        let indices = CatalogNode::folder(&root, FolderKind::Indices);
        let templates = CatalogNode::folder(&root, FolderKind::IndexTemplates);
        let kinds = [NodeKind::SearchIndex];
        assert_eq!(
            TreeCatalog::new(&tree, &config)
                .missing_top_level()
                .unrequested,
            std::slice::from_ref(&root)
        );
        load(&mut tree, &root, vec![indices.clone(), templates.clone()]);
        assert_eq!(
            TreeCatalog::new(&tree, &config)
                .missing_top_level()
                .unrequested,
            [indices.path.clone(), templates.path.clone()]
        );
        load(
            &mut tree,
            &indices.path,
            vec![
                node(indices.path.child(NodeKind::SearchIndex, "orders"), false),
                node(indices.path.child(NodeKind::SearchIndex, ".kibana"), true),
            ],
        );
        load(
            &mut tree,
            &templates.path,
            vec![node(
                templates.path.child(NodeKind::IndexTemplate, "logs"),
                false,
            )],
        );
        let catalog = TreeCatalog::new(&tree, &config);
        assert!(!catalog.missing_top_level().is_pending());
        assert_eq!(catalog.top_level_names(&kinds), ["orders"]);
    }
}
