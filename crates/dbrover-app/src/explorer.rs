//! The connection explorer panel: data sources and their catalogs as a lazily loaded tree.
//! It only reports what the user asked for; the app turns that into engine commands.

use std::collections::BTreeSet;

use dbrover_core::{
    CatalogNode, CatalogPath, DataSourceId, DriverKind, FolderKind, NodeKind, ProjectId, Quoting,
};
use dbrover_engine::SessionState;
use dbrover_language::QueryLanguage;
use dbrover_sql::{Capabilities, Dialect, SchemaCreation, TableChange, UserModel};
use dbrover_store::{ActionId, ActionTarget, ConsoleKey, CustomAction, DiagramKey, Project};
use egui::{
    Color32, CornerRadius, Id, Label, Rect, Response, RichText, ScrollArea, Sense, Shape, TextEdit,
    TextStyle, Ui, WidgetInfo, WidgetType, vec2,
};

use crate::catalog_tree::{CatalogTree, Children, LevelEnd};
use crate::consoles::SavedConsoles;
use crate::custom_actions;
use crate::diagram::SavedDiagrams;
use crate::icons;
use crate::mcp_client_config;
use crate::state::{DataSourceEntry, DataSources};
use crate::theme::Palette;
use crate::widgets::{self, IconButton};

const STATUS_DOT_RADIUS: f32 = 3.5;
/// How strongly a data source's colour tints its row.
const SOURCE_ROW_TINT: f32 = 0.16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplorerAction {
    New(DriverKind),
    Edit(DataSourceId),
    Delete(DataSourceId),
    Connect(DataSourceId),
    Disconnect(DataSourceId),
    Expand(DataSourceId, CatalogPath),
    Collapse(DataSourceId, CatalogPath),
    /// **Load more…** at the end of a partial level: the next page of the node's children.
    LoadMore(DataSourceId, CatalogPath),
    Refresh(DataSourceId, CatalogPath),
    ShowDefinition {
        id: DataSourceId,
        path: CatalogPath,
        title: String,
    },
    NewConsole(DataSourceId),
    /// Reopens a console from its file.
    OpenSavedConsole(ConsoleKey),
    /// Opens a console that selects everything from the table or view at `path`.
    OpenData(DataSourceId, CatalogPath),
    /// Asks for a new project's name; from a data source's Move to project menu, that data
    /// source then moves into it.
    NewProject {
        then_move: Option<DataSourceId>,
    },
    RenameProject(ProjectId),
    /// Opens the project's page in Settings, where MCP access is set.
    ProjectSettings(ProjectId),
    DeleteProject(ProjectId),
    /// Puts the data source in the project, or in no project.
    MoveToProject(DataSourceId, Option<ProjectId>),
    /// Opens an object helper's dialog on the data source.
    Helper(DataSourceId, Helper),
    /// Copies the statement that creates the table or view at the path.
    CopyDdl(DataSourceId, CatalogPath),
    /// Asks for a CSV file, then opens the dialog that imports it into the table at the path.
    ImportCsv(DataSourceId, CatalogPath),
    /// Export data…: every row of a table or view, to a file.
    ExportData(DataSourceId, CatalogPath),
    /// Diagram…: asks which of the schema's tables at the path to draw.
    Diagram(DataSourceId, CatalogPath),
    /// Diagram with related tables: the table at the path and every table its keys join.
    DiagramRelated(DataSourceId, CatalogPath),
    /// Reopens a saved diagram, or brings its tab to the front.
    OpenDiagram(DiagramKey),
    /// Makes the node at the path, or the data source for the root, the one that keys and the
    /// Actions menu act on.
    Select(DataSourceId, CatalogPath),
    /// Runs a custom action from `actions.toml` on the node at the path.
    RunAction {
        data_source: DataSourceId,
        path: CatalogPath,
        action: ActionId,
    },
}

/// An object helper, with the catalog node it was opened on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Helper {
    /// On a schema, or its tables folder.
    NewTable(CatalogPath),
    /// On a table, as are the next three.
    ModifyTable(CatalogPath),
    RenameTable(CatalogPath),
    TruncateTable(CatalogPath),
    DropTable(CatalogPath),
    /// On a table, or its indexes folder.
    NewIndex(CatalogPath),
    DropIndex(CatalogPath),
    /// On a schema, or its views folder.
    NewView(CatalogPath),
    /// On a schema, or its views or materialized views folder.
    NewMaterializedView(CatalogPath),
    /// On a view or a materialized view, as are the next two.
    EditView(CatalogPath),
    RenameView(CatalogPath),
    DropView(CatalogPath),
    /// On a materialized view.
    RefreshView(CatalogPath),
    /// On a database, or on the data source (the root) where its schemas are databases.
    NewSchema(CatalogPath),
    DropSchema(CatalogPath),
    /// On the data source (the root) or its roles folder, where users are roles, as the next
    /// one.
    NewUser(CatalogPath),
    NewRole(CatalogPath),
    /// On a role, as the next two.
    EditRole(CatalogPath),
    RenameRole(CatalogPath),
    DropRole(CatalogPath),
}

/// The explorer's own view settings.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExplorerOptions {
    /// Case-insensitive text matched against loaded node labels. A level the server pages is
    /// asked for again with it, by the app, once typing pauses; a level loaded whole never is.
    pub filter: String,
    /// Show `pg_catalog`, `information_schema`, `sqlite_sequence` and the like.
    pub show_system: bool,
    /// Project groups the user folded. Groups start open, so a new project's data sources
    /// are in sight.
    pub collapsed_groups: BTreeSet<Group>,
    /// The node last clicked: the root path for a data source's own row.
    pub selected: Option<(DataSourceId, CatalogPath)>,
}

/// A group of data sources in the explorer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    Project(ProjectId),
    /// Data sources in no project, or in one `projects.toml` doesn't know.
    NoProject,
}

