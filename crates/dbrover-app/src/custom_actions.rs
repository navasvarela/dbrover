//! Custom actions from `actions.toml`: which are offered on what, and the SQL each fills in.
//! Running it is the console's usual run, so parameters, the safety checks and the history
//! apply as to SQL typed by hand. See `docs/hackability.md`.

use dbrover_core::{
    CatalogPath, DataSource, DriverKind, NodeKind, Quoting, Row, SelectedCells, TemplateValues,
};
use dbrover_store::{ActionBody, ActionTarget, CustomAction};

/// The grid's cells an action was chosen on, and the table they come from when the grid knows
/// it (an editable result's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridCells {
    /// Every column of the result, by name.
    pub columns: Vec<String>,
    /// The selected columns, as indices into `columns`, left to right.
    pub selected: Vec<usize>,
    /// The selected rows, top to bottom, with every column's value.
    pub rows: Vec<Row>,
    pub table: Option<GridTable>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridTable {
    pub schema: String,
    pub name: String,
}

/// What an action is chosen on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// An explorer node; the root path is the data source's own row.
    Node(CatalogPath),
    Cells(GridCells),
}

/// What kind of thing is selected, which decides the actions the menu bar and keys can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubjectKind {
    pub driver: DriverKind,
    pub on: SubjectOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectOn {
    Node(ActionTarget),
    Cells,
}

impl SubjectKind {
    pub fn offers(self, action: &CustomAction) -> bool {
        match self.on {
            SubjectOn::Node(target) => action.applies_to(target, self.driver),
            SubjectOn::Cells => grid_target(action).is_some() && action.runs_on(self.driver),
        }
    }
}

/// What an explorer node is to `applies_to`: the data source for the root, a node kind
/// otherwise. `None` for folders, which no action applies to.
pub fn node_target(path: &CatalogPath) -> Option<ActionTarget> {
    match path.last().map(|segment| segment.kind) {
        None => Some(ActionTarget::DataSource),
        Some(NodeKind::Folder(_)) => None,
        Some(kind) => Some(ActionTarget::Node(kind)),
    }
}

/// How an action acts on grid cells: once for the selection, or once per row. An action that
/// lists both runs once.
pub fn grid_target(action: &CustomAction) -> Option<ActionTarget> {
    [ActionTarget::GridSelection, ActionTarget::GridRow]
        .into_iter()
        .find(|target| action.applies_to.contains(target))
}

/// The actions to offer on `target`, in the file's order.
pub fn offered(
    actions: &[CustomAction],
    target: ActionTarget,
    driver: DriverKind,
) -> impl Iterator<Item = &CustomAction> {
    actions
        .iter()
        .filter(move |action| action.applies_to(target, driver))
}

/// The console text an action fills in on `subject`: one statement, or one per selected row,
/// each ending in `;`.
///
/// # Errors
///
/// What the action needs that `subject` lacks, as a sentence naming the action.
pub fn fill(
    action: &CustomAction,
    data_source: &DataSource,
    subject: &Subject,
    quoting: Quoting,
) -> Result<String, String> {
    let ActionBody::Sql { sql, .. } = &action.body else {
        return Err(format!("{}: runs a program, not SQL", action.name));
    };
    let statements = values(action, data_source, subject, quoting)
        .iter()
        .map(|values| sql.render(values, quoting))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{}: {error}", action.name))?;
    Ok(statements
        .iter()
        .map(|sql| {
            let sql = sql.trim_end();
            if sql.ends_with(';') {
                format!("{sql}\n")
            } else {
                format!("{sql};\n")
            }
        })
        .collect())
}

/// What fills a command action's placeholders on `subject`. A command never runs per row, so
/// there is one set, whatever the action applies to.
pub fn command_values(
    data_source: &DataSource,
    subject: &Subject,
    quoting: Quoting,
) -> TemplateValues {
    let base = base_values(data_source);
    match subject {
        Subject::Node(path) => node_values(path, base, quoting),
        Subject::Cells(cells) => cell_values(cells, base, quoting),
    }
}

fn base_values(data_source: &DataSource) -> TemplateValues {
    TemplateValues {
        database: data_source.config.database().map(str::to_owned),
        data_source: data_source.name.clone(),
        ..TemplateValues::default()
    }
}

/// What fills the placeholders, per statement.
fn values(
    action: &CustomAction,
    data_source: &DataSource,
    subject: &Subject,
    quoting: Quoting,
) -> Vec<TemplateValues> {
    let base = base_values(data_source);
    match subject {
        Subject::Node(path) => vec![node_values(path, base, quoting)],
        Subject::Cells(cells) if grid_target(action) == Some(ActionTarget::GridRow) => cells
            .rows
            .iter()
            .map(|row| {
                let one_row = GridCells {
                    rows: vec![row.clone()],
                    ..cells.clone()
                };
                cell_values(&one_row, base.clone(), quoting)
            })
            .collect(),
        Subject::Cells(cells) => vec![cell_values(cells, base, quoting)],
    }
}

