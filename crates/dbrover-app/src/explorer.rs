//! The connection explorer panel: data sources and their catalogs as a lazily loaded tree.
//! It only reports what the user asked for; the app turns that into engine commands.

use std::collections::BTreeSet;

use dbrover_core::{
    AccountKind, CatalogNode, CatalogPath, DataSourceId, DriverKind, DumpEngine, FolderKind,
    ManagedService, NodeKind, ProjectId, Quoting, SafetyLevel, ServiceVendor,
};
use dbrover_engine::SessionState;
use dbrover_language::QueryLanguage;
use dbrover_sql::{Capabilities, SchemaCreation, SearchSupport, TableChange, UserModel};
use dbrover_store::{ActionId, ActionTarget, ConsoleKey, CustomAction, DiagramKey, Project};
use egui::{
    Color32, CornerRadius, DragAndDrop, Id, Label, LayerId, Order, Rangef, Rect, Response,
    RichText, ScrollArea, Sense, Shape, Stroke, StrokeKind, TextEdit, TextStyle, Ui, WidgetInfo,
    WidgetType, epaint::RectShape, vec2,
};

use crate::catalog_tree::{CatalogTree, Children, LevelEnd, Search};
use crate::consoles::SavedConsoles;
use crate::custom_actions;
use crate::diagram::SavedDiagrams;
use crate::icons;
use crate::query_folders::{self, QueryFolderAction, QueryFolders, SectionState};
use crate::reports::{ReportAction, Reports};
use crate::state::{DataSourceEntry, DataSources};
use crate::theme::Palette;
use crate::widgets::{self, IconButton};

const STATUS_DOT_RADIUS: f32 = 3.5;
/// How strongly a data source's colour tints its row.
const SOURCE_ROW_TINT: f32 = 0.16;
/// How strongly the accent fills a project header the pointer is about to drop onto (D9).
const DROP_TARGET_TINT: f32 = 0.22;
/// How close to the explorer's top or bottom edge a dragged data source scrolls the list, so a
/// project folded out of sight can still be dropped on.
const DRAG_SCROLL_BAND: f32 = 24.0;
/// Points scrolled per frame with the pointer at the very edge of the band; less further in.
const DRAG_SCROLL_SPEED: f32 = 8.0;
/// How far below and right of the cursor the dragged data source's name is drawn, so the cursor
/// itself stays visible over the header it is aimed at.
const GHOST_CURSOR_GAP: f32 = 8.0;

/// A data source being dragged towards a project header (D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DraggedSource {
    id: DataSourceId,
    /// The group it is shown in now, so dropping it there again can do nothing. A project
    /// `projects.toml` doesn't know counts as no project, as the grouping itself does.
    from: Option<ProjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplorerAction {
    /// A new data source, in `project` when a project header's menu asked for it (D9).
    New {
        driver: DriverKind,
        project: Option<ProjectId>,
    },
    /// A new data source of a managed service, its preset filled in (C1f), in `project` as
    /// [`Self::New`] is.
    NewService {
        service: ManagedService,
        project: Option<ProjectId>,
    },
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
    /// The entry an MCP client's configuration needs to serve the project.
    CopyMcpConfiguration(ProjectId),
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
    /// Server activity: the data source's sessions, locks, Cancel and Kill, in a tab (W15).
    ServerActivity(DataSourceId),
    /// Search data…: the Search data tab, searching the tables under the path (W16).
    SearchData(DataSourceId, CatalogPath),
    /// Dump…: the Dump dialog, dumping the database, schema or table at the path (W17).
    Dump(DataSourceId, CatalogPath),
    /// Restore…: the Restore dialog, into the database named, or a new one (W17).
    Restore(DataSourceId, Option<String>),
    /// Compare schema…: a compare tab with the schema at the path on its left (W13).
    CompareSchema(DataSourceId, CatalogPath),
    /// Save schema snapshot…: the schema at the path written to a file the user picks (VC3).
    SaveSchemaSnapshot(DataSourceId, CatalogPath),
    /// Reopens a saved diagram, or brings its tab to the front.
    OpenDiagram(DiagramKey),
    /// Something asked of the **Reports** section.
    Report(ReportAction),
    /// Something asked of the **Query folders** section.
    QueryFolder(QueryFolderAction),
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
    /// MySQL's and MariaDB's accounts (W18).
    Account(AccountHelper),
    /// SQL Server's logins, users and roles (W18).
    SqlServer(SqlServerHelper),
}

/// The helpers for SQL Server's logins, server roles, database users and database roles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlServerHelper {
    /// On the data source (the root) or its logins folder.
    NewLogin(CatalogPath),
    /// On the server roles folder.
    NewServerRole(CatalogPath),
    /// On a database's users folder.
    NewUser(CatalogPath),
    /// On a database's roles folder.
    NewDatabaseRole(CatalogPath),
    /// On a login, a server role, a database user or a database role, as the next two.
    Edit(CatalogPath),
    Rename(CatalogPath),
    Drop(CatalogPath),
    /// On a login: Disable, or Enable a disabled one.
    SetEnabled(CatalogPath),
}

/// The helpers for MySQL's and MariaDB's accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountHelper {
    /// On the data source (the root), or its users or roles folder.
    New(CatalogPath, AccountKind),
    /// On a user or role, as the next ones.
    Edit(CatalogPath),
    Rename(CatalogPath),
    /// On a user only, as the next one.
    Lock(CatalogPath),
    ExpirePassword(CatalogPath),
    Drop(CatalogPath),
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
    /// What is folded and opened in **Query folders**.
    pub query_folders: SectionState,
}