/// What the explorer shows.
pub struct ExplorerView<'a> {
    pub data_sources: &'a DataSources,
    /// Sorted by name. With none, data sources are listed without group headers.
    pub projects: &'a [Project],
    /// Offered for reopening in each data source's menu.
    pub saved_consoles: &'a SavedConsoles,
    /// Offered for reopening in each data source's menu, too.
    pub saved_diagrams: &'a SavedDiagrams,
    /// Offered in the menus of the nodes they apply to.
    pub actions: &'a [CustomAction],
}

pub fn show(
    ui: &mut Ui,
    view: &ExplorerView<'_>,
    options: &mut ExplorerOptions,
    actions: &mut Vec<ExplorerAction>,
) {
    toolbar(ui, options, actions);
    ui.separator();
    match view.data_sources {
        DataSources::Loading => {
            ui.weak("Loading data sources…");
        }
        DataSources::Unavailable(error) => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Can't read data sources: {error}"),
            );
        }
        DataSources::Loaded(entries) if entries.is_empty() && view.projects.is_empty() => {
            ui.weak("No data sources yet. Add one with +.");
        }
        DataSources::Loaded(entries) => {
            let needle = options.filter.trim().to_lowercase();
            let filter = TreeFilter {
                needle: (!needle.is_empty()).then_some(needle.as_str()),
                show_system: options.show_system,
            };
            let tree = SourcesView {
                projects: view.projects,
                saved_consoles: view.saved_consoles,
                saved_diagrams: view.saved_diagrams,
                actions: view.actions,
                selected: options.selected.as_ref(),
            };
            let collapsed = &mut options.collapsed_groups;
            ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                if view.projects.is_empty() {
                    data_sources(ui, &tree, entries.iter(), &filter, actions);
                } else {
                    for (group, members) in groups(view.projects, entries) {
                        group_tree(ui, &tree, group, &members, &filter, collapsed, actions);
                    }
                }
            });
        }
    }
}

/// What every data source row needs besides its entry.
struct SourcesView<'a> {
    projects: &'a [Project],
    saved_consoles: &'a SavedConsoles,
    saved_diagrams: &'a SavedDiagrams,
    actions: &'a [CustomAction],
    selected: Option<&'a (DataSourceId, CatalogPath)>,
}

/// Each project with its data sources, in the projects' order, then No project. Entries keep
/// their order (by name) within a group.
fn groups<'a>(
    projects: &'a [Project],
    entries: &'a [DataSourceEntry],
) -> Vec<(GroupHeader<'a>, Vec<&'a DataSourceEntry>)> {
    let in_project = |entry: &DataSourceEntry, id: ProjectId| entry.data_source.project == Some(id);
    let known = |entry: &DataSourceEntry| {
        entry
            .data_source
            .project
            .is_some_and(|id| projects.iter().any(|project| project.id == id))
    };
    projects
        .iter()
        .map(|project| {
            let members = entries
                .iter()
                .filter(|e| in_project(e, project.id))
                .collect();
            (GroupHeader::Project(project), members)
        })
        .chain(std::iter::once((
            GroupHeader::NoProject,
            entries.iter().filter(|entry| !known(entry)).collect(),
        )))
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum GroupHeader<'a> {
    Project(&'a Project),
    NoProject,
}

impl GroupHeader<'_> {
    fn group(self) -> Group {
        match self {
            Self::Project(project) => Group::Project(project.id),
            Self::NoProject => Group::NoProject,
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Project(project) => &project.name,
            Self::NoProject => "No project",
        }
    }
}

/// A foldable project header with its data sources under it. While filtering, a project
/// whose name matches shows all its data sources; otherwise it shows only those that match,
/// and is hidden when none does.
fn group_tree(
    ui: &mut Ui,
    tree: &SourcesView<'_>,
    header: GroupHeader<'_>,
    members: &[&DataSourceEntry],
    filter: &TreeFilter<'_>,
    collapsed: &mut BTreeSet<Group>,
    actions: &mut Vec<ExplorerAction>,
) {
    let shows_every_member = matches!(header, GroupHeader::Project(_))
        && filter
            .needle
            .is_none_or(|needle| filter.matches(header.label(), needle));
    let members_filter = if shows_every_member {
        filter.below_match()
    } else {
        filter.clone()
    };
    let shown: Vec<&DataSourceEntry> = members
        .iter()
        .copied()
        .filter(|entry| members_filter.source_visibility(entry) != Visibility::Hidden)
        .collect();
    let filtering = filter.needle.is_some();
    if filtering && !shows_every_member && shown.is_empty() {
        return;
    }
    // No project is only worth a header when something is in it.
    if matches!(header, GroupHeader::NoProject) && members.is_empty() {
        return;
    }
    let group = header.group();
    let forced_open = filtering && !shows_every_member;
    let expanded = forced_open || !collapsed.contains(&group);
    let response = group_header_row(ui, header, members.len(), expanded);
    if response.toggled {
        if expanded {
            collapsed.insert(group);
        } else {
            collapsed.remove(&group);
        }
    }
    response
        .name
        .context_menu(|ui| group_menu(ui, header, actions));
    if expanded {
        ui.indent(Id::new(("explorer_group", group)), |ui| {
            if members.is_empty() {
                ui.weak("No data sources. Use Move to project on one.");
            }
            data_sources(ui, tree, shown.into_iter(), &members_filter, actions);
        });
    }
}

struct GroupHeaderResponse {
    name: Response,
    toggled: bool,
}