fn node_values(path: &CatalogPath, base: TemplateValues, quoting: Quoting) -> TemplateValues {
    let name = |kind| path.name_of(kind).map(str::to_owned);
    let table = path
        .segments()
        .iter()
        .rev()
        .find(|segment| {
            matches!(
                segment.kind,
                NodeKind::Table | NodeKind::View | NodeKind::MaterializedView
            )
        })
        .map(|segment| segment.name.clone());
    TemplateValues {
        qualified_name: path.quoted_name(quoting),
        database: name(NodeKind::Database).or(base.database),
        schema: name(NodeKind::Schema),
        table,
        column: name(NodeKind::Column),
        ..base
    }
}

fn cell_values(cells: &GridCells, base: TemplateValues, quoting: Quoting) -> TemplateValues {
    let table = cells.table.as_ref();
    TemplateValues {
        qualified_name: table.map(|table| {
            format!(
                "{}.{}",
                quoting.identifier(&table.schema),
                quoting.identifier(&table.name)
            )
        }),
        schema: table.map(|table| table.schema.clone()),
        table: table.map(|table| table.name.clone()),
        column: cells
            .selected
            .first()
            .and_then(|&index| cells.columns.get(index))
            .cloned(),
        cells: Some(SelectedCells {
            columns: cells.columns.clone(),
            selected: cells.selected.clone(),
            rows: cells.rows.clone(),
        }),
        ..base
    }
}

/// A starting `actions.toml`: the plan's examples, commented out, so the file documents the
/// format and adds nothing until a block is uncommented.
pub const TEMPLATE: &str = r#"# DbRover custom actions. Uncomment and change an action, then View → Custom actions… → Reload.
#
# applies_to: node kinds (database, schema, table, view, materialized_view, function, procedure,
#   sequence, column, index, constraint, trigger, role, …), data_source, grid.selection (once for
#   the selected cells) or grid.row (once per selected row).
# drivers: postgres, sqlite, mysql, sqlserver, duckdb, clickhouse, redis, mongodb, elasticsearch;
#   every driver when left out.
# run = false opens the SQL in a console to edit first.
#
# Placeholders, never pasted in as text: names are quoted, values become literals.
#   {qualified_name} {database} {schema} {table} {column} {column:default}
#   {data_source} (a string)  {value} {values} {row.<column>} (from the grid)
# Write {{ for a { that would otherwise start a placeholder.
#
# An action can run a program instead of SQL, with command = [program, arguments…]. There is no
# shell: each placeholder fills one argument, as raw text. Commands also take the data source's
# {host} {port} {user} {file}, never a password. With a command:
#   stdin = "json", "csv" or "tsv" sends the grid's selected cells (default "none");
#   output = "viewer", "console", "log" or "none" says where stdout goes (default "viewer");
#   timeout = "30s" stops it after that long (default 60s).
# DbRover shows each command in full and asks before its first run.
#
# Bind an action to a key in keymap.toml as "custom.<id>" = "cmd+shift+y".

# [[action]]
# id = "row-count"
# name = "Row count"
# applies_to = ["table", "view"]
# sql = "SELECT count(*) FROM {qualified_name}"

# [[action]]
# id = "latest"
# name = "Last 100 rows"
# applies_to = ["table"]
# sql = "SELECT * FROM {qualified_name} ORDER BY {column:created_at} DESC LIMIT 100"
# run = false

# [[action]]
# id = "rows-to-jq"
# name = "Send rows to jq"
# applies_to = ["grid.selection"]
# command = ["jq", "-r", ".[] | .email"]
# stdin = "json"
# output = "viewer"
"#;

#[cfg(test)]
mod tests {
    use dbrover_core::{ConnectionConfig, DataSourceId, FolderKind, SafetyLevel, SqliteConfig};
    use dbrover_store::parse_actions;

    use super::*;

    fn shop() -> DataSource {
        DataSource {
            id: DataSourceId::generate(),
            name: "shop".to_owned(),
            color: None,
            safety: SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Sqlite(SqliteConfig {
                path: "/data/shop.db".into(),
                read_only: false,
            }),
        }
    }

    fn action(applies_to: &str, sql: &str) -> CustomAction {
        let checked = parse_actions(&format!(
            "[[action]]\nid = \"a\"\nname = \"An action\"\napplies_to = [{applies_to}]\n\
             sql = {sql:?}\n"
        ));
        assert!(checked.problems.is_empty(), "{:?}", checked.problems);
        checked.value.into_iter().next().unwrap()
    }

    fn table(name: &str) -> CatalogPath {
        CatalogPath::root()
            .child(NodeKind::Schema, "main")
            .child(NodeKind::Folder(FolderKind::Tables), "tables")
            .child(NodeKind::Table, name)
    }