/// A group of data sources in the explorer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    Project(ProjectId),
    /// Data sources in no project, or in one `projects.toml` doesn't know.
    NoProject,
    /// Not data sources: the **Reports** under them.
    Reports,
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
    /// Listed under the data sources, when there is a reports folder.
    pub reports: &'a Reports,
    /// Listed last, always, so a first folder can be added from the section's menu.
    pub query_folders: &'a QueryFolders,
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
            query_folders_section(
                ui,
                view.query_folders,
                None,
                &mut options.query_folders,
                actions,
            );
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
            let query_folders = &mut options.query_folders;
            ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                if view.projects.is_empty() {
                    data_sources(ui, &tree, entries.iter(), &filter, actions);
                } else {
                    for (group, members) in groups(view.projects, entries) {
                        group_tree(ui, &tree, group, &members, &filter, collapsed, actions);
                    }
                }
                if view.reports.is_set_up() {
                    reports_section(ui, view.reports, &filter, collapsed, actions);
                }
                query_folders_section(
                    ui,
                    view.query_folders,
                    filter.needle,
                    query_folders,
                    actions,
                );
                auto_scroll_while_dragging(ui);
            });
        }
    }
}

fn query_folders_section(
    ui: &mut Ui,
    folders: &QueryFolders,
    needle: Option<&str>,
    state: &mut SectionState,
    actions: &mut Vec<ExplorerAction>,
) {
    let mut asked = Vec::new();
    query_folders::section(ui, folders, needle, state, &mut asked);
    actions.extend(asked.into_iter().map(ExplorerAction::QueryFolder));
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

    /// The project a data source joins by landing in this group: `None` under **No project**,
    /// which is also what a data source in no project has.
    fn project(self) -> Option<ProjectId> {
        match self {
            Self::Project(project) => Some(project.id),
            Self::NoProject => None,
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
    widgets::context_menu(&response.name, |ui| group_menu(ui, header, actions));
    header_drop_target(ui, header, &response, actions);
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
    /// The row's own rect, which the drop target widens to the whole panel (D9).
    row: Rect,
    /// Reserved behind the row, for the fill a valid drop target gets.
    background: egui::layers::ShapeIdx,
}

fn group_header_row(
    ui: &mut Ui,
    header: GroupHeader<'_>,
    count: usize,
    expanded: bool,
) -> GroupHeaderResponse {
    let palette = &Palette::for_visuals(ui.visuals());
    let background = ui.painter().add(Shape::Noop);
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
    GroupHeaderResponse {
        name,
        toggled,
        row: row.response.rect,
        background,
    }
}

/// A header takes a data source dragged onto it, anywhere along the row (D9).
///
/// Dropping a data source on the group it is already in does nothing, and shows no highlight:
/// it is a move to where it already is, which is what **Move to project ›** says by showing the
/// current project checked and doing nothing when it is picked. Anything that isn't a header
/// never claims the payload, so a release over a data source, a table or another section moves
/// nothing; egui drops the payload itself when the button comes up.
fn header_drop_target(
    ui: &Ui,
    header: GroupHeader<'_>,
    row: &GroupHeaderResponse,
    actions: &mut Vec<ExplorerAction>,
) {
    let rect = Rect::from_x_y_ranges(ui.max_rect().x_range(), row.row.y_range());
    let id = Id::new(("explorer_drop", header.group()));
    let response = ui.interact(rect, id, Sense::hover());
    let Some(dragged) = response.dnd_hover_payload::<DraggedSource>() else {
        return;
    };
    let target = header.project();
    if dragged.from == target {
        return;
    }
    let palette = &Palette::for_visuals(ui.visuals());
    ui.painter().set(
        row.background,
        RectShape::new(
            rect.expand2(vec2(0.0, 1.0)),
            CornerRadius::same(4),
            palette.tint(palette.primary, DROP_TARGET_TINT),
            Stroke::new(1.0, palette.primary),
            StrokeKind::Inside,
        ),
    );
    if response.dnd_release_payload::<DraggedSource>().is_some() {
        actions.push(ExplorerAction::MoveToProject(dragged.id, target));
    }
}

/// How far to scroll the explorer with a data source held at `y`, in a view spanning `view`:
/// positive scrolls towards the top of the list, as a wheel's delta does.
///
/// Zero away from the edges, growing to [`DRAG_SCROLL_SPEED`] at the very edge, so the list
/// creeps when the pointer is just inside the band and runs when it is held against the edge.
fn drag_scroll_delta(view: Rangef, y: f32) -> f32 {
    let from_top = y - view.min;
    let from_bottom = view.max - y;
    if from_top < DRAG_SCROLL_BAND {
        (DRAG_SCROLL_BAND - from_top.max(0.0)) / DRAG_SCROLL_BAND * DRAG_SCROLL_SPEED
    } else if from_bottom < DRAG_SCROLL_BAND {
        -(DRAG_SCROLL_BAND - from_bottom.max(0.0)) / DRAG_SCROLL_BAND * DRAG_SCROLL_SPEED
    } else {
        0.0
    }
}

/// Scrolls the explorer while a data source is dragged against its top or bottom edge.
///
/// Only while one is in flight, so a list that fits the panel never moves on its own. The frame
/// asks for a repaint, so the list keeps moving while the pointer is held still.
fn auto_scroll_while_dragging(ui: &Ui) {
    if !DragAndDrop::has_payload_of_type::<DraggedSource>(ui.ctx()) {
        return;
    }
    let view = ui.clip_rect();
    let Some(pointer) = ui
        .ctx()
        .pointer_interact_pos()
        .filter(|at| view.contains(*at))
    else {
        return;
    };
    let delta = drag_scroll_delta(view.y_range(), pointer.y);
    if delta != 0.0 {
        ui.scroll_with_delta(vec2(0.0, delta));
        ui.ctx().request_repaint();
    }
}

/// **New data source ›** creates in the group whose header this menu belongs to (D9), so the
/// project needs no picking; the rest of the menu is about the project itself.
fn group_menu(ui: &mut Ui, header: GroupHeader<'_>, actions: &mut Vec<ExplorerAction>) {
    new_data_source_item(ui, header, actions);
    ui.separator();
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
            actions.push(ExplorerAction::CopyMcpConfiguration(project.id));
        }
        if widgets::menu_item(ui, icons::TRASH, "Delete…").clicked() {
            actions.push(ExplorerAction::DeleteProject(project.id));
        }
        ui.separator();
    }
    new_project_item(ui, None, actions);
}

