//! New schema… and Drop schema…: `CREATE SCHEMA` in a database, or `CREATE DATABASE` where
//! the schemas in the tree are databases.

use dbrover_core::{CatalogPath, FolderKind, NodeKind};
use dbrover_sql::{DdlError, DdlOperation, Dialect, SchemaCreation, SchemaSpec};
use egui::{Grid, TextEdit, Ui};

use super::{Destruction, FormContext, ObjectForm};
use crate::catalog_tree::CatalogTree;

const NAME_WIDTH: f32 = 260.0;

/// "schema", or "database" where the dialect's schemas are databases.
fn noun(dialect: Dialect) -> &'static str {
    match dialect.capabilities().schema_creation {
        SchemaCreation::CreateDatabase => "database",
        SchemaCreation::Unsupported | SchemaCreation::CreateSchema => "schema",
    }
}

pub(crate) struct NewSchema {
    /// The node the schemas are listed under: a database, or the data source.
    parent: CatalogPath,
    name: String,
    owner: String,
}

impl NewSchema {
    /// On a database, or on the data source (the root) where its schemas are its top level.
    pub(crate) fn new(path: CatalogPath) -> Option<Self> {
        let on_database = path.last().map(|segment| segment.kind) == Some(NodeKind::Database);
        (on_database || path.is_root()).then(|| Self {
            parent: path,
            name: String::new(),
            owner: String::new(),
        })
    }
}

impl ObjectForm for NewSchema {
    fn title(&self) -> String {
        "New schema".to_owned()
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        let dialect = context.target.dialect;
        Grid::new("new_schema").num_columns(2).show(ui, |ui| {
            let label = ui.label("Name");
            ui.add(TextEdit::singleline(&mut self.name).desired_width(NAME_WIDTH))
                .labelled_by(label.id);
            ui.end_row();
            if dialect.capabilities().schema_owner {
                let label = ui.label("Owner");
                ui.add(
                    TextEdit::singleline(&mut self.owner)
                        .desired_width(NAME_WIDTH)
                        .hint_text("you"),
                )
                .labelled_by(label.id);
                ui.end_row();
            }
        });
        if noun(dialect) == "database" {
            ui.weak(format!(
                "{}'s schemas are its databases, so this creates a database.",
                dialect.display_name()
            ));
        }
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        let owner = self.owner.trim();
        Ok(vec![DdlOperation::CreateSchema(SchemaSpec {
            name: self.name.trim().to_owned(),
            owner: (!owner.is_empty()).then(|| owner.to_owned()),
        })])
    }

    fn changed(&self) -> CatalogPath {
        self.parent.clone()
    }
}

/// Drops a schema, and with `CASCADE` (or always, for a database) everything in it. The
/// confirmation counts what it holds, as far as the tree has loaded it.
pub(crate) struct DropSchema {
    path: CatalogPath,
    name: String,
    cascade: bool,
    /// The schema's folders, once it is loaded.
    folders: Vec<FolderKind>,
    /// How many objects each loaded folder holds.
    contents: Vec<(FolderKind, usize)>,
}

impl DropSchema {
    /// On a schema.
    pub(crate) fn new(path: CatalogPath) -> Option<Self> {
        let schema = path.last()?;
        if schema.kind != NodeKind::Schema {
            return None;
        }
        Some(Self {
            name: schema.name.clone(),
            path,
            cascade: false,
            folders: Vec::new(),
            contents: Vec::new(),
        })
    }

    fn folder(&self, kind: FolderKind) -> CatalogPath {
        self.path.child(NodeKind::Folder(kind), kind.label())
    }

    /// Reads the schema's folders and what they hold from the loaded tree.
    fn count(&mut self, catalog: &CatalogTree) {
        self.folders = catalog
            .loaded_children(&self.path)
            .into_iter()
            .flatten()
            .filter_map(|node| match node.kind() {
                Some(NodeKind::Folder(kind)) => Some(kind),
                _ => None,
            })
            .collect();
        self.contents = self
            .folders
            .iter()
            .filter_map(|kind| {
                let objects = catalog.loaded_children(&self.folder(*kind))?;
                Some((*kind, objects.iter().filter(|node| !node.system).count()))
            })
            .collect();
    }

    /// "3 tables and 1 view", or `None` for an empty schema.
    fn holds(&self) -> Option<String> {
        let counts: Vec<String> = self
            .contents
            .iter()
            .filter(|(_, count)| *count > 0)
            .map(|(folder, count)| match count {
                1 => format!("1 {}", singular(*folder)),
                n => format!("{n} {}", folder.label()),
            })
            .collect();
        match counts.as_slice() {
            [] => None,
            [one] => Some(one.clone()),
            [rest @ .., last] => Some(format!("{} and {last}", rest.join(", "))),
        }
    }
}

fn singular(folder: FolderKind) -> &'static str {
    match folder {
        FolderKind::Tables => "table",
        FolderKind::Views => "view",
        FolderKind::MaterializedViews => "materialized view",
        FolderKind::Functions => "function",
        FolderKind::Procedures => "procedure",
        FolderKind::Sequences => "sequence",
        FolderKind::Events => "event",
        FolderKind::Synonyms => "synonym",
        FolderKind::Dictionaries => "dictionary",
        FolderKind::Columns => "column",
        FolderKind::Indexes => "index",
        FolderKind::Constraints => "constraint",
        FolderKind::Triggers => "trigger",
        FolderKind::Roles => "role",
        FolderKind::Indices => "index",
        FolderKind::Aliases => "alias",
        FolderKind::DataStreams => "data stream",
        FolderKind::IndexTemplates => "index template",
    }
}