    fn cells() -> GridCells {
        let text = |value: &str| Some(value.to_owned());
        GridCells {
            columns: vec!["id".to_owned(), "email".to_owned()],
            selected: vec![1],
            rows: vec![vec![text("1"), text("a@x")], vec![text("2"), None]],
            table: None,
        }
    }

    #[test]
    fn fills_names_from_the_node_quoted_whatever_they_spell() {
        let count = action(
            "\"table\"",
            "SELECT count(*) FROM {qualified_name} -- {data_source}",
        );
        let hostile = table("x\"; DROP TABLE y; --");

        let sql = fill(&count, &shop(), &Subject::Node(hostile), Quoting::STANDARD).unwrap();

        assert_eq!(
            sql,
            "SELECT count(*) FROM \"main\".\"x\"\"; DROP TABLE y; --\" -- 'shop';\n"
        );
    }

    #[test]
    fn a_column_node_fills_its_table_and_column() {
        let latest = action(
            "\"table\", \"column\"",
            "SELECT * FROM {schema}.{table} ORDER BY {column:created_at} DESC",
        );
        let on_table = fill(
            &latest,
            &shop(),
            &Subject::Node(table("orders")),
            Quoting::STANDARD,
        );
        let column = table("orders").child(NodeKind::Column, "placed_at");
        let on_column = fill(&latest, &shop(), &Subject::Node(column), Quoting::STANDARD);

        assert_eq!(
            on_table.unwrap(),
            "SELECT * FROM \"main\".\"orders\" ORDER BY \"created_at\" DESC;\n"
        );
        assert_eq!(
            on_column.unwrap(),
            "SELECT * FROM \"main\".\"orders\" ORDER BY \"placed_at\" DESC;\n"
        );
    }

    #[test]
    fn a_selection_runs_once_and_rows_once_each() {
        let once = action(
            "\"grid.selection\"",
            "SELECT * FROM users WHERE {column} IN ({values});",
        );
        let each = action("\"grid.row\"", "DELETE FROM users WHERE id = {row.id}");
        let subject = Subject::Cells(cells());

        assert_eq!(
            fill(&once, &shop(), &subject, Quoting::STANDARD).unwrap(),
            "SELECT * FROM users WHERE \"email\" IN ('a@x', NULL);\n"
        );
        assert_eq!(
            fill(&each, &shop(), &subject, Quoting::STANDARD).unwrap(),
            "DELETE FROM users WHERE id = '1';\nDELETE FROM users WHERE id = '2';\n"
        );
    }

    #[test]
    fn names_the_action_when_the_subject_lacks_a_value() {
        let by_table = action("\"grid.selection\"", "SELECT count(*) FROM {table}");
        let error = fill(
            &by_table,
            &shop(),
            &Subject::Cells(cells()),
            Quoting::STANDARD,
        );
        assert_eq!(
            error,
            Err("An action: nothing here fills {table}".to_owned())
        );

        let with_table = Subject::Cells(GridCells {
            table: Some(GridTable {
                schema: "main".to_owned(),
                name: "users".to_owned(),
            }),
            ..cells()
        });
        assert_eq!(
            fill(&by_table, &shop(), &with_table, Quoting::STANDARD).unwrap(),
            "SELECT count(*) FROM \"users\";\n"
        );
    }

    #[test]
    fn offers_actions_by_what_is_selected_and_the_driver() {
        let on_tables = action("\"table\"", "SELECT 1");
        let on_cells = action("\"grid.row\"", "SELECT {value}");
        let table_kind = SubjectKind {
            driver: DriverKind::Sqlite,
            on: SubjectOn::Node(ActionTarget::Node(NodeKind::Table)),
        };
        let cells_kind = SubjectKind {
            driver: DriverKind::Sqlite,
            on: SubjectOn::Cells,
        };

        assert!(table_kind.offers(&on_tables));
        assert!(!table_kind.offers(&on_cells));
        assert!(cells_kind.offers(&on_cells));
        assert!(!cells_kind.offers(&on_tables));
        assert_eq!(
            node_target(&CatalogPath::root()),
            Some(ActionTarget::DataSource)
        );
        assert_eq!(node_target(&table("x").parent().unwrap()), None);
    }

    #[test]
    fn the_template_adds_nothing_until_uncommented_and_then_reads_cleanly() {
        let as_written = dbrover_store::parse_actions(TEMPLATE);
        assert!(as_written.value.is_empty() && as_written.problems.is_empty());

        let uncommented: String = TEMPLATE
            .lines()
            .skip_while(|line| *line != "# [[action]]")
            .map(|line| format!("{}\n", line.strip_prefix("# ").unwrap_or(line)))
            .collect();
        let checked = dbrover_store::parse_actions(&uncommented);
        assert!(checked.problems.is_empty(), "{:?}", checked.problems);
        assert_eq!(checked.value.len(), 3);
    }
}