fn group_header_row(
    ui: &mut Ui,
    header: GroupHeader<'_>,
    count: usize,
    expanded: bool,
) -> GroupHeaderResponse {
    let palette = &Palette::for_visuals(ui.visuals());
    let row = ui.horizontal(|ui| {
        let toggled = expander(ui, expanded, header.label()).clicked();
        let (glyph, color) = match header {
            GroupHeader::Project(_) => (icons::FOLDER_SIMPLE, palette.primary),
            GroupHeader::NoProject => (icons::FOLDER_SIMPLE_DASHED, palette.text_weak),
        };
        ui.label(widgets::glyph(glyph).color(color));
        let text = RichText::new(header.label()).strong();
        let text = match header {
            GroupHeader::Project(_) => text,
            GroupHeader::NoProject => text.color(palette.text_weak),
        };
        let name = ui.add(Label::new(text).selectable(false).sense(Sense::click()));
        widgets::count_badge(ui, count);
        (name, toggled)
    });
    let (name, toggled) = row.inner;
    let toggled = toggled || name.double_clicked();
    let name = match header {
        GroupHeader::Project(project) => name.on_hover_text(format!("Slug: {}", project.slug)),
        GroupHeader::NoProject => name,
    };
    GroupHeaderResponse { name, toggled }
}

fn group_menu(ui: &mut Ui, header: GroupHeader<'_>, actions: &mut Vec<ExplorerAction>) {
    if let GroupHeader::Project(project) = header {
        if widgets::menu_item(ui, icons::PENCIL_SIMPLE, "Rename…").clicked() {
            actions.push(ExplorerAction::RenameProject(project.id));
        }
        if widgets::menu_item(ui, icons::GEAR, "Project settings…").clicked() {
            actions.push(ExplorerAction::ProjectSettings(project.id));
        }
        let copy = widgets::menu_item(ui, icons::COPY, "Copy MCP configuration").on_hover_text(
            "The entry an MCP client's configuration needs to run dbrover mcp for this project",
        );
        if copy.clicked() {
            copy_mcp_configuration(ui, project);
        }
        if widgets::menu_item(ui, icons::TRASH, "Delete…").clicked() {
            actions.push(ExplorerAction::DeleteProject(project.id));
        }
        ui.separator();
    }
    new_project_item(ui, None, actions);
}

pub fn copy_mcp_configuration(ui: &Ui, project: &Project) {
    match mcp_client_config::executable() {
        Ok(executable) => {
            let text = mcp_client_config::client_configuration(&project.slug, &executable);
            ui.ctx().copy_text(text);
            tracing::info!(project = %project.slug, "copied the MCP configuration");
        }
        Err(error) => tracing::error!(
            "could not find DbRover's executable for the MCP configuration: {}",
            dbrover_core::describe_error(&error)
        ),
    }
}

fn new_project_item(
    ui: &mut Ui,
    then_move: Option<DataSourceId>,
    actions: &mut Vec<ExplorerAction>,
) {
    if widgets::menu_item(ui, icons::FOLDER_SIMPLE_PLUS, "New project…").clicked() {
        actions.push(ExplorerAction::NewProject { then_move });
    }
}

/// Data source rows, each hidden while filtering unless it or something loaded under it
/// matches.
fn data_sources<'a>(
    ui: &mut Ui,
    tree: &SourcesView<'_>,
    entries: impl Iterator<Item = &'a DataSourceEntry>,
    filter: &TreeFilter<'_>,
    actions: &mut Vec<ExplorerAction>,
) {
    for entry in entries {
        match filter.source_visibility(entry) {
            Visibility::Hidden => {}
            Visibility::Shown => {
                data_source_tree(ui, entry, tree, false, &filter.below_match(), actions);
            }
            Visibility::LeadsToMatch => data_source_tree(ui, entry, tree, true, filter, actions),
        }
    }
}

fn toolbar(ui: &mut Ui, options: &mut ExplorerOptions, actions: &mut Vec<ExplorerAction>) {
    let palette = &Palette::for_visuals(ui.visuals());
    widgets::toolbar(ui, |ui| {
        let new = ui.menu_button(widgets::glyph(icons::PLUS).size(15.0), |ui| {
            new_data_source_menu(ui, actions);
            ui.separator();
            new_project_item(ui, None, actions);
        });
        widgets::name(
            new.response.on_hover_text("New data source or project"),
            WidgetType::Button,
            "New data source",
            true,
        );
        let (system_icon, system_color) = if options.show_system {
            (icons::EYE, palette.primary)
        } else {
            (icons::EYE_SLASH, palette.text_weak)
        };
        if IconButton::new(system_icon, "Show system schemas and tables")
            .color(system_color)
            .show(ui)
            .clicked()
        {
            options.show_system = !options.show_system;
        }
        ui.add_space(4.0);
        ui.label(widgets::glyph(icons::MAGNIFYING_GLASS).color(palette.text_weak));
        ui.add(
            TextEdit::singleline(&mut options.filter)
                .hint_text("Filter loaded nodes")
                .desired_width(f32::INFINITY),
        );
    });
}

/// One entry per driver, each with its icon. Also used by the Welcome tab.
pub fn new_data_source_menu(ui: &mut Ui, actions: &mut Vec<ExplorerAction>) {
    let palette = &Palette::for_visuals(ui.visuals());
    let logo_size = ui.text_style_height(&TextStyle::Button);
    for driver in DriverKind::ALL {
        let logo = icons::driver_logo_sized(driver, palette, logo_size);
        let response = widgets::name(
            ui.add(egui::Button::image_and_text(logo, driver.display_name())),
            WidgetType::Button,
            driver.display_name(),
            true,
        );
        if response.clicked() {
            actions.push(ExplorerAction::New(driver));
        }
    }
}