/// The submenu that creates a data source in the group whose menu this is.
///
/// It reads **New data source ›**, but its accessible name says which group it creates in: the
/// toolbar's `+` and the Welcome tab's card are called **New data source** too, and the group is
/// what tells them apart for a screen reader, and for the UI tests.
fn new_data_source_item(ui: &mut Ui, header: GroupHeader<'_>, actions: &mut Vec<ExplorerAction>) {
    let label = widgets::icon_text(ui, icons::PLUS, Color32::PLACEHOLDER, "New data source");
    let menu = widgets::menu_button(ui, label, |ui| {
        new_data_source_menu(ui, header.project(), actions);
    });
    let name = format!("New data source in {}", header.label());
    widgets::name(menu.response, WidgetType::Button, &name, true);
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
        let new = widgets::menu_button(ui, widgets::glyph(icons::PLUS).size(15.0), |ui| {
            new_data_source_menu(ui, None, actions);
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
                .hint_text("Filter, or search the server")
                .desired_width(f32::INFINITY),
        );
    });
}

/// One entry per engine people run themselves, each with its icon, then a submenu per cloud
/// with the engines only that cloud runs (DynamoDB, Athena, BigQuery) and its managed services.
/// Also used by the Welcome tab.
///
/// `project` is the project the new data source is created in: the one whose header's menu this
/// is (D9), and `None` from the toolbar's `+` and the Welcome tab, where no project is in sight.
///
/// A cloud's own engines are in its submenu alone: listed in both places, the menu grew past
/// what a 900-point window shows with the Azure submenu open beside it (C4c).
pub fn new_data_source_menu(
    ui: &mut Ui,
    project: Option<ProjectId>,
    actions: &mut Vec<ExplorerAction>,
) {
    let logo_size = ui.text_style_height(&TextStyle::Button);
    let run_by_anyone = DriverKind::ALL
        .into_iter()
        .filter(|driver| ServiceVendor::running_only(*driver).is_none());
    for driver in run_by_anyone {
        new_driver_item(ui, driver, project, logo_size, actions);
    }
    ui.separator();
    for vendor in ServiceVendor::ALL {
        widgets::menu_button(ui, vendor.display_name(), |ui| {
            cloud_service_items(ui, vendor, project, actions);
        });
    }
}

/// The menu's entry for a new data source of `driver`, with its logo.
fn new_driver_item(
    ui: &mut Ui,
    driver: DriverKind,
    project: Option<ProjectId>,
    logo_size: f32,
    actions: &mut Vec<ExplorerAction>,
) {
    let palette = &Palette::for_visuals(ui.visuals());
    let logo = icons::driver_logo_sized(driver, palette, logo_size);
    let response = widgets::name(
        ui.add(egui::Button::image_and_text(logo, driver.display_name())),
        WidgetType::Button,
        driver.display_name(),
        true,
    );
    if response.clicked() {
        actions.push(ExplorerAction::New { driver, project });
    }
}