impl ObjectForm for DropSchema {
    fn title(&self) -> String {
        format!("Drop schema {}", self.name)
    }

    fn show(&mut self, ui: &mut Ui, context: &FormContext<'_>) {
        self.count(context.catalog);
        if context.target.dialect.capabilities().drop_schema_cascade {
            ui.checkbox(&mut self.cascade, "Also drop everything in it (CASCADE)");
        }
    }

    /// The schema's folders, then what each holds, to count.
    fn catalog_requests(&self) -> Vec<CatalogPath> {
        let mut requests = vec![self.path.clone()];
        requests.extend(self.folders.iter().map(|kind| self.folder(*kind)));
        requests
    }

    fn operations(&self) -> Result<Vec<DdlOperation>, DdlError> {
        Ok(vec![DdlOperation::DropSchema {
            schema: self.name.clone(),
            cascade: self.cascade,
        }])
    }

    fn changed(&self) -> CatalogPath {
        self.path.parent().unwrap_or_default()
    }

    fn destruction(&self, dialect: Dialect) -> Option<Destruction> {
        let capabilities = dialect.capabilities();
        let name = dbrover_sql::quote_name(dialect, &self.name);
        let holds = self.holds();
        let (summary, consequences) = match capabilities.schema_creation {
            SchemaCreation::CreateDatabase => (
                format!(
                    "Drops the database {name} and everything in it{}.",
                    holds.map(|holds| format!(": {holds}")).unwrap_or_default()
                ),
                Vec::new(),
            ),
            _ if self.cascade => (
                format!("Drops the schema {name} and everything in it."),
                vec![match holds {
                    Some(holds) => format!("CASCADE drops {holds}, with their rows."),
                    None => "CASCADE drops whatever it holds.".to_owned(),
                }],
            ),
            _ => (
                format!("Drops the schema {name}."),
                vec![match holds {
                    Some(holds) => format!(
                        "It holds {holds}: the database refuses to drop a schema that isn't \
                         empty{}.",
                        if capabilities.drop_schema_cascade {
                            ", unless CASCADE is ticked"
                        } else {
                            ""
                        }
                    ),
                    None => "The database refuses if it isn't empty.".to_owned(),
                }],
            ),
        };
        Some(Destruction {
            name: self.name.clone(),
            summary,
            consequences,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sales() -> CatalogPath {
        CatalogPath::root()
            .child(NodeKind::Database, "shop")
            .child(NodeKind::Schema, "sales")
    }

    /// How many objects one folder holds.
    type Count = (FolderKind, usize);

    fn holding(contents: &[Count]) -> DropSchema {
        let mut form = DropSchema::new(sales()).unwrap();
        form.contents = contents.to_vec();
        form
    }

    #[test]
    fn new_schema_goes_in_a_database_or_the_data_source() {
        let database = CatalogPath::root().child(NodeKind::Database, "shop");
        let cases = [
            (database.clone(), Some(database)),
            (CatalogPath::root(), Some(CatalogPath::root())),
            (sales(), None),
        ];
        for (path, expected) in cases {
            let parent = NewSchema::new(path.clone()).map(|form| form.changed());
            assert_eq!(parent, expected, "{path}");
        }
        assert!(DropSchema::new(CatalogPath::root()).is_none());
    }

    #[test]
    fn counts_what_the_schema_holds() {
        let cases: [(&[Count], Option<&str>); 4] = [
            (&[], None),
            (&[(FolderKind::Tables, 0), (FolderKind::Views, 0)], None),
            (&[(FolderKind::Tables, 1)], Some("1 table")),
            (
                &[
                    (FolderKind::Tables, 3),
                    (FolderKind::Views, 1),
                    (FolderKind::Sequences, 0),
                    (FolderKind::Functions, 2),
                ],
                Some("3 tables, 1 view and 2 functions"),
            ),
        ];
        for (contents, expected) in cases {
            assert_eq!(
                holding(contents).holds().as_deref(),
                expected,
                "{contents:?}"
            );
        }
    }

    #[test]
    fn the_confirmation_names_what_goes_with_the_schema() {
        let contents = [(FolderKind::Tables, 2), (FolderKind::Views, 1)];
        let mut form = holding(&contents);
        let postgres = form.destruction(Dialect::Postgres).unwrap();
        assert_eq!(postgres.summary, "Drops the schema sales.");
        assert_eq!(
            postgres.consequences,
            [
                "It holds 2 tables and 1 view: the database refuses to drop a schema that isn't \
              empty, unless CASCADE is ticked."
            ]
        );
        form.cascade = true;
        let cascade = form.destruction(Dialect::Postgres).unwrap();
        assert_eq!(
            cascade.consequences,
            ["CASCADE drops 2 tables and 1 view, with their rows."]
        );
        let mysql = holding(&contents).destruction(Dialect::Mysql).unwrap();
        assert_eq!(
            mysql.summary,
            "Drops the database sales and everything in it: 2 tables and 1 view."
        );
        assert_eq!(mysql.name, "sales");
    }
}