fn data_source_tree(
    ui: &mut Ui,
    entry: &DataSourceEntry,
    tree: &SourcesView<'_>,
    forced_open: bool,
    filter: &TreeFilter<'_>,
    actions: &mut Vec<ExplorerAction>,
) {
    let id = entry.data_source.id;
    let root = CatalogPath::root();
    let expanded = forced_open || entry.catalog.is_expanded(&root);
    let palette = &Palette::for_visuals(ui.visuals());
    let row_background = ui.painter().add(Shape::Noop);
    let row = ui.horizontal(|ui| {
        if expander(ui, expanded, &entry.data_source.name).clicked() {
            actions.push(toggle_expansion(id, &root, expanded));
        }
        let logo_size = ui.text_style_height(&TextStyle::Body);
        ui.add(icons::driver_logo_sized(
            entry.data_source.config.driver(),
            palette,
            logo_size,
        ));
        let name = ui.add(
            Label::new(&entry.data_source.name)
                .selectable(false)
                .sense(Sense::click()),
        );
        status_dot(ui, &entry.session, palette);
        name
    });
    let selected = tree.selected == Some(&(id, root.clone()));
    let fill = if selected {
        Some(ui.visuals().selection.bg_fill)
    } else {
        entry
            .data_source
            .color
            .map(|color| palette.tint(palette.source(color), SOURCE_ROW_TINT))
    };
    if let Some(fill) = fill {
        paint_row(ui, row_background, row.response.rect, fill);
    }
    let response = row.inner.on_hover_text(tooltip(entry));

    if response.clicked() {
        actions.push(ExplorerAction::Select(id, root.clone()));
    }
    if response.double_clicked() {
        actions.push(toggle_expansion(id, &root, expanded));
    }
    response.context_menu(|ui| data_source_menu(ui, entry, tree, actions));
    if expanded {
        let language = QueryLanguage::from(entry.data_source.config.driver());
        let tree = TreeView {
            id,
            catalog: &entry.catalog,
            quoting: language.quoting(),
            reads_rows: language.dialect().is_some(),
            // The object helpers write SQL, and not CQL's DDL (`Dialect::writes_ddl`).
            capabilities: language
                .dialect()
                .filter(|dialect| dialect.writes_ddl())
                .map(Dialect::capabilities),
            own_database: entry.data_source.config.database(),
            driver: entry.data_source.config.driver(),
            actions: tree.actions,
            selected: tree
                .selected
                .filter(|(selected, _)| *selected == id)
                .map(|(_, path)| path),
        };
        ui.indent(Id::new(id), |ui| {
            children(ui, &tree, &root, filter, actions)
        });
    }
}

fn data_source_menu(
    ui: &mut Ui,
    entry: &DataSourceEntry,
    tree: &SourcesView<'_>,
    actions: &mut Vec<ExplorerAction>,
) {
    let id = entry.data_source.id;
    let connected = matches!(
        entry.session,
        SessionState::Connecting | SessionState::Connected(_)
    );
    if connected {
        if widgets::menu_item(ui, icons::PLUGS, "Disconnect").clicked() {
            actions.push(ExplorerAction::Disconnect(id));
        }
        if widgets::menu_item(ui, icons::ARROWS_CLOCKWISE, "Refresh").clicked() {
            actions.push(ExplorerAction::Refresh(id, CatalogPath::root()));
        }
    } else if widgets::menu_item(ui, icons::PLUGS_CONNECTED, "Connect").clicked() {
        actions.push(ExplorerAction::Connect(id));
    }
    if widgets::menu_item(ui, icons::TERMINAL_WINDOW, "New query console").clicked() {
        actions.push(ExplorerAction::NewConsole(id));
    }
    saved_consoles_menu(ui, id, tree.saved_consoles, actions);
    saved_diagrams_menu(ui, id, tree.saved_diagrams, actions);
    let root = CatalogPath::root();
    let mut new = Vec::new();
    if let Some(capabilities) = QueryLanguage::from(entry.data_source.config.driver())
        .dialect()
        .filter(|dialect| dialect.writes_ddl())
        .map(Dialect::capabilities)
    {
        if capabilities.schema_creation == SchemaCreation::CreateDatabase {
            new.push((
                icons::TREE_STRUCTURE,
                "Schema…",
                Helper::NewSchema(root.clone()),
            ));
        }
        new.extend(new_roles(&root, capabilities));
    }
    if !new.is_empty() {
        ui.separator();
        let label = widgets::icon_text(ui, icons::PLUS, Color32::PLACEHOLDER, "New");
        let menu = ui.menu_button(label, |ui| helper_items(ui, id, new, actions));
        widgets::name(menu.response, WidgetType::Button, "New", true);
    }
    ui.separator();
    if widgets::menu_item(ui, icons::PENCIL_SIMPLE, "Edit…").clicked() {
        actions.push(ExplorerAction::Edit(id));
    }
    move_to_project_menu(ui, entry, tree.projects, actions);
    if widgets::menu_item(ui, icons::TRASH, "Delete…").clicked() {
        actions.push(ExplorerAction::Delete(id));
    }
    let driver = entry.data_source.config.driver();
    custom_action_items(
        ui,
        tree.actions,
        ActionTarget::DataSource,
        driver,
        |action| {
            actions.push(ExplorerAction::RunAction {
                data_source: id,
                path: CatalogPath::root(),
                action,
            });
        },
    );
}

/// The custom actions for what was right-clicked, after a separator, if there are any.
fn custom_action_items(
    ui: &mut Ui,
    all: &[CustomAction],
    target: ActionTarget,
    driver: DriverKind,
    mut chosen: impl FnMut(ActionId),
) {
    let mut offered = custom_actions::offered(all, target, driver).peekable();
    if offered.peek().is_some() {
        ui.separator();
    }
    for action in offered {
        if widgets::menu_item(ui, icons::MAGIC_WAND, &action.name).clicked() {
            chosen(action.id.clone());
        }
    }
}

/// Fills a row's background, behind what was drawn in it.
fn paint_row(ui: &Ui, background: egui::layers::ShapeIdx, row: Rect, fill: Color32) {
    let rect = Rect::from_x_y_ranges(ui.max_rect().x_range(), row.y_range());
    ui.painter().set(
        background,
        Shape::rect_filled(rect.expand2(vec2(0.0, 1.0)), CornerRadius::same(4), fill),
    );
}