/// The engines the vendor's submenu offers (DynamoDB and Athena under AWS, BigQuery under
/// Google Cloud, Snowflake under the other cloud services), then the managed services that run
/// in `vendor`'s cloud (C1f), each with its engine's icon. A submenu per cloud rather than one
/// for them all: fifteen services and their headings made a submenu taller than a laptop's
/// window.
fn cloud_service_items(
    ui: &mut Ui,
    vendor: ServiceVendor,
    project: Option<ProjectId>,
    actions: &mut Vec<ExplorerAction>,
) {
    let palette = &Palette::for_visuals(ui.visuals());
    let logo_size = ui.text_style_height(&TextStyle::Button);
    if !vendor.engines().is_empty() {
        for &driver in vendor.engines() {
            new_driver_item(ui, driver, project, logo_size, actions);
        }
        ui.separator();
    }
    for service in vendor.services() {
        let logo = icons::driver_logo_sized(service.driver(), palette, logo_size);
        let response = widgets::name(
            ui.add(egui::Button::image_and_text(logo, service.display_name())),
            WidgetType::Button,
            service.display_name(),
            true,
        );
        if response.clicked() {
            actions.push(ExplorerAction::NewService { service, project });
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
        let name = drag_source_name(ui, entry, tree.projects);
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
    widgets::context_menu(&response, |ui| data_source_menu(ui, entry, tree, actions));
    if expanded {
        let language = QueryLanguage::from(entry.data_source.config.driver());
        let tree = TreeView {
            id,
            catalog: &entry.catalog,
            quoting: language.quoting(),
            reads_rows: language.dialect().is_some(),
            capabilities: helper_capabilities(entry),
            own_database: entry.data_source.config.database(),
            driver: entry.data_source.config.driver(),
            language,
            data_search: data_search(entry),
            dumps: entry.dump_engine(),
            read_only: entry.data_source.safety == SafetyLevel::ReadOnly,
            actions: tree.actions,
            selected: tree
                .selected
                .filter(|(selected, _)| *selected == id)
                .map(|(_, path)| path),
        };
        ui.indent(Id::new(id), |ui| {
            children(ui, &tree, &root, filter, actions);
            found_on_server(ui, &tree, filter, actions);
        });
    }
}

/// A data source's name, which is also the handle that drags it onto a project header (D9).
///
/// The name is one widget that senses both, rather than a label inside `Ui::dnd_drag_source`:
/// that helper lays a drag-sense widget over the body, and the row's secondary click went to it
/// instead of the label, so every data source's context menu stopped opening. Sensing click and
/// drag together keeps the row's click, double-click and context menu, and egui's own threshold
/// decides which gesture a press turns into.
///
/// The ghost that follows the cursor is painted here for the same reason, in the tooltip layer
/// above the panel. The row keeps its name, expander, logo and status dot while it is dragged,
/// so a drag that is let go over nothing leaves no trace.
fn drag_source_name(ui: &mut Ui, entry: &DataSourceEntry, projects: &[Project]) -> Response {
    let id = entry.data_source.id;
    let name = &entry.data_source.name;
    let response = ui.add(
        Label::new(name)
            .selectable(false)
            .sense(Sense::click_and_drag()),
    );
    response.dnd_set_drag_payload(DraggedSource {
        id,
        from: shown_project(&entry.data_source.project, projects),
    });
    if response.dragged() {
        drag_ghost(ui, id, name);
    }
    response
}

/// The dragged data source's name at the cursor, so it is clear what is in flight.
fn drag_ghost(ui: &Ui, id: DataSourceId, name: &str) {
    let Some(at) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    let palette = &Palette::for_visuals(ui.visuals());
    let layer = LayerId::new(Order::Tooltip, Id::new(("explorer_drag_ghost", id)));
    let painter = ui.ctx().layer_painter(layer);
    let galley = painter.layout_no_wrap(
        name.to_owned(),
        TextStyle::Body.resolve(ui.style()),
        palette.text,
    );
    let margin = vec2(6.0, 3.0);
    let top_left = at + vec2(GHOST_CURSOR_GAP, GHOST_CURSOR_GAP);
    let rect = Rect::from_min_size(top_left, galley.size() + margin * 2.0);
    painter.add(RectShape::new(
        rect,
        CornerRadius::same(4),
        palette.tint(palette.primary, DROP_TARGET_TINT),
        Stroke::new(1.0, palette.primary),
        StrokeKind::Inside,
    ));
    painter.galley(top_left + margin, galley, palette.text);
}

/// The project a data source is grouped under: its own, unless `projects.toml` doesn't know it,
/// which puts it under **No project**. Mirrors what [`groups`] does.
fn shown_project(project: &Option<ProjectId>, projects: &[Project]) -> Option<ProjectId> {
    project.filter(|id| projects.iter().any(|project| project.id == *id))
}

/// What the object helpers may write for `entry`: its dialect's DDL, unless it writes none
/// (CQL's) or its server is a service whose DDL differs (C1).
/// Whether the data source's data can be searched (W16), and whether it asks first.
fn data_search(entry: &DataSourceEntry) -> SearchSupport {
    QueryLanguage::from(entry.data_source.config.driver())
        .dialect()
        .map_or(SearchSupport::Unsupported, |dialect| {
            dialect.data_search(entry.service())
        })
}

fn helper_capabilities(entry: &DataSourceEntry) -> Option<&'static Capabilities> {
    QueryLanguage::from(entry.data_source.config.driver())
        .dialect()?
        .capabilities_on(entry.service())
}

/// **Dump…** and **Restore…** on a connected data source of an engine DbRover dumps (W17);
/// Restore disabled, with the reason, on a read-only one.
fn dump_items(ui: &mut Ui, entry: &DataSourceEntry, actions: &mut Vec<ExplorerAction>) {
    if entry.dump_engine().is_none() {
        return;
    }
    let id = entry.data_source.id;
    if widgets::menu_item(ui, icons::DOWNLOAD_SIMPLE, "Dump\u{2026}").clicked() {
        actions.push(ExplorerAction::Dump(id, CatalogPath::root()));
    }
    let writable = entry.data_source.safety != SafetyLevel::ReadOnly;
    let restore = ui
        .add_enabled_ui(writable, |ui| {
            widgets::menu_item(ui, icons::UPLOAD_SIMPLE, "Restore\u{2026}")
        })
        .inner
        .on_disabled_hover_text(READ_ONLY_RESTORE);
    if restore.clicked() {
        actions.push(ExplorerAction::Restore(id, None));
    }
}

/// Why a read-only data source's **Restore…** is disabled.
const READ_ONLY_RESTORE: &str = "This data source is read-only, so nothing can be restored into it";

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
    let shows_activity =
        helper_capabilities(entry).is_some_and(|capabilities| capabilities.server_activity);
    if shows_activity && widgets::menu_item(ui, icons::PULSE, "Server activity").clicked() {
        actions.push(ExplorerAction::ServerActivity(id));
    }
    // An engine that bills each read is searched a schema or a table at a time, never whole.
    if data_search(entry) == SearchSupport::Searchable
        && widgets::menu_item(ui, icons::MAGNIFYING_GLASS, "Search data\u{2026}").clicked()
    {
        actions.push(ExplorerAction::SearchData(id, CatalogPath::root()));
    }
    dump_items(ui, entry, actions);
    saved_consoles_menu(ui, id, tree.saved_consoles, actions);
    saved_diagrams_menu(ui, id, tree.saved_diagrams, actions);
    let root = CatalogPath::root();
    let mut new = Vec::new();
    if let Some(capabilities) = helper_capabilities(entry) {
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
        let menu = widgets::menu_button(ui, label, |ui| helper_items(ui, id, new, actions));
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
    let menu = widgets::menu_button(ui, label, |ui| {
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
    /// What **Open data** asks for a language that reads a table in its own words.
    language: QueryLanguage,
    /// Whether **Search data…** is offered, and on which nodes.
    data_search: SearchSupport,
    /// The engine **Dump…** runs the programs of; `None` where it isn't offered (W17).
    dumps: Option<DumpEngine>,
    /// Whether the data source is read-only, which disables **Restore…**.
    read_only: bool,
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
        } else if self.has_matching_descendant(&entry.catalog, &CatalogPath::root())
            || !self.found_on_server(&entry.catalog).is_empty()
        {
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

    /// The matches of the server's search for the filter that the tree doesn't show already,
    /// system objects only when asked for (C0g).
    fn found_on_server<'c>(&self, catalog: &'c CatalogTree) -> Vec<&'c CatalogNode> {
        if self.needle.is_none() {
            return Vec::new();
        }
        catalog
            .found_elsewhere()
            .into_iter()
            .filter(|node| self.show_system || !node.system)
            .collect()
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

/// **Found on the server**: what the server's search for the explorer's filter found that
/// the tree doesn't show, each acting as the node itself, with its schema beside it (C0g).
fn found_on_server(
    ui: &mut Ui,
    tree: &TreeView<'_>,
    filter: &TreeFilter<'_>,
    actions: &mut Vec<ExplorerAction>,
) {
    if filter.needle.is_none() {
        return;
    }
    match tree.catalog.search() {
        Search::Idle | Search::Unsupported => {}
        Search::Searching(_) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Searching the server…");
            });
        }
        Search::Failed { error, .. } => {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        Search::Found { matches, .. } => {
            let found = filter.found_on_server(tree.catalog);
            if found.is_empty() {
                return;
            }
            ui.weak("Found on the server");
            for node in found {
                node_row(ui, tree, node, false, &filter.below_match(), actions);
            }
            if matches.more {
                ui.weak("More matches: type more of the name");
            }
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
    widgets::context_menu(&response, |ui| node_menu(ui, tree, node, actions));
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
    ) || tree.language.read_all(&node.path).is_some();
    if is_read_whole && widgets::menu_item(ui, icons::TABLE, "Open data").clicked() {
        actions.push(ExplorerAction::OpenData(id, node.path.clone()));
    }
    if holds_rows && widgets::menu_item(ui, icons::EXPORT, "Export data…").clicked() {
        actions.push(ExplorerAction::ExportData(id, node.path.clone()));
    }
    search_item(ui, tree, node, actions);
    node_dump_items(ui, tree, node, actions);
    diagram_items(ui, tree, node, actions);
    compare_items(ui, tree, node, actions);
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

/// **Dump…** on a database, a schema or a table, and **Restore into this database…** on a
/// database, of a connected data source DbRover dumps (W17). PostgreSQL's programs connect to
/// one database, so only the data source's own is offered there.
fn node_dump_items(
    ui: &mut Ui,
    tree: &TreeView<'_>,
    node: &CatalogNode,
    actions: &mut Vec<ExplorerAction>,
) {
    let (Some(engine), Some(kind)) = (tree.dumps, node.kind()) else {
        return;
    };
    let dumpable = matches!(
        kind,
        NodeKind::Database | NodeKind::Schema | NodeKind::Table
    ) && (engine != DumpEngine::Postgres || tree.can_change(&node.path));
    if dumpable && widgets::menu_item(ui, icons::DOWNLOAD_SIMPLE, "Dump\u{2026}").clicked() {
        actions.push(ExplorerAction::Dump(tree.id, node.path.clone()));
    }
    // MySQL's tree lists its databases as schemas.
    let database = match (engine, kind) {
        (DumpEngine::Postgres, NodeKind::Database) if tree.can_change(&node.path) => {
            node.path.name_of(NodeKind::Database)
        }
        (DumpEngine::Mysql | DumpEngine::MariaDb, NodeKind::Schema) => {
            node.path.name_of(NodeKind::Schema)
        }
        _ => None,
    };
    let Some(database) = database else {
        return;
    };
    let restore = ui
        .add_enabled_ui(!tree.read_only, |ui| {
            widgets::menu_item(
                ui,
                icons::UPLOAD_SIMPLE,
                "Restore into this database\u{2026}",
            )
        })
        .inner
        .on_disabled_hover_text(READ_ONLY_RESTORE);
    if restore.clicked() {
        actions.push(ExplorerAction::Restore(tree.id, Some(database.to_owned())));
    }
}

/// **Search data…** on a node that holds tables, or on one table (W16). PostgreSQL searches
/// only the database it connects to; an engine that bills each read, only a schema or a table.
fn search_item(
    ui: &mut Ui,
    tree: &TreeView<'_>,
    node: &CatalogNode,
    actions: &mut Vec<ExplorerAction>,
) {
    let Some(kind) = node.kind() else {
        return;
    };
    let offered = match tree.data_search {
        SearchSupport::Unsupported => false,
        SearchSupport::Searchable => matches!(
            kind,
            NodeKind::Database
                | NodeKind::Schema
                | NodeKind::Folder(
                    FolderKind::Tables | FolderKind::Views | FolderKind::MaterializedViews
                )
                | NodeKind::Table
                | NodeKind::View
                | NodeKind::MaterializedView
        ),
        SearchSupport::AsksFirst => {
            matches!(kind, NodeKind::Schema | NodeKind::Table | NodeKind::View)
        }
    };
    if offered
        && tree.can_change(&node.path)
        && widgets::menu_item(ui, icons::MAGNIFYING_GLASS, "Search data\u{2026}").clicked()
    {
        actions.push(ExplorerAction::SearchData(tree.id, node.path.clone()));
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

/// **Compare schema…** and **Save schema snapshot…** on a schema, where its data source's
/// schemas can be read whole.
fn compare_items(
    ui: &mut Ui,
    tree: &TreeView<'_>,
    node: &CatalogNode,
    actions: &mut Vec<ExplorerAction>,
) {
    let comparable = tree
        .capabilities
        .is_some_and(|capabilities| capabilities.schema_compare);
    if !comparable || node.kind() != Some(NodeKind::Schema) {
        return;
    }
    if widgets::menu_item(ui, icons::GIT_DIFF, "Compare schema\u{2026}").clicked() {
        actions.push(ExplorerAction::CompareSchema(tree.id, node.path.clone()));
    }
    if widgets::menu_item(ui, icons::FLOPPY_DISK, "Save schema snapshot\u{2026}").clicked() {
        actions.push(ExplorerAction::SaveSchemaSnapshot(
            tree.id,
            node.path.clone(),
        ));
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
        Some(NodeKind::Folder(
            FolderKind::Users | FolderKind::Logins | FolderKind::ServerRoles,
        )) => new.extend(new_roles(path, capabilities)),
        Some(kind @ (NodeKind::Login | NodeKind::User | NodeKind::Role))
            if capabilities.users == UserModel::LoginsAndUsers =>
        {
            object.extend(sql_server_actions(path, kind));
        }
        Some(kind @ (NodeKind::User | NodeKind::Role))
            if capabilities.users == UserModel::Accounts =>
        {
            object.extend(account_actions(path, kind));
        }
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
        Some(NodeKind::Role) => {
            matches!(capabilities.users, UserModel::Roles | UserModel::Accounts)
        }
        Some(NodeKind::User) => capabilities.users == UserModel::Accounts,
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
        let menu = widgets::menu_button(ui, label, |ui| helper_items(ui, tree.id, new, actions));
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
        UserModel::Accounts => vec![
            (
                icons::USER,
                "User…",
                Helper::Account(AccountHelper::New(path.clone(), AccountKind::User)),
            ),
            (
                icons::USERS,
                "Role…",
                Helper::Account(AccountHelper::New(path.clone(), AccountKind::Role)),
            ),
        ],
        UserModel::LoginsAndUsers => new_sql_server_principals(path),
        UserModel::Unsupported => Vec::new(),
    }
}

/// New login, server role, user or role, by the folder at `path` (or the data source, where
/// New login is offered).
fn new_sql_server_principals(path: &CatalogPath) -> Vec<(&'static str, &'static str, Helper)> {
    let helper = |helper| Helper::SqlServer(helper);
    let folder = path.last().and_then(|segment| match segment.kind {
        NodeKind::Folder(folder) => Some(folder),
        _ => None,
    });
    match (path.is_root(), folder) {
        (true, _) | (false, Some(FolderKind::Logins)) => vec![(
            icons::USER,
            "Login…",
            helper(SqlServerHelper::NewLogin(path.clone())),
        )],
        (false, Some(FolderKind::ServerRoles)) => vec![(
            icons::USERS,
            "Server role…",
            helper(SqlServerHelper::NewServerRole(path.clone())),
        )],
        (false, Some(FolderKind::Users)) => vec![(
            icons::USER,
            "User…",
            helper(SqlServerHelper::NewUser(path.clone())),
        )],
        (false, Some(FolderKind::Roles)) => vec![(
            icons::USERS,
            "Role…",
            helper(SqlServerHelper::NewDatabaseRole(path.clone())),
        )],
        _ => Vec::new(),
    }
}

/// What can be done to the SQL Server login, user or role at `path`.
fn sql_server_actions(
    path: &CatalogPath,
    kind: NodeKind,
) -> Vec<(&'static str, &'static str, Helper)> {
    let helper = |helper| Helper::SqlServer(helper);
    let mut items = vec![
        (
            icons::PENCIL_SIMPLE,
            "Edit…",
            helper(SqlServerHelper::Edit(path.clone())),
        ),
        (
            icons::PENCIL_SIMPLE,
            "Rename…",
            helper(SqlServerHelper::Rename(path.clone())),
        ),
    ];
    if kind == NodeKind::Login {
        items.push((
            icons::LOCK_SIMPLE,
            "Disable or enable…",
            helper(SqlServerHelper::SetEnabled(path.clone())),
        ));
    }
    items.push((
        icons::TRASH,
        "Drop…",
        helper(SqlServerHelper::Drop(path.clone())),
    ));
    items
}

/// What can be done to the MySQL or MariaDB account at `path`: a user (`NodeKind::User`) or
/// a role.
fn account_actions(
    path: &CatalogPath,
    kind: NodeKind,
) -> Vec<(&'static str, &'static str, Helper)> {
    let helper = |helper| Helper::Account(helper);
    let mut items = vec![
        (
            icons::PENCIL_SIMPLE,
            "Edit…",
            helper(AccountHelper::Edit(path.clone())),
        ),
        (
            icons::PENCIL_SIMPLE,
            "Rename…",
            helper(AccountHelper::Rename(path.clone())),
        ),
    ];
    if kind == NodeKind::User {
        items.push((
            icons::LOCK_SIMPLE,
            "Lock or unlock…",
            helper(AccountHelper::Lock(path.clone())),
        ));
        items.push((
            icons::CLOCK,
            "Expire password…",
            helper(AccountHelper::ExpirePassword(path.clone())),
        ));
    }
    items.push((
        icons::TRASH,
        "Drop…",
        helper(AccountHelper::Drop(path.clone())),
    ));
    items
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
    widgets::menu_button(ui, "Open saved console", |ui| {
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

/// **Reports**: the report files, under the data sources, each opening its tab, with a mark on
/// those that can't run. Hidden while filtering unless a report's name matches.
fn reports_section(
    ui: &mut Ui,
    reports: &Reports,
    filter: &TreeFilter<'_>,
    collapsed: &mut BTreeSet<Group>,
    actions: &mut Vec<ExplorerAction>,
) {
    let palette = &Palette::for_visuals(ui.visuals());
    let shown: Vec<_> = reports
        .listed()
        .iter()
        .filter(|listed| {
            filter
                .needle
                .is_none_or(|needle| filter.matches(listed.title(), needle))
        })
        .collect();
    if filter.needle.is_some() && shown.is_empty() {
        return;
    }
    ui.add_space(4.0);
    let expanded = filter.needle.is_some() || !collapsed.contains(&Group::Reports);
    let row = ui.horizontal(|ui| {
        let toggled = expander(ui, expanded, "Reports").clicked();
        ui.label(widgets::glyph(icons::CLIPBOARD_TEXT).color(palette.primary));
        let name = ui.add(
            Label::new(RichText::new("Reports").strong())
                .selectable(false)
                .sense(Sense::click()),
        );
        widgets::count_badge(ui, reports.listed().len());
        (name, toggled)
    });
    let (name, toggled) = row.inner;
    if toggled || name.double_clicked() {
        if expanded {
            collapsed.insert(Group::Reports);
        } else {
            collapsed.remove(&Group::Reports);
        }
    }
    widgets::context_menu(&name, |ui| reports_menu(ui, actions));
    if !expanded {
        return;
    }
    ui.indent(Id::new("explorer_reports"), |ui| {
        if let Some(error) = reports.unavailable() {
            ui.colored_label(
                palette.error,
                format!("Can't read the reports folder: {error}"),
            );
        } else if reports.listed().is_empty() {
            ui.weak("No reports yet. Right-click Reports for New report.");
        }
        for listed in shown {
            ui.horizontal(|ui| {
                let (glyph, color) = match &listed.read {
                    Ok(_) => (icons::CLIPBOARD_TEXT, palette.text_weak),
                    Err(_) => (icons::WARNING, palette.error),
                };
                ui.label(widgets::glyph(glyph).color(color));
                let row = ui.add(
                    Label::new(listed.title())
                        .selectable(false)
                        .sense(Sense::click()),
                );
                let row = match &listed.read {
                    Ok(_) => row.on_hover_text(listed.path.display().to_string()),
                    Err(problems) => row.on_hover_text(format!(
                        "{} problem(s) in {}",
                        problems.len(),
                        listed.path.display()
                    )),
                };
                if row.clicked() {
                    actions.push(ExplorerAction::Report(ReportAction::Open(
                        listed.name.clone(),
                    )));
                }
                widgets::context_menu(&row, |ui| {
                    if widgets::menu_item(ui, icons::PENCIL_SIMPLE, "Rename…").clicked() {
                        actions.push(ExplorerAction::Report(ReportAction::Rename(
                            listed.name.clone(),
                        )));
                    }
                    ui.separator();
                    reports_menu(ui, actions);
                });
            });
        }
    });
}

fn reports_menu(ui: &mut Ui, actions: &mut Vec<ExplorerAction>) {
    let items = [
        (icons::FILE_PLUS, "New report", ReportAction::New),
        (
            icons::FOLDER_OPEN,
            "Open the reports folder",
            ReportAction::OpenFolder,
        ),
        (icons::ARROWS_CLOCKWISE, "Reload", ReportAction::Reload),
    ];
    for (glyph, label, action) in items {
        if widgets::menu_item(ui, glyph, label).clicked() {
            actions.push(ExplorerAction::Report(action));
        }
    }
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
    let menu = widgets::menu_button(ui, label, |ui| {
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
    use std::cell::RefCell;

    use dbrover_core::{ConnectionConfig, SafetyLevel, SqliteConfig};
    use dbrover_engine::SessionState;
    use dbrover_store::ProjectSlug;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::test_support::ui_harness;

    fn project(name: &str) -> Project {
        Project::new(
            ProjectId::generate(),
            name.to_owned(),
            ProjectSlug::from_name(name),
        )
    }

    fn entry(name: &str, project: Option<ProjectId>) -> DataSourceEntry {
        DataSourceEntry {
            data_source: dbrover_core::DataSource {
                id: DataSourceId::generate(),
                name: name.to_owned(),
                color: None,
                safety: SafetyLevel::Normal,
                project,
                mcp_login: None,
                config: ConnectionConfig::Sqlite(SqliteConfig {
                    path: format!("{name}.db").into(),
                    read_only: false,
                }),
            },
            session: SessionState::Disconnected,
            catalog: CatalogTree::default(),
        }
    }

    /// Shows the explorer with `projects` and `entries` and nothing else, drives it with
    /// `drive`, and returns what `drive` returned along with everything the explorer asked for.
    fn explorer<R>(
        projects: &[Project],
        entries: Vec<DataSourceEntry>,
        options: &RefCell<ExplorerOptions>,
        drive: impl FnOnce(&mut egui_kittest::Harness<'_>) -> R,
    ) -> (R, Vec<ExplorerAction>) {
        let data_sources = DataSources::Loaded(entries);
        let saved_consoles = SavedConsoles::default();
        let saved_diagrams = SavedDiagrams::default();
        let reports = Reports::default();
        let query_folders = QueryFolders::default();
        let asked = RefCell::new(Vec::new());
        let mut harness = ui_harness(|ui| {
            let view = ExplorerView {
                data_sources: &data_sources,
                projects,
                saved_consoles: &saved_consoles,
                saved_diagrams: &saved_diagrams,
                actions: &[],
                reports: &reports,
                query_folders: &query_folders,
            };
            let mut pushed = Vec::new();
            show(ui, &view, &mut options.borrow_mut(), &mut pushed);
            asked.borrow_mut().extend(pushed);
        });
        let driven = drive(&mut harness);
        let pushed = asked.borrow().clone();
        (driven, pushed)
    }

    /// Drags the row labelled `source` onto the row labelled `target` and returns the moves the
    /// explorer asked for.
    ///
    /// egui_kittest's pointer helpers push events and step no frames of their own, so this steps
    /// between them: a press and a release at one position is a click, and egui starts a drag
    /// only once the pointer has moved a few points from the press, hence the halfway move.
    fn drag_onto(
        projects: &[Project],
        entries: Vec<DataSourceEntry>,
        source: &str,
        target: &str,
    ) -> Vec<ExplorerAction> {
        let options = RefCell::new(ExplorerOptions::default());
        let (_, asked) = explorer(projects, entries, &options, |harness| {
            let from = harness.get_by_label(source).rect().center();
            let to = harness.get_by_label(target).rect().center();
            harness.hover_at(from);
            harness.step();
            harness.drag_at(from);
            harness.step();
            harness.hover_at(from.lerp(to, 0.5));
            harness.step();
            harness.hover_at(to);
            harness.step();
            harness.drop_at(to);
            harness.run();
        });
        asked
            .into_iter()
            .filter(|action| matches!(action, ExplorerAction::MoveToProject(..)))
            .collect()
    }

    /// Dropping a data source on a project header moves it there, and on **No project** takes it
    /// out of its project. Dropping it on the group it is already in, or on anything that isn't
    /// a header, moves nothing: those targets never claim the payload, and egui drops it when
    /// the button comes up.
    #[test]
    fn dragging_a_data_source_onto_a_header_moves_it_into_that_group() {
        let analytics = project("Analytics");
        let projects = [analytics.clone()];
        let loose = entry("scratch", None);
        let member = entry("warehouse", Some(analytics.id));

        let moved = drag_onto(
            &projects,
            vec![member.clone(), loose.clone()],
            "scratch",
            "Analytics",
        );
        assert_eq!(
            moved,
            [ExplorerAction::MoveToProject(
                loose.data_source.id,
                Some(analytics.id)
            )]
        );

        let out = drag_onto(
            &projects,
            vec![member.clone(), loose.clone()],
            "warehouse",
            "No project",
        );
        assert_eq!(
            out,
            [ExplorerAction::MoveToProject(member.data_source.id, None)],
            "No project takes it out of its project"
        );

        let same = drag_onto(
            &projects,
            vec![member.clone(), loose.clone()],
            "warehouse",
            "Analytics",
        );
        assert_eq!(same, [], "it is already in Analytics");

        let onto_source = drag_onto(
            &projects,
            vec![member.clone(), loose.clone()],
            "scratch",
            "warehouse",
        );
        assert_eq!(onto_source, [], "a data source's row is not a drop target");
    }

    /// The drop target covers the whole header row, including the expander that folds the group.
    /// It only senses hover, so the expander must still get its clicks.
    #[test]
    fn a_header_s_expander_still_folds_the_group_under_the_drop_target() {
        let analytics = project("Web shop");
        let group = Group::Project(analytics.id);
        let options = RefCell::new(ExplorerOptions::default());
        let entries = vec![entry("scratch", None)];
        explorer(&[analytics], entries, &options, |harness| {
            harness.get_by_label("Collapse Web shop").click();
            harness.run();
        });
        assert!(
            options.borrow().collapsed_groups.contains(&group),
            "clicking the expander folded the project"
        );
    }

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

    /// The band only reaches [`DRAG_SCROLL_BAND`] into the view from each edge, scrolls towards
    /// the top above and the bottom below, and is strongest at the edge. A pointer past the edge
    /// scrolls no faster than one on it, so a drag that leaves the panel doesn't bolt.
    #[test]
    fn drag_scrolls_only_near_the_explorer_s_edges() {
        let view = Rangef::new(100.0, 400.0);
        let cases = [
            (100.0, DRAG_SCROLL_SPEED),
            (90.0, DRAG_SCROLL_SPEED),
            (112.0, DRAG_SCROLL_SPEED / 2.0),
            (124.0, 0.0),
            (250.0, 0.0),
            (376.0, 0.0),
            (388.0, -DRAG_SCROLL_SPEED / 2.0),
            (400.0, -DRAG_SCROLL_SPEED),
            (410.0, -DRAG_SCROLL_SPEED),
        ];
        for (y, expected) in cases {
            let delta = drag_scroll_delta(view, y);
            assert!(
                (delta - expected).abs() < f32::EPSILON,
                "at {y}: expected {expected}, got {delta}"
            );
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