/// Every project to move the data source to, then No project, then a new one. Its current
/// group is shown checked and does nothing.
fn move_to_project_menu(
    ui: &mut Ui,
    entry: &DataSourceEntry,
    projects: &[Project],
    actions: &mut Vec<ExplorerAction>,
) {
    let id = entry.data_source.id;
    let current = entry
        .data_source
        .project
        .filter(|current| projects.iter().any(|project| project.id == *current));
    let label = widgets::icon_text(
        ui,
        icons::ARROW_SQUARE_RIGHT,
        Color32::PLACEHOLDER,
        "Move to project",
    );
    let menu = ui.menu_button(label, |ui| {
        for project in projects {
            let here = current == Some(project.id);
            if ui.radio(here, &project.name).clicked() && !here {
                actions.push(ExplorerAction::MoveToProject(id, Some(project.id)));
            }
        }
        if ui.radio(current.is_none(), "No project").clicked() && current.is_some() {
            actions.push(ExplorerAction::MoveToProject(id, None));
        }
        ui.separator();
        new_project_item(ui, Some(id), actions);
    });
    widgets::name(menu.response, WidgetType::Button, "Move to project", true);
}

/// One data source's loaded catalog, as the tree functions need it.
struct TreeView<'a> {
    id: DataSourceId,
    catalog: &'a CatalogTree,
    /// For the names the context menu copies.
    quoting: Quoting,
    /// Whether the data source speaks SQL or CQL, which **Open data** and **Export data…**
    /// write.
    reads_rows: bool,
    /// Which object helpers the context menu offers; `None` when the data source doesn't speak
    /// SQL whose DDL they write.
    capabilities: Option<&'static Capabilities>,
    /// The database the data source connects to, where its tree lists several. The helpers'
    /// statements run on that connection, so they are offered only inside it.
    own_database: Option<&'a str>,
    driver: DriverKind,
    actions: &'a [CustomAction],
    /// The selected node, if it is in this data source.
    selected: Option<&'a CatalogPath>,
}

impl TreeView<'_> {
    /// Whether the helpers' statements would reach the object at `path`.
    fn can_change(&self, path: &CatalogPath) -> bool {
        match path.name_of(NodeKind::Database) {
            Some(database) => self.own_database == Some(database),
            None => true,
        }
    }
}

/// Which loaded nodes to draw.
#[derive(Clone)]
struct TreeFilter<'a> {
    /// Lowercase filter text; `None` shows everything.
    needle: Option<&'a str>,
    show_system: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visibility {
    Hidden,
    /// Drawn as usual: there is no filter, or the node itself matches it.
    Shown,
    /// Drawn only because something loaded below it matches; forced open to reveal it.
    LeadsToMatch,
}

impl TreeFilter<'_> {
    fn matches(&self, label: &str, needle: &str) -> bool {
        label.to_lowercase().contains(needle)
    }

    /// A data source is shown, with everything under it, when its name matches; otherwise
    /// only for what matches in its loaded catalog.
    fn source_visibility(&self, entry: &DataSourceEntry) -> Visibility {
        let Some(needle) = self.needle else {
            return Visibility::Shown;
        };
        if self.matches(&entry.data_source.name, needle) {
            Visibility::Shown
        } else if self.has_matching_descendant(&entry.catalog, &CatalogPath::root()) {
            Visibility::LeadsToMatch
        } else {
            Visibility::Hidden
        }
    }

    fn visibility(&self, catalog: &CatalogTree, node: &CatalogNode) -> Visibility {
        if node.system && !self.show_system {
            return Visibility::Hidden;
        }
        let Some(needle) = self.needle else {
            return Visibility::Shown;
        };
        if self.matches(&node.label, needle) {
            Visibility::Shown
        } else if self.has_matching_descendant(catalog, &node.path) {
            Visibility::LeadsToMatch
        } else {
            Visibility::Hidden
        }
    }

    fn has_matching_descendant(&self, catalog: &CatalogTree, path: &CatalogPath) -> bool {
        let Some(Children::Loaded(nodes)) = catalog.children(path) else {
            return false;
        };
        nodes
            .iter()
            .any(|node| self.visibility(catalog, node) != Visibility::Hidden)
    }

    /// Below a node that matched, everything is shown: the match is what the user looked for.
    fn below_match(&self) -> Self {
        Self {
            needle: None,
            show_system: self.show_system,
        }
    }
}

fn children(
    ui: &mut Ui,
    tree: &TreeView<'_>,
    path: &CatalogPath,
    filter: &TreeFilter<'_>,
    actions: &mut Vec<ExplorerAction>,
) {
    match tree.catalog.children(path) {
        None => {}
        Some(Children::Loading) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Loading…");
            });
        }
        Some(Children::Failed(error)) => {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        Some(Children::Loaded(nodes)) if nodes.is_empty() => {
            ui.weak("empty");
        }
        Some(Children::Loaded(nodes)) => {
            for node in nodes {
                match filter.visibility(tree.catalog, node) {
                    Visibility::Hidden => {}
                    Visibility::Shown => {
                        node_row(ui, tree, node, false, &filter.below_match(), actions);
                    }
                    Visibility::LeadsToMatch => node_row(ui, tree, node, true, filter, actions),
                }
            }
            level_end(ui, tree, path, actions);
        }
    }
}

/// **Load more…** after the loaded children of a partial level, or a spinner while the next
/// page loads.
fn level_end(
    ui: &mut Ui,
    tree: &TreeView<'_>,
    path: &CatalogPath,
    actions: &mut Vec<ExplorerAction>,
) {
    match tree.catalog.level_end(path) {
        LevelEnd::Complete => {}
        LevelEnd::LoadingMore => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Loading more…");
            });
        }
        LevelEnd::More(error) => {
            if let Some(error) = error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if ui.link("Load more…").clicked() {
                actions.push(ExplorerAction::LoadMore(tree.id, path.clone()));
            }
        }
    }
}

fn node_row(
    ui: &mut Ui,
    tree: &TreeView<'_>,
    node: &CatalogNode,
    forced_open: bool,
    filter: &TreeFilter<'_>,
    actions: &mut Vec<ExplorerAction>,
) {
    let expanded = forced_open || tree.catalog.is_expanded(&node.path);
    let row_background = ui.painter().add(Shape::Noop);
    let row = ui.horizontal(|ui| {
        if node.has_children {
            if expander(ui, expanded, &node.label).clicked() {
                actions.push(toggle_expansion(tree.id, &node.path, expanded));
            }
        } else {
            ui.add_space(ui.spacing().icon_width);
        }
        if let Some(kind) = node.kind() {
            ui.label(icons::node(kind).rich_text(&Palette::for_visuals(ui.visuals())));
        }
        let label = ui.add(
            Label::new(&node.label)
                .selectable(false)
                .sense(Sense::click()),
        );
        if let Some(detail) = &node.detail {
            ui.weak(detail);
        }
        if let Some(count) = folder_count(tree.catalog, node, filter.show_system) {
            widgets::count_badge(ui, count);
        }
        label
    });
    if tree.selected == Some(&node.path) {
        let fill = ui.visuals().selection.bg_fill;
        paint_row(ui, row_background, row.response.rect, fill);
    }
    let response = row.inner;

    if response.clicked() {
        actions.push(ExplorerAction::Select(tree.id, node.path.clone()));
    }
    if node.has_children && response.double_clicked() {
        actions.push(toggle_expansion(tree.id, &node.path, expanded));
    }
    response.context_menu(|ui| node_menu(ui, tree, node, actions));
    if expanded {
        ui.indent(Id::new((tree.id, &node.path)), |ui| {
            children(ui, tree, &node.path, filter, actions);
        });
    }
}

fn node_menu(
    ui: &mut Ui,
    tree: &TreeView<'_>,
    node: &CatalogNode,
    actions: &mut Vec<ExplorerAction>,
) {
    let id = tree.id;
    let holds_rows = tree.reads_rows
        && matches!(
            node.kind(),
            Some(NodeKind::Table | NodeKind::View | NodeKind::MaterializedView)
        );
    if holds_rows && widgets::menu_item(ui, icons::TABLE, "Open data").clicked() {
        actions.push(ExplorerAction::OpenData(id, node.path.clone()));
    }
    let is_read_whole = matches!(
        node.kind(),
        Some(NodeKind::Key(_) | NodeKind::Collection | NodeKind::SearchIndex)
    );
    if is_read_whole && widgets::menu_item(ui, icons::TABLE, "Open data").clicked() {
        actions.push(ExplorerAction::OpenData(id, node.path.clone()));
    }
    if holds_rows && widgets::menu_item(ui, icons::EXPORT, "Export data…").clicked() {
        actions.push(ExplorerAction::ExportData(id, node.path.clone()));
    }
    diagram_items(ui, tree, node, actions);
    if node.kind() == Some(NodeKind::Table)
        && tree.capabilities.is_some()
        && tree.can_change(&node.path)
        && widgets::menu_item(ui, icons::FILE_CSV, "Import CSV…").clicked()
    {
        actions.push(ExplorerAction::ImportCsv(id, node.path.clone()));
    }
    if let Some(name) = node.path.qualified_name(tree.quoting)
        && widgets::menu_item(ui, icons::COPY, "Copy name").clicked()
    {
        ui.ctx().copy_text(name);
    }
    if node.has_definition && widgets::menu_item(ui, icons::FILE_SQL, "Show definition").clicked() {
        actions.push(ExplorerAction::ShowDefinition {
            id,
            path: node.path.clone(),
            title: node.label.clone(),
        });
    }
    if node.has_children && widgets::menu_item(ui, icons::ARROWS_CLOCKWISE, "Refresh").clicked() {
        actions.push(ExplorerAction::Refresh(id, node.path.clone()));
    }
    if tree.can_change(&node.path) {
        helpers_menu(ui, tree, node, actions);
    }
    if let Some(target) = custom_actions::node_target(&node.path) {
        custom_action_items(ui, tree.actions, target, tree.driver, |action| {
            actions.push(ExplorerAction::RunAction {
                data_source: id,
                path: node.path.clone(),
                action,
            });
        });
    }
}

/// **Diagram…** on a schema and **Diagram with related tables** on a table, where the
/// dialect has foreign keys to draw.
fn diagram_items(
    ui: &mut Ui,
    tree: &TreeView<'_>,
    node: &CatalogNode,
    actions: &mut Vec<ExplorerAction>,
) {
    if !tree
        .capabilities
        .is_some_and(|capabilities| capabilities.foreign_keys)
    {
        return;
    }
    let path = &node.path;
    let (label, action) = match node.kind() {
        Some(NodeKind::Schema) => ("Diagram…", ExplorerAction::Diagram(tree.id, path.clone())),
        Some(NodeKind::Table) if path.name_of(NodeKind::Schema).is_some() => (
            "Diagram with related tables",
            ExplorerAction::DiagramRelated(tree.id, path.clone()),
        ),
        _ => return,
    };
    if widgets::menu_item(ui, icons::GRAPH, label).clicked() {
        actions.push(action);
    }
}

/// The object helpers for `node`, after the browsing items: a New submenu for what can be
/// created in it, then what can be done to it.
fn helpers_menu(
    ui: &mut Ui,
    tree: &TreeView<'_>,
    node: &CatalogNode,
    actions: &mut Vec<ExplorerAction>,
) {
    let path = &node.path;
    let Some(capabilities) = tree.capabilities else {
        return;
    };
    let mut new: Vec<(&str, &str, Helper)> = Vec::new();
    let mut object: Vec<(&str, &str, Helper)> = Vec::new();
    match node.kind() {
        Some(NodeKind::Database)
            if capabilities.schema_creation == SchemaCreation::CreateSchema =>
        {
            new.push((
                icons::TREE_STRUCTURE,
                "Schema…",
                Helper::NewSchema(path.clone()),
            ));
        }
        Some(NodeKind::Schema) => {
            new.push((icons::TABLE, "Table…", Helper::NewTable(path.clone())));
            new.extend(new_views(path, capabilities));
            if capabilities.schema_creation != SchemaCreation::Unsupported && !node.system {
                object.push((
                    icons::TRASH,
                    "Drop schema…",
                    Helper::DropSchema(path.clone()),
                ));
            }
        }
        Some(NodeKind::Folder(FolderKind::Tables)) => {
            new.push((icons::TABLE, "Table…", Helper::NewTable(path.clone())));
        }
        Some(NodeKind::Folder(FolderKind::Views)) => new.extend(new_views(path, capabilities)),
        Some(NodeKind::Folder(FolderKind::MaterializedViews))
            if capabilities.materialized_views =>
        {
            new.push((
                icons::STACK,
                "Materialized view…",
                Helper::NewMaterializedView(path.clone()),
            ));
        }
        Some(NodeKind::Folder(FolderKind::Indexes)) if capabilities.indexes && on_table(path) => {
            new.push((icons::LIST_DASHES, "Index…", Helper::NewIndex(path.clone())));
        }
        Some(NodeKind::Table) => {
            object.push((
                icons::PENCIL_SIMPLE,
                "Modify table…",
                Helper::ModifyTable(path.clone()),
            ));
            if capabilities.indexes {
                object.push((
                    icons::LIST_DASHES,
                    "New index…",
                    Helper::NewIndex(path.clone()),
                ));
            }
            if capabilities.changes_in_place(TableChange::RenameTable) {
                object.push((
                    icons::PENCIL_SIMPLE,
                    "Rename…",
                    Helper::RenameTable(path.clone()),
                ));
            }
            object.push((
                icons::ERASER,
                "Truncate…",
                Helper::TruncateTable(path.clone()),
            ));
            object.push((icons::TRASH, "Drop…", Helper::DropTable(path.clone())));
        }
        Some(NodeKind::Index)
            if capabilities.indexes && on_table(&path.parent().unwrap_or_default()) =>
        {
            object.push((icons::TRASH, "Drop index…", Helper::DropIndex(path.clone())));
        }
        Some(NodeKind::Folder(FolderKind::Roles)) => new.extend(new_roles(path, capabilities)),
        Some(NodeKind::Role) if capabilities.users == UserModel::Roles => {
            object.push((
                icons::PENCIL_SIMPLE,
                "Edit…",
                Helper::EditRole(path.clone()),
            ));
            object.push((
                icons::PENCIL_SIMPLE,
                "Rename…",
                Helper::RenameRole(path.clone()),
            ));
            object.push((icons::TRASH, "Drop…", Helper::DropRole(path.clone())));
        }
        Some(NodeKind::View) => object.extend(view_actions(path, capabilities)),
        Some(NodeKind::MaterializedView) if capabilities.materialized_views => {
            object.push((
                icons::ARROWS_CLOCKWISE,
                "Refresh…",
                Helper::RefreshView(path.clone()),
            ));
            object.extend(view_actions(path, capabilities));
        }
        _ => {}
    }
    let copies_ddl = match node.kind() {
        Some(NodeKind::Table) => node.has_definition || capabilities.rebuilds_table_ddl,
        Some(NodeKind::View | NodeKind::MaterializedView) => node.has_definition,
        Some(NodeKind::Role) => capabilities.users == UserModel::Roles,
        _ => false,
    };
    if copies_ddl && widgets::menu_item(ui, icons::COPY, "Copy DDL").clicked() {
        actions.push(ExplorerAction::CopyDdl(tree.id, path.clone()));
    }
    if new.is_empty() && object.is_empty() {
        return;
    }
    ui.separator();
    if !new.is_empty() {
        let label = widgets::icon_text(ui, icons::PLUS, Color32::PLACEHOLDER, "New");
        let menu = ui.menu_button(label, |ui| helper_items(ui, tree.id, new, actions));
        widgets::name(menu.response, WidgetType::Button, "New", true);
    }
    helper_items(ui, tree.id, object, actions);
}

/// New view, and New materialized view where the dialect has them, in the schema at `path`
/// or the schema whose views folder that is.
fn new_views(
    path: &CatalogPath,
    capabilities: &Capabilities,
) -> Vec<(&'static str, &'static str, Helper)> {
    let mut items = vec![(icons::EYE, "View…", Helper::NewView(path.clone()))];
    if capabilities.materialized_views {
        items.push((
            icons::STACK,
            "Materialized view…",
            Helper::NewMaterializedView(path.clone()),
        ));
    }
    items
}

/// New user and New role, on the data source or its roles folder at `path`, where the
/// dialect's users are roles.
fn new_roles(
    path: &CatalogPath,
    capabilities: &Capabilities,
) -> Vec<(&'static str, &'static str, Helper)> {
    match capabilities.users {
        UserModel::Roles => vec![
            (icons::USER, "User…", Helper::NewUser(path.clone())),
            (icons::USERS, "Role…", Helper::NewRole(path.clone())),
        ],
        UserModel::Unsupported => Vec::new(),
    }
}

/// What can be done to the view or materialized view at `path`.
fn view_actions(
    path: &CatalogPath,
    capabilities: &Capabilities,
) -> Vec<(&'static str, &'static str, Helper)> {
    let mut items = vec![(
        icons::PENCIL_SIMPLE,
        "Edit…",
        Helper::EditView(path.clone()),
    )];
    if capabilities.renames_views {
        items.push((
            icons::PENCIL_SIMPLE,
            "Rename…",
            Helper::RenameView(path.clone()),
        ));
    }
    items.push((icons::TRASH, "Drop…", Helper::DropView(path.clone())));
    items
}

/// Whether `folder` is a table's own, rather than a view's.
fn on_table(folder: &CatalogPath) -> bool {
    folder
        .parent()
        .and_then(|parent| parent.last().map(|segment| segment.kind))
        == Some(NodeKind::Table)
}

fn helper_items(
    ui: &mut Ui,
    id: DataSourceId,
    helpers: Vec<(&str, &str, Helper)>,
    actions: &mut Vec<ExplorerAction>,
) {
    for (glyph, label, helper) in helpers {
        if widgets::menu_item(ui, glyph, label).clicked() {
            actions.push(ExplorerAction::Helper(id, helper));
        }
    }
}

/// Lists the data source's closed console files, if there are any.
fn saved_consoles_menu(
    ui: &mut Ui,
    id: DataSourceId,
    saved_consoles: &SavedConsoles,
    actions: &mut Vec<ExplorerAction>,
) {
    if saved_consoles.names(id).next().is_none() {
        return;
    }
    ui.menu_button("Open saved console", |ui| {
        for name in saved_consoles.names(id) {
            if ui.button(format!("{name}.sql")).clicked() {
                actions.push(ExplorerAction::OpenSavedConsole(ConsoleKey {
                    data_source: id,
                    name: name.clone(),
                }));
            }
        }
    });
}

/// Lists the data source's saved diagrams, if it has any.
fn saved_diagrams_menu(
    ui: &mut Ui,
    id: DataSourceId,
    saved_diagrams: &SavedDiagrams,
    actions: &mut Vec<ExplorerAction>,
) {
    if saved_diagrams.of(id).next().is_none() {
        return;
    }
    let label = widgets::icon_text(ui, icons::GRAPH, Color32::PLACEHOLDER, "Diagrams");
    let menu = ui.menu_button(label, |ui| {
        for key in saved_diagrams.of(id) {
            if ui.button(key.name.as_str()).clicked() {
                actions.push(ExplorerAction::OpenDiagram(key.clone()));
            }
        }
    });
    widgets::name(menu.response, WidgetType::Button, "Diagrams", true);
}

fn toggle_expansion(id: DataSourceId, path: &CatalogPath, expanded: bool) -> ExplorerAction {
    if expanded {
        ExplorerAction::Collapse(id, path.clone())
    } else {
        ExplorerAction::Expand(id, path.clone())
    }
}

/// The disclosure triangle. Labelled "Expand x" / "Collapse x" for screen readers and tests.
pub(crate) fn expander(ui: &mut Ui, expanded: bool, label: &str) -> Response {
    let size = ui.spacing().icon_width;
    let (_, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let verb = if expanded { "Collapse" } else { "Expand" };
    response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{verb} {label}")));
    let caret = if expanded {
        icons::CARET_DOWN
    } else {
        icons::CARET_RIGHT
    };
    let color = if response.hovered() {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().text(
        response.rect.center(),
        egui::Align2::CENTER_CENTER,
        caret,
        crate::fonts::icon(size * 0.8),
        color,
    );
    response
}

/// The number of children a loaded folder shows, for the badge after its label.
fn folder_count(catalog: &CatalogTree, node: &CatalogNode, show_system: bool) -> Option<usize> {
    if !matches!(node.kind(), Some(NodeKind::Folder(_))) {
        return None;
    }
    let Some(Children::Loaded(children)) = catalog.children(&node.path) else {
        return None;
    };
    Some(
        children
            .iter()
            .filter(|child| show_system || !child.system)
            .count(),
    )
}

/// The connection state as a dot after the name. Disconnected data sources get none, so the
/// eye goes to the ones that are live or in trouble.
fn status_dot(ui: &mut Ui, session: &SessionState, palette: &Palette) {
    let size = vec2(
        STATUS_DOT_RADIUS * 2.0,
        ui.text_style_height(&TextStyle::Body),
    );
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if let Some(color) = status_color(session, palette) {
        ui.painter()
            .circle_filled(rect.center(), STATUS_DOT_RADIUS, color);
    }
}

fn status_color(session: &SessionState, palette: &Palette) -> Option<Color32> {
    match session {
        SessionState::Disconnected => None,
        SessionState::Connecting => Some(palette.warning),
        SessionState::Connected(_) => Some(palette.success),
        SessionState::Failed(_) | SessionState::Lost(_) => Some(palette.error),
    }
}

fn tooltip(entry: &DataSourceEntry) -> String {
    let config = &entry.data_source.config;
    let status = match &entry.session {
        SessionState::Disconnected => "Disconnected".to_owned(),
        SessionState::Connecting => "Connecting…".to_owned(),
        SessionState::Connected(server) => format!("Connected to {server}"),
        SessionState::Failed(error) => format!("Connection failed: {error}"),
        SessionState::Lost(reason) => {
            format!("Disconnected: {reason}\nReconnects when you next browse it.")
        }
    };
    format!("{} · {config}\n{status}", config.driver().display_name())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(parent: &CatalogPath, name: &str, system: bool) -> CatalogNode {
        CatalogNode {
            path: parent.child(NodeKind::Schema, name),
            label: name.to_owned(),
            detail: None,
            has_children: true,
            has_definition: false,
            system,
        }
    }

    #[test]
    fn filter_shows_matches_their_ancestors_and_respects_system_objects() {
        let root = CatalogPath::root();
        let public = node(&root, "public", false);
        let users = node(&public.path, "users", false);
        let pg_catalog = node(&root, "pg_catalog", true);
        let mut catalog = CatalogTree::default();
        assert!(catalog.expand(&root));
        catalog.loaded_whole(&root, Ok(vec![public.clone(), pg_catalog.clone()]));
        assert!(catalog.expand(&public.path));
        catalog.loaded_whole(&public.path, Ok(vec![users.clone()]));

        let cases = [
            (None, false, &public, Visibility::Shown),
            (None, false, &pg_catalog, Visibility::Hidden),
            (None, true, &pg_catalog, Visibility::Shown),
            (Some("user"), false, &public, Visibility::LeadsToMatch),
            (Some("user"), false, &users, Visibility::Shown),
            (Some("pub"), false, &public, Visibility::Shown),
            (Some("orders"), false, &public, Visibility::Hidden),
        ];
        for (needle, show_system, node, expected) in cases {
            let filter = TreeFilter {
                needle,
                show_system,
            };
            assert_eq!(
                filter.visibility(&catalog, node),
                expected,
                "{needle:?} {show_system} {}",
                node.label
            );
        }
    }
}
