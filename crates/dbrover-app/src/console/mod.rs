//! Query consoles: a SQL editor bound to a data source, and the results of its latest run.
//!
//! This module holds the state and its rules (what to run, how events update a run); `view`
//! and `results` draw it. See `docs/features/sql-runner.md`.

mod catalog_lint;
mod catalog_source;
pub(crate) mod cell_viewer;
mod chart;
mod clipboard;
mod completion;
mod completion_popup;
mod documents;
mod edit;
mod file;
mod find;
mod find_bar;
mod grid;
pub(crate) mod highlight;
mod keys;
mod plan;
mod problems;
mod results;
mod snippet;
mod view;

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use dbrover_core::{
    Cell, ColumnInfo, DataSource, DataSourceId, Identifier, RelationName, ResultShape, Row,
    SafetyLevel, TableDescription, TransactionStatus,
};
use dbrover_engine::{
    Command, ConsoleId, EditFailure, Event, Export, ExportAll, Exporter, RowEdit, RunId, RunOrigin,
    RunOutcome, RunPurpose, ScanApproval, SessionState, StatementFailure, StatementSummary,
    TransactionEnd, TransactionMode, WatchedFile,
};
use dbrover_export::{ExportColumn, ExportOptions, ExportSpec, RowSource};

use crate::export_dialog::{ExportPreview, PREVIEW_ROWS};
use dbrover_language::redis::{KeyView, ViewShape};
use dbrover_language::{EditsRun, ExplainDepth, QueryLanguage, command_at};
use dbrover_plan::PlanFormat;
use dbrover_sql::{Dialect, FormatOptions, ParameterValues, format_ranges};
use dbrover_store::{ConsoleKey, Snippet, SnippetBody, SqlFileContents};

use edit::{EditTarget, Editability};

use crate::custom_actions::{GridCells, GridTable};
use crate::picture::PictureFormat;
use file::{ConfigFile, Reload, SqlFile, Storage};

pub use clipboard::{ClipboardCopy, ClipboardFormat};
pub use edit::ChangeCount;
pub use file::Disk;
pub use find::Found;
pub use keys::RowsToOpen;

pub use catalog_source::TreeCatalog;
pub use completion::{Candidate, RecentCompletions};
pub use problems::LintOptions;
pub use view::{ConsoleAction, ConsoleContext, show};

/// One statement sent to the engine, and where it came from in the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedStatement {
    pub sql: String,
    /// Byte range in the console's text, to place error markers.
    pub range: Range<usize>,
    /// How many bytes at the start of `sql` aren't in the console's text, such as an
    /// `EXPLAIN` prefix. Error positions are shifted back by this much.
    pub prefix_len: usize,
    /// Set when `sql` wraps the statement to sort and filter its rows in the database.
    pub in_database: Option<InDatabase>,
    /// Set when `sql` explains the statement, and the database answers with a plan in this
    /// format, to draw rather than show as rows.
    pub plan: Option<PlanFormat>,
}

/// A statement whose rows the database sorts and filters, by running it wrapped: the
/// statement as typed, which decides what can be edited, and the sort and filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InDatabase {
    pub typed: String,
    pub sort: Option<grid::SortKey>,
    pub filter: String,
}

/// A run as a console asks for it: its statements, what running them is for, and the values
/// of their `:name` parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedRun {
    pub statements: Vec<SubmittedStatement>,
    pub purpose: RunPurpose,
    /// None yet for a run typed in the editor, which is asked for them; the values the
    /// statement last ran with for one that runs a shown statement again.
    pub parameters: ParameterValues,
    /// What the history records the statements as coming from.
    pub origin: RunOrigin,
    pub destination: RunDestination,
    /// Whether a statement that would read a whole table runs, on a data source whose cost
    /// guard asks first.
    pub scans: ScanApproval,
}

/// Where a run's rows go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunDestination {
    Grid,
    /// Run to file: every row of the run's one statement, written to the file as it arrives.
    File {
        path: PathBuf,
        spec: ExportSpec,
    },
}

impl SubmittedRun {
    /// The `:name` parameters of the run's statements, each once, in order.
    pub fn parameter_names(&self, dialect: Dialect) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for statement in &self.statements {
            for name in dbrover_sql::parameter_names(&statement.sql, dialect) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }
}

/// Which rows an export writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportedRows {
    /// The rows the grid has loaded, as it shows them.
    Loaded,
    /// Every row the statement returns, by running it again.
    All,
}

/// What a format covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatScope {
    /// The selection if there is one, otherwise the statement under the cursor.
    Current,
    /// Every statement in the console.
    All,
}

/// Which statements a run covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunScope {
    /// The selection if there is one, otherwise the statement under the cursor.
    Current,
    /// Every statement in the console.
    All,
    /// The statement under the cursor (or the first one selected), explained to this depth.
    Explain(ExplainDepth),
}

impl RunScope {
    /// What running the scope's statements is for.
    pub fn purpose(self) -> RunPurpose {
        match self {
            Self::Current | Self::All => RunPurpose::Run,
            Self::Explain(ExplainDepth::Estimated) => RunPurpose::Explain,
            Self::Explain(ExplainDepth::Analysed) => RunPurpose::ExplainAnalyse,
        }
    }
}

pub struct QueryConsole {
    /// The engine session's id, new every time the app starts.
    id: ConsoleId,
    /// The console's file, which outlives the app.
    key: ConsoleKey,
    title: String,
    language: QueryLanguage,
    text: String,
    /// The editor's cursor or selection as byte offsets into `text`, in either order.
    selection: Range<usize>,
    /// The state of the console's own connection.
    session: SessionState,
    transactions: TransactionMode,
    transaction: TransactionStatus,
    /// How the last Commit or Rollback went, until the next run.
    transaction_ended: Option<Result<TransactionEnd, String>>,
    run: Option<RunView>,
    /// Where the last failure points in `text`; underlined until the text changes.
    error_marker: Option<Range<usize>>,
    /// A byte offset to move the editor's cursor to on the next frame, after the text was
    /// changed from outside the editor.
    pending_cursor: Option<usize>,
    /// A range to select in the editor and scroll to on the next frame, leaving the keyboard
    /// focus where it is: a match the find bar went to.
    pending_selection: Option<Range<usize>>,
    find: find::FindBar,
    /// The text before it was replaced from outside the editor (formatted, or reloaded from
    /// its file), for the editor to record as an undo point next to the new text, so one
    /// undo takes the whole change back.
    replaced_text: Option<String>,
    /// Why the last format was refused, until the text changes.
    format_error: Option<String>,
    /// Syntax errors and lint warnings, kept up to date while typing.
    problems: problems::Problems,
    /// Where each shown problem's squiggle was drawn last frame (with its index), for hovering.
    problem_rects: Vec<(egui::Rect, usize)>,
    /// The cell shown in full in the cell viewer window.
    viewed_cell: Option<ViewedCell>,
    /// The completion list, while it is open.
    completion: Option<Completion>,
    /// The fields of the snippet just inserted, while Tab moves through them.
    snippet_fields: Option<snippet::SnippetFields>,
    /// The candidate accepted last, until the app records it as recent.
    accepted_completion: Option<completion::Candidate>,
    /// How the last export went: where the rows went, or why they didn't.
    export_status: Option<Result<String, String>>,
    /// An export of every row in progress: its run, to cancel it, and how many rows it has
    /// written.
    exporting: Option<(RunId, usize)>,
    /// The format of the export last sent, for what its status says.
    last_export: Option<ExportSpec>,
    /// Where the chart's picture is being written, until the engine says it was.
    chart_export: Option<PathBuf>,
    /// The file the last run went to, run to file rather than into the grid; the results
    /// panel shows how that went in place of rows.
    to_file: Option<PathBuf>,
    storage: Storage,
    /// The values the console's parameters last ran with, which the parameters dialog starts
    /// from, and which running a shown statement again reuses.
    parameters: ParameterValues,
    /// The data source's name and safety level, kept up to date as it is edited.
    data_source_name: String,
    safety: SafetyLevel,
}

pub struct RunView {
    id: RunId,
    statements: Vec<StatementView>,
    state: RunState,
    /// The statement whose result is shown.
    selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Running,
    Finished {
        outcome: RunOutcome,
        elapsed: Duration,
    },
}

pub struct StatementView {
    /// As typed, even when the database sorts and filters its rows by running it wrapped.
    sql: String,
    range: Range<usize>,
    prefix_len: usize,
    columns: Vec<ColumnInfo>,
    rows: Vec<Row>,
    /// More rows can be fetched.
    more: bool,
    /// A fetch is in flight, so the grid doesn't ask twice.
    fetching: bool,
    result: Option<Result<StatementSummary, StatementFailure>>,
    grid: grid::GridView,
    edits: edit::EditSession,
    /// The table the rows come from, while it is being described.
    describing: Option<RelationName>,
    /// The table's foreign keys, once described, for following them from the rows.
    links: keys::KeyLinks,
    /// The rows came from the statement wrapped to sort and filter them in the database.
    wrapped: bool,
    /// The result is documents, which `columns` and `rows` show flattened.
    documents: Option<documents::DocumentResult>,
    /// The bar above a Redis key's value, and what it is asking.
    key_bar: results::KeyBar,
    /// The statement explains another, and its one value is a plan to draw.
    plan: Option<plan::PlanView>,
    /// The chart of the rows, shown in place of the grid when picked.
    chart: chart::ChartTab,
}

/// The selected cells of loaded rows, for a custom action: added rows have no values yet.
fn selected_cells(
    columns: &[ColumnInfo],
    rows: &[Row],
    grid: &grid::GridView,
    edits: &edit::EditSession,
) -> Option<GridCells> {
    let selected_columns = grid.selected_columns()?;
    let selected_rows = grid
        .selected_rows()?
        .filter_map(|position| grid.order().get(position))
        .filter_map(|&index| rows.get(index).cloned())
        .collect::<Vec<_>>();
    if selected_rows.is_empty() {
        return None;
    }
    Some(GridCells {
        columns: columns.iter().map(|column| column.name.clone()).collect(),
        selected: selected_columns.collect(),
        rows: selected_rows,
        table: edits.target().map(|target| GridTable {
            schema: target.table.schema.clone(),
            name: target.table.name.clone(),
        }),
    })
}

/// Whether a key's value, read by `view`'s command into `columns`, can be edited in the grid.
fn key_editability(view: KeyView, columns: &[ColumnInfo], read_only: Option<&str>) -> Editability {
    let names: Vec<&str> = columns.iter().map(|column| column.name.as_str()).collect();
    if let Some(reason) = read_only {
        Editability::ReadOnly(reason.to_owned())
    } else if !view.fits(&names) {
        Editability::ReadOnly("Only a key's whole value can be edited".to_owned())
    } else if matches!(view.shape(), ViewShape::Stream | ViewShape::Other) {
        Editability::ReadOnly(format!(
            "A {} can't be edited in the grid; change it with commands",
            view.shape().label()
        ))
    } else {
        Editability::Key(view)
    }
}

/// Whether a result of documents can be edited in the grid: a `find`'s on one collection, or a
/// search's hits.
fn documents_editability(
    language: QueryLanguage,
    command: &str,
    columns: &[ColumnInfo],
    read_only: Option<&str>,
) -> Editability {
    match (language.document_view(command), read_only) {
        (_, Some(reason)) => Editability::ReadOnly(reason.to_owned()),
        (Some(view), None) => Editability::Documents(edit::DocumentsTarget::new(view, columns)),
        (None, None) => Editability::ReadOnly(language.uneditable_documents().to_owned()),
    }
}

impl StatementView {
    /// The statements that submit the pending changes, each with its row: SQL for a table's
    /// rows, a Redis command per change for a key's value, a MongoDB command per change for a
    /// collection's documents.
    fn edit_statements(
        &self,
        dialect: Option<Dialect>,
    ) -> Result<Vec<(edit::RowRef, RowEdit)>, String> {
        let pending = &self.edits.pending;
        if let Some(view) = self.edits.key_view() {
            return pending.key_commands(view, &self.columns, &self.rows);
        }
        if let (Some(target), Some(documents)) = (self.edits.documents(), &self.documents) {
            return pending.document_commands(target, documents, &self.columns);
        }
        match (self.edits.target(), dialect) {
            (Some(target), Some(dialect)) => {
                Ok(pending.statements(target, &self.columns, &self.rows, dialect))
            }
            _ => Err("These rows can't be edited".to_owned()),
        }
    }

    /// Whether the statement returns rows or documents, rather than only running.
    fn returns_rows(&self) -> bool {
        !self.columns.is_empty() || self.documents.is_some()
    }

    /// Reads a page of rows into the grid: as they are, or flattened when they are documents.
    fn add_rows(&mut self, page: &[Row]) {
        let Some(documents) = &mut self.documents else {
            self.rows.extend(page.iter().cloned());
            return;
        };
        if documents.add_page(page, &mut self.columns, &mut self.rows)
            == documents::ColumnsMoved::Yes
        {
            self.grid.columns_moved();
        }
    }

    /// Expands or collapses a column of documents.
    fn fold_column(&mut self, fold: &documents::ColumnFold) {
        if let Some(documents) = &mut self.documents {
            documents.fold(fold, &mut self.columns, &mut self.rows);
            self.grid.columns_moved();
        }
    }

    /// Where the rows come from, for the export formats: the dialect and, when the grid can
    /// edit it, the table, which the SQL formats need; whether they are documents, which JSON
    /// and JSON Lines write whole.
    pub fn row_source(&self, dialect: Option<Dialect>) -> RowSource {
        RowSource {
            dialect,
            table: self.edits.target().map(EditTarget::source_table),
            shape: match self.documents {
                Some(_) => ResultShape::Documents,
                None => ResultShape::Rows,
            },
        }
    }

    /// The documents at `indices` among those loaded, one a row, for the formats that write
    /// them whole; none for a result of rows.
    fn document_rows(&self, indices: &[usize]) -> Vec<Row> {
        self.documents
            .as_ref()
            .map(|documents| documents.document_rows(indices))
            .unwrap_or_default()
    }

    fn selected_cells(&self) -> Option<GridCells> {
        selected_cells(&self.columns, &self.rows, &self.grid, &self.edits)
    }

    /// The selected columns of the selected rows, as the grid shows them, for copying, and
    /// those rows' documents when they are documents.
    fn selection(&self, dialect: Option<Dialect>) -> Option<clipboard::Selection> {
        let cells = self.selected_cells()?;
        let columns = cells
            .selected
            .iter()
            .filter_map(|&index| self.columns.get(index))
            .map(|column| ExportColumn::typed_by(column, dialect))
            .collect();
        let selected_rows: Vec<usize> = self
            .grid
            .selected_rows()?
            .filter_map(|position| self.grid.order().get(position).copied())
            .collect();
        let rows = cells
            .rows
            .iter()
            .map(|row| {
                cells
                    .selected
                    .iter()
                    .map(|&index| row.get(index).cloned().flatten())
                    .collect()
            })
            .collect();
        Some(clipboard::Selection {
            columns,
            rows,
            documents: self.document_rows(&selected_rows),
            source: self.row_source(dialect).select_columns(&cells.selected),
        })
    }

    /// The statement to run to show this result again, sorted and filtered as the grid now
    /// asks: wrapped if the database is to sort and filter it, as typed otherwise.
    fn resubmission(&self, dialect: Option<Dialect>) -> Result<SubmittedStatement, String> {
        let as_typed = SubmittedStatement {
            sql: self.sql.clone(),
            range: self.range.clone(),
            prefix_len: self.prefix_len,
            in_database: None,
            plan: self.plan.as_ref().map(plan::PlanView::format),
        };
        if !self.grid.is_in_database() || !self.grid.has_sort_or_filter() {
            return Ok(as_typed);
        }
        let columns: Vec<String> = self
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect();
        let dialect = dialect.ok_or("Only SQL results are sorted and filtered in the database")?;
        let sort = self.grid.sort();
        let arranged = dbrover_sql::arrange(
            &self.sql,
            dialect,
            &dbrover_sql::Arrangement {
                columns: &columns,
                sort: sort.map(|sort| dbrover_sql::SortOrder {
                    column: sort.column,
                    descending: sort.descending,
                }),
                filter: self.grid.filter(),
            },
        )?;
        Ok(SubmittedStatement {
            sql: arranged.sql,
            prefix_len: arranged.prefix_len,
            in_database: Some(InDatabase {
                typed: self.sql.clone(),
                sort,
                filter: self.grid.filter().to_owned(),
            }),
            ..as_typed
        })
    }

    /// Whether the database can sort and filter these rows; `Err` says why not.
    fn can_arrange(&self, dialect: Dialect) -> Result<(), String> {
        if self.documents.is_some() {
            return Err("Documents are sorted and filtered here, among those loaded".to_owned());
        }
        let columns: Vec<String> = self
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect();
        dbrover_sql::can_arrange(&self.sql, dialect, &columns)
    }
}

/// The completion list at the cursor.
struct Completion {
    /// Opened on request, rather than by typing: stays open with nothing to suggest.
    explicit: bool,
    candidates: Vec<completion::Candidate>,
    /// The typed prefix, which an accepted candidate replaces.
    replace: Range<usize>,
    selected: usize,
    /// Where the list was drawn last frame, so a click on it doesn't count as leaving the
    /// editor.
    rect: Option<egui::Rect>,
    /// Catalog levels it needs are still loading.
    loading: bool,
    /// The bodies of the snippets offered, by trigger.
    snippets: Vec<(String, SnippetBody)>,
}

/// The one column of an export that writes documents whole, which no format writes out.
const DOCUMENT_COLUMN: &str = "document";

/// A copy of one cell, so the viewer stays open while new rows arrive or a new run starts.
pub struct ViewedCell {
    column: String,
    value: Cell,
    /// The value read as JSON, when it is an object or an array, for the tree.
    tree: Option<dbrover_document::Value>,
    /// How the tree reads it: Extended JSON from a document, plain JSON otherwise.
    syntax: dbrover_document::Syntax,
    /// Whether the tree or the text is shown, for a value with a tree.
    shown_as: cell_viewer::ShownAs,
}

impl ViewedCell {
    fn new(column: String, value: Cell, syntax: dbrover_document::Syntax) -> Self {
        let tree = value.as_deref().and_then(cell_viewer::json_tree);
        Self {
            column,
            value,
            tree,
            syntax,
            shown_as: cell_viewer::ShownAs::Tree,
        }
    }
}

impl QueryConsole {
    pub fn new(
        id: ConsoleId,
        key: ConsoleKey,
        title: String,
        language: QueryLanguage,
        text: String,
    ) -> Self {
        Self {
            id,
            key,
            title,
            language,
            text,
            selection: 0..0,
            session: SessionState::Disconnected,
            transactions: TransactionMode::Auto,
            transaction: TransactionStatus::Idle,
            transaction_ended: None,
            run: None,
            error_marker: None,
            pending_cursor: None,
            pending_selection: None,
            find: find::FindBar::default(),
            replaced_text: None,
            format_error: None,
            problems: problems::Problems::new(Instant::now()),
            problem_rects: Vec::new(),
            viewed_cell: None,
            completion: None,
            accepted_completion: None,
            snippet_fields: None,
            export_status: None,
            exporting: None,
            last_export: None,
            chart_export: None,
            to_file: None,
            storage: Storage::Config(ConfigFile::default()),
            parameters: ParameterValues::default(),
            data_source_name: String::new(),
            safety: SafetyLevel::Normal,
        }
    }

    /// This console on `data_source`, as careful as its safety level asks: a careful one
    /// starts in manual commit mode.
    pub fn on(mut self, data_source: &DataSource) -> Self {
        self.follow(data_source);
        if self.safety == SafetyLevel::Careful {
            self.transactions = TransactionMode::Manual;
        }
        self
    }

    /// Takes the data source's new name and safety level after it was edited. The commit
    /// mode stays as the user left it.
    pub fn follow(&mut self, data_source: &DataSource) {
        data_source.name.clone_into(&mut self.data_source_name);
        self.safety = data_source.safety;
    }

    pub fn parameters(&self) -> &ParameterValues {
        &self.parameters
    }

    /// Keeps the values a run's parameters were given, over those of the same names.
    pub fn remember_parameters(&mut self, values: &ParameterValues) {
        self.parameters.extend(values);
    }

    pub fn safety(&self) -> SafetyLevel {
        self.safety
    }

    pub fn data_source_name(&self) -> &str {
        &self.data_source_name
    }

    /// How many rows the shown statement's pending edits add, change and delete.
    pub fn pending_changes(&self) -> ChangeCount {
        self.run
            .as_ref()
            .and_then(|run| run.statements.get(run.selected))
            .map_or_else(ChangeCount::default, |statement| {
                statement.edits.pending.count()
            })
    }

    /// A console reopened from its file, which closing it again must keep.
    pub fn reopened(mut self) -> Self {
        if let Storage::Config(file) = &mut self.storage {
            file.keep = true;
        }
        self
    }

    /// This console showing a `.sql` file just read, rather than one in the config folder.
    pub fn showing(mut self, path: PathBuf, contents: SqlFileContents) -> Self {
        let file = SqlFile::new(path, contents);
        self.title = file.file_name();
        self.storage = Storage::File(file);
        self
    }

    pub fn id(&self) -> ConsoleId {
        self.id
    }

    pub fn data_source(&self) -> DataSourceId {
        self.key.data_source
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Where each problem's squiggle was drawn last frame, for tests to hover over.
    #[cfg(test)]
    pub fn squiggle_rects(&self) -> impl Iterator<Item = egui::Rect> + '_ {
        self.problem_rects.iter().map(|(rect, _)| *rect)
    }

    /// Decides, for each result that hasn't been looked at, whether it may be editable, and
    /// returns the tables to describe: those a result's rows come from, which tell whether it
    /// is editable and which foreign keys can be followed from it. A read-only data source's
    /// tables are described too, for their keys.
    pub fn tables_to_describe(&mut self) -> Vec<RelationName> {
        let language = self.language;
        let read_only = (self.safety == SafetyLevel::ReadOnly)
            .then(|| format!("\u{201c}{}\u{201d} is read-only", self.data_source_name));
        let Some(run) = self.run.as_mut() else {
            return Vec::new();
        };
        let mut tables = Vec::new();
        for statement in &mut run.statements {
            if !statement.returns_rows() || statement.edits.editability != Editability::Unknown {
                continue;
            }
            if statement.documents.is_some()
                && let Some(table) = language.documents_table(&statement.sql)
            {
                // A DynamoDB table's items are found by its primary key, which only the
                // database knows: the view waits for the table's description.
                let table = RelationName {
                    schema: None,
                    name: Identifier {
                        name: table,
                        quoted: true,
                    },
                };
                tables.push(table.clone());
                statement.describing = Some(table.clone());
                statement.edits.editability = match &read_only {
                    Some(reason) => Editability::ReadOnly(reason.clone()),
                    None => Editability::Describing(table),
                };
                continue;
            }
            if statement.documents.is_some() {
                statement.edits.editability = documents_editability(
                    language,
                    &statement.sql,
                    &statement.columns,
                    read_only.as_deref(),
                );
                continue;
            }
            if let Some(view) = language.key_view(&statement.sql) {
                statement.edits.editability =
                    key_editability(view, &statement.columns, read_only.as_deref());
                continue;
            }
            let Some(dialect) = language.dialect() else {
                statement.edits.editability = Editability::ReadOnly(format!(
                    "Results in {} can't be edited",
                    language.display_name()
                ));
                continue;
            };
            if let Some(reason) = dialect.read_only_results() {
                statement.edits.editability = Editability::ReadOnly(reason.to_owned());
                continue;
            }
            statement.edits.editability = match edit::editable_source(&statement.sql, dialect) {
                Ok(table) => {
                    tables.push(table.clone());
                    statement.describing = Some(table.clone());
                    match &read_only {
                        Some(reason) => Editability::ReadOnly(reason.clone()),
                        None => Editability::Describing(table),
                    }
                }
                Err(reason) => Editability::ReadOnly(read_only.clone().unwrap_or(reason)),
            };
        }
        tables
    }

    fn table_described(
        &mut self,
        table: &RelationName,
        result: &Result<Option<TableDescription>, String>,
    ) {
        // A SQL or CQL statement's table is described for its rows, and a DynamoDB `SELECT`'s
        // for its items' key.
        let dialect = self.language.dialect();
        let Some(run) = self.run.as_mut() else {
            return;
        };
        for statement in &mut run.statements {
            if statement.describing.as_ref() != Some(table) {
                continue;
            }
            statement.describing = None;
            if let Ok(Some(description)) = result {
                statement.links = keys::KeyLinks::new(description, &statement.columns);
            }
            if statement.edits.editability != Editability::Describing(table.clone()) {
                continue;
            }
            let language = self.language;
            statement.edits.editability = match result {
                Ok(Some(description)) if statement.documents.is_some() => {
                    match language.described_view(&statement.sql, &description.primary_key) {
                        Some(view) => Editability::Documents(edit::DocumentsTarget::new(
                            view,
                            &statement.columns,
                        )),
                        None => Editability::ReadOnly(language.uneditable_documents().to_owned()),
                    }
                }
                Ok(Some(description)) => match dialect {
                    Some(dialect) => {
                        edit::edit_target(description.clone(), &statement.columns, dialect)
                    }
                    None => Editability::ReadOnly(language.uneditable_documents().to_owned()),
                },
                Ok(None) => Editability::ReadOnly(format!(
                    "{} isn't a table",
                    table.to_sql(self.language.quoting())
                )),
                Err(error) => Editability::ReadOnly(format!("Couldn't look up the table: {error}")),
            };
        }
    }

    /// The statements for the shown statement's pending changes, marking them as submitted.
    /// `None` if there is nothing to submit, or a change can't be written yet, which the grid
    /// then says.
    pub fn submit_edits(&mut self) -> Option<Command> {
        let dialect = self.dialect();
        let run = self.run.as_mut()?;
        let statement = run.statements.get_mut(run.selected)?;
        if statement.edits.pending.is_empty() || statement.edits.submitting {
            return None;
        }
        let statements = match statement.edit_statements(dialect) {
            Ok(statements) => statements,
            Err(reason) => {
                statement.edits.error = Some(reason);
                return None;
            }
        };
        let statements = statements.into_iter().map(|(_, edit)| edit).collect();
        statement.edits.submitting = true;
        statement.edits.error = None;
        statement.edits.editing = None;
        Some(Command::ApplyEdits {
            console: self.id,
            statements,
        })
    }

    fn edits_applied(&mut self, result: &Result<(), EditFailure>) {
        let dialect = self.dialect();
        let one_by_one = self.language.edits_run() == Some(EditsRun::OneByOne);
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let Some(statement) = run
            .statements
            .iter_mut()
            .find(|statement| statement.edits.submitting)
        else {
            return;
        };
        // The same statements, in the same order, as were submitted.
        let submitted = statement.edit_statements(dialect).unwrap_or_default();
        let edits = &mut statement.edits;
        edits.submitting = false;
        match result {
            Ok(()) => {
                // A key's value is read again when rows came or went: a list item is removed by
                // value, so the server may have removed another item equal to it. Documents are
                // read again after any change, so the grid and the tree show each field with
                // the type the server stored.
                let rows_changed = edits.pending.count().deleted > 0;
                edits.reload = edits.pending.new_rows() > 0
                    || (rows_changed && edits.key_view().is_some())
                    || edits.documents().is_some();
                edits.pending.apply_to(&mut statement.rows);
                edits.error = None;
            }
            // Changes made one by one stay made up to the one that failed, so what is pending
            // no longer says what is left to do: the value is read again instead.
            Err(failure) if one_by_one && failure.statement.is_some_and(|index| index > 0) => {
                edits.pending.clear();
                edits.reload = true;
                edits.error = Some(failure.message.clone());
            }
            Err(failure) => {
                let row = failure
                    .statement
                    .and_then(|index| submitted.get(index))
                    .map(|(row, _)| *row);
                edits.error = Some(match row {
                    Some(row) => format!("{}: {}", row.describe(), failure.message),
                    None => failure.message.clone(),
                });
            }
        }
    }

    /// Running the shown statement again, once rows were added to its result, so they show
    /// with what the database put in the columns left to their defaults.
    pub fn take_reload(&mut self) -> Option<SubmittedRun> {
        let dialect = self.dialect();
        let run = self.run.as_mut()?;
        let statement = run
            .statements
            .iter_mut()
            .find(|statement| statement.edits.reload)?;
        statement.edits.reload = false;
        let statement = statement.resubmission(dialect).ok()?;
        Some(SubmittedRun {
            statements: vec![statement],
            purpose: RunPurpose::Run,
            parameters: self.parameters.clone(),
            origin: RunOrigin::Console,
            destination: RunDestination::Grid,
            // The statement ran already, as the user allowed; reading it again after edits
            // isn't asked about twice.
            scans: ScanApproval::Allow,
        })
    }

    /// Runs the shown statement again so that the database sorts and filters its rows as the
    /// grid now asks, or, when the grid no longer asks, as typed. `Err` says why it can't be.
    pub fn rearrange(&mut self) -> Result<SubmittedRun, String> {
        let dialect = self.dialect();
        let statement = self
            .run
            .as_ref()
            .and_then(|run| run.statements.get(run.selected))
            .ok_or_else(|| "There is no result to sort and filter".to_owned())?;
        Ok(SubmittedRun {
            statements: vec![statement.resubmission(dialect)?],
            purpose: RunPurpose::Run,
            parameters: self.parameters.clone(),
            origin: RunOrigin::Console,
            destination: RunDestination::Grid,
            scans: ScanApproval::Ask,
        })
    }

    /// The selected cells as `copy` turns them into text, with the export dialog's `options`.
    /// `None` without a selection; an error says why they can't be copied that way, and is
    /// shown where an export's would be.
    pub fn copy(&mut self, copy: ClipboardCopy, options: &ExportOptions) -> Option<String> {
        let run = self.run.as_ref()?;
        let statement = run.statements.get(run.selected)?;
        let selection = statement.selection(self.dialect())?;
        match copy.text(&selection, options) {
            Ok(text) => Some(text),
            Err(reason) => {
                self.export_status = Some(Err(reason.to_owned()));
                None
            }
        }
    }

    /// The one statement at the cursor, or selected, as Run to file runs it; `None` if there
    /// is no statement there or the selection holds several.
    pub fn statement_to_file(&self) -> Option<SubmittedRun> {
        let statements = self.statements_to_run(RunScope::Current);
        if statements.len() != 1 {
            return None;
        }
        Some(SubmittedRun {
            statements,
            purpose: RunPurpose::Run,
            parameters: dbrover_sql::ParameterValues::default(),
            origin: RunOrigin::Console,
            destination: RunDestination::Grid,
            scans: ScanApproval::Ask,
        })
    }

    /// Runs `sql` straight into the file at `path`, every row it returns written as they come,
    /// without loading the grid; the results panel shows how it goes.
    pub fn run_to_file(
        &mut self,
        run: RunId,
        sql: String,
        parameters: dbrover_sql::ParameterValues,
        path: PathBuf,
        spec: ExportSpec,
    ) -> Option<Command> {
        self.run = None;
        self.to_file = Some(path.clone());
        self.exporting = Some((run, 0));
        self.export_status = None;
        self.last_export = Some(spec.clone());
        self.remember_parameters(&parameters);
        Some(Command::RunToFile {
            console: self.id,
            data_source: self.data_source(),
            export: ExportAll {
                run,
                sql,
                parameters,
                path,
                spec,
            },
        })
    }

    /// What the export dialog shows of the selected statement's rows. `None` if it returns
    /// none.
    pub fn export_preview(&self) -> Option<ExportPreview> {
        let dialect = self.dialect();
        let run = self.run.as_ref()?;
        let statement = run.statements.get(run.selected)?;
        if !statement.returns_rows() {
            return None;
        }
        let shown = statement.grid.shown_indices(&statement.rows);
        let sample: Vec<usize> = shown.iter().copied().take(PREVIEW_ROWS).collect();
        Some(ExportPreview {
            columns: statement
                .columns
                .iter()
                .map(|column| ExportColumn::typed_by(column, dialect))
                .collect(),
            loaded: shown.len(),
            sample: sample
                .iter()
                .filter_map(|&index| statement.rows.get(index).cloned())
                .collect(),
            document_sample: statement.document_rows(&sample),
            source: statement.row_source(dialect),
        })
    }

    /// The export of the rows the grid shows for the selected statement, in the order and
    /// with the filter it shows them. `None` if there are no rows to export.
    pub fn export(&mut self, spec: ExportSpec, path: PathBuf) -> Option<Command> {
        let run = self.run.as_ref()?;
        let statement = run.statements.get(run.selected)?;
        if !statement.returns_rows() {
            return None;
        }
        let (columns, rows) = if spec.writes_documents() {
            let shown = statement.grid.shown_indices(&statement.rows);
            (
                vec![ExportColumn::text(DOCUMENT_COLUMN)],
                statement.document_rows(&shown),
            )
        } else {
            let columns = statement
                .columns
                .iter()
                .map(|column| ExportColumn::typed_by(column, self.dialect()))
                .collect();
            (columns, statement.grid.shown_rows(&statement.rows))
        };
        self.last_export = Some(spec.clone());
        Some(Command::ExportRows {
            console: self.id,
            export: Export {
                path,
                spec,
                columns,
                rows,
            },
        })
    }

    /// What a finished export says: how many rows went where, and into how many sheets when
    /// they filled more than one.
    fn exported(&self, rows: usize, path: &Path) -> String {
        let sheets = self
            .last_export
            .as_ref()
            .and_then(|spec| spec.sheets(rows))
            .filter(|&sheets| sheets > 1);
        match sheets {
            Some(sheets) => format!(
                "Exported {rows} rows to {}, in {sheets} sheets: an Excel sheet holds \
                 1,048,576 rows",
                path.display()
            ),
            None => format!("Exported {rows} rows to {}", path.display()),
        }
    }

    /// The export of every row of the selected statement: it runs again, as the grid shows it
    /// (wrapped, if the database sorts and filters it), and the engine writes its rows as they
    /// come. `None` if the statement returns no rows.
    pub fn export_all(&mut self, run: RunId, spec: ExportSpec, path: PathBuf) -> Option<Command> {
        let shown = self.run.as_ref()?;
        let statement = shown.statements.get(shown.selected)?;
        // Documents are exported as loaded: the engine would write a cursor's documents as rows
        // of one cell, before any of their columns is known (docs/nosql.md, N0b's decisions).
        if statement.columns.is_empty() || statement.documents.is_some() {
            return None;
        }
        let sql = statement.resubmission(self.dialect()).ok()?.sql;
        self.exporting = Some((run, 0));
        self.export_status = None;
        self.last_export = Some(spec.clone());
        Some(Command::ExportAllRows {
            console: self.id,
            export: ExportAll {
                run,
                sql,
                parameters: self.parameters.clone(),
                path,
                spec,
            },
        })
    }

    /// Opens the completion list; it fills on the next [`QueryConsole::refresh_completion`].
    pub fn open_completion(&mut self, explicit: bool) {
        match &mut self.completion {
            Some(open) => open.explicit |= explicit,
            None => {
                self.completion = Some(Completion {
                    explicit,
                    candidates: Vec::new(),
                    replace: self.selection.end..self.selection.end,
                    selected: 0,
                    rect: None,
                    loading: false,
                    snippets: Vec::new(),
                });
            }
        }
    }

    pub fn close_completion(&mut self) {
        self.completion = None;
    }

    /// Recomputes the open completion list for the cursor, closing it where nothing applies
    /// (a selection, inside a string, or nothing to suggest unless asked). Returns the catalog
    /// levels that should be loaded to suggest more.
    pub fn refresh_completion(
        &mut self,
        catalog: Option<&TreeCatalog<'_>>,
        recent: &completion::RecentCompletions,
        snippets: &[&Snippet],
    ) -> Vec<dbrover_core::CatalogPath> {
        let Some(open) = &self.completion else {
            return Vec::new();
        };
        let (start, end) = ordered(&self.selection);
        let point = (start == end)
            .then(|| completion::completion_point(&self.text, end, self.language))
            .flatten();
        let Some(point) = point else {
            self.completion = None;
            return Vec::new();
        };
        // The statement the prefix belongs to; none past the end of the one before it, where
        // a new statement is starting.
        let (statement, statement_start) = self
            .current_statement()
            .filter(|range| point.replace.start <= range.end)
            .and_then(|range| Some((self.text.get(range.clone())?, range.start)))
            .unwrap_or_default();
        let (references, mut candidates) = match self.language.dialect() {
            Some(dialect) => (
                completion::table_references(statement, dialect),
                completion::candidates(&completion::CompletionInput {
                    point: &point,
                    statement,
                    prefix_start: point.replace.start.saturating_sub(statement_start),
                    dialect,
                    catalog: match catalog {
                        Some(catalog) => catalog,
                        None => &completion::NoCatalog,
                    },
                    recent,
                }),
            ),
            None => {
                let names = catalog
                    .map(|catalog| catalog.top_level_names(self.language.named_objects()))
                    .unwrap_or_default();
                (
                    Vec::new(),
                    completion::word_candidates(&point.prefix, self.language, &names, recent),
                )
            }
        };
        let snippets = if point.qualifier.is_none() {
            snippet::offer(&mut candidates, snippets, &point.prefix);
            snippets
                .iter()
                .map(|snippet| (snippet.trigger.clone(), snippet.body.clone()))
                .collect()
        } else {
            Vec::new()
        };
        let needed = catalog
            .map(|catalog| match self.language.named_objects() {
                [] => catalog.missing(&references),
                _ => catalog.missing_top_level(),
            })
            .unwrap_or_default();
        // While the catalog loads, an empty list may yet fill; it shows "Loading…".
        if candidates.is_empty() && !open.explicit && !needed.is_pending() {
            self.completion = None;
            return Vec::new();
        }
        let selected = open.selected.min(candidates.len().saturating_sub(1));
        let rect = open.rect;
        let explicit = open.explicit;
        self.completion = Some(Completion {
            explicit,
            candidates,
            replace: point.replace,
            selected,
            rect,
            loading: needed.is_pending(),
            snippets,
        });
        needed.unrequested
    }

    /// Moves the highlight in the completion list, wrapping around.
    pub fn move_completion(&mut self, down: bool) {
        if let Some(open) = &mut self.completion
            && !open.candidates.is_empty()
        {
            let count = open.candidates.len();
            open.selected = if down {
                (open.selected + 1) % count
            } else {
                (open.selected + count - 1) % count
            };
        }
    }

    /// Replaces the typed prefix with the highlighted candidate and closes the list.
    pub fn accept_completion(&mut self, now: Instant) {
        let Some(open) = self.completion.take() else {
            return;
        };
        let Some(candidate) = open.candidates.get(open.selected) else {
            return;
        };
        if self.text.get(open.replace.clone()).is_none() {
            return;
        }
        self.text
            .replace_range(open.replace.clone(), &candidate.insert);
        let cursor = open.replace.start + candidate.insert.len() - candidate.cursor_back;
        self.selection = cursor..cursor;
        self.pending_cursor = Some(cursor);
        self.accepted_completion = Some(candidate.clone());
        if candidate.kind == completion::CandidateKind::Snippet
            && let Some((_, body)) = open
                .snippets
                .iter()
                .find(|(trigger, _)| *trigger == candidate.label)
        {
            self.start_snippet(body, open.replace.start);
        }
        self.edited(now);
    }

    /// Selects the first field of a snippet just inserted at `at`, or places the cursor where
    /// it ends if it has none.
    fn start_snippet(&mut self, body: &SnippetBody, at: usize) {
        self.snippet_fields = snippet::SnippetFields::start(body, at, self.text.len());
        let first = match &self.snippet_fields {
            Some(fields) => fields.current(),
            None => {
                let end = snippet::SnippetFields::end_of(body, at);
                end..end
            }
        };
        self.select(first);
    }

    /// Selects `range` in the editor, focusing it.
    fn select(&mut self, range: Range<usize>) {
        self.selection = range.clone();
        self.pending_cursor = Some(range.end);
        self.pending_selection = Some(range);
    }

    /// Whether Tab moves through a snippet's fields right now: there are fields, and no
    /// completion list with something to pick, which takes Tab first.
    pub fn in_snippet(&self) -> bool {
        let picking = self
            .completion
            .as_ref()
            .is_some_and(|open| !open.candidates.is_empty());
        self.snippet_fields.is_some() && !picking
    }

    /// Selects the snippet's next field, or the previous one. Past the last, at the end, the
    /// fields are done and Tab is the editor's again.
    pub fn next_snippet_field(&mut self, forward: bool) {
        let Some(fields) = &mut self.snippet_fields else {
            return;
        };
        let next = fields.advance(forward);
        if next.is_none() || fields.at_end() {
            self.snippet_fields = None;
        }
        if let Some(range) = next {
            self.select(range);
        }
    }

    pub fn leave_snippet(&mut self) {
        self.snippet_fields = None;
    }

    /// Follows what the editor did this frame: typing in the current field moves the fields
    /// after it; typing or clicking anywhere else leaves the snippet.
    pub fn follow_snippet(&mut self, changed: bool, clicked: bool) {
        let Some(fields) = &mut self.snippet_fields else {
            return;
        };
        let still_in = if changed {
            fields.follow_edit(self.text.len(), self.selection.end)
        } else {
            !clicked || fields.holds(&self.selection)
        };
        if !still_in {
            self.snippet_fields = None;
        }
    }

    /// The candidate accepted since this was last asked, for the app to remember as recent.
    pub fn take_accepted_completion(&mut self) -> Option<completion::Candidate> {
        self.accepted_completion.take()
    }

    /// Puts `sql` at the cursor, replacing the selection, as a statement on a line of its own,
    /// and moves the cursor past it.
    pub fn insert_statement(&mut self, sql: &str, now: Instant) {
        let (start, end) = ordered(&self.selection);
        let sql = sql.trim_end();
        let mut inserted = String::new();
        if !self.text[..start].is_empty() && !self.text[..start].ends_with('\n') {
            inserted.push('\n');
        }
        inserted.push_str(sql);
        if !sql.ends_with(';') {
            inserted.push(';');
        }
        inserted.push('\n');
        self.text.replace_range(start..end, &inserted);
        let cursor = start + inserted.len();
        self.selection = cursor..cursor;
        self.pending_cursor = Some(cursor);
        self.edited(now);
    }

    /// Formats what `scope` covers, keeping the cursor on the token it was on. Leaves the
    /// text alone (and says why) if formatting would change what it does.
    pub fn format(&mut self, scope: FormatScope, options: &FormatOptions, now: Instant) {
        let Some(dialect) = self.dialect() else {
            self.format_error = Some(format!(
                "DbRover formats SQL, and this console speaks {}",
                self.language.display_name()
            ));
            return;
        };
        let ranges = self.ranges_to_format(scope);
        let formatted = match format_ranges(&self.text, &ranges, dialect, options) {
            Ok(formatted) => formatted,
            Err(error) => {
                self.format_error = Some(error.to_string());
                return;
            }
        };
        self.format_error = None;
        if formatted.text() == self.text {
            return;
        }
        let cursor = formatted.map_offset(self.selection.end);
        self.replaced_text = Some(std::mem::replace(&mut self.text, formatted.into_text()));
        self.selection = cursor..cursor;
        self.pending_cursor = Some(cursor);
        self.edited(now);
    }

    /// The statements `scope` formats. A selection is narrowed to the tokens wholly inside
    /// it, and split where statements end, so a selection that starts or ends inside a
    /// string never formats half of it.
    fn ranges_to_format(&self, scope: FormatScope) -> Vec<Range<usize>> {
        let statements = self.language.split(&self.text);
        let (start, end) = ordered(&self.selection);
        match scope {
            FormatScope::All => statements,
            FormatScope::Current if start == end => command_at(&statements, end)
                .map(|index| statements[index].clone())
                .into_iter()
                .collect(),
            FormatScope::Current => {
                let inside: Vec<Range<usize>> = self
                    .language
                    .tokens(&self.text)
                    .into_iter()
                    .filter(|token| {
                        !token.kind.is_trivia()
                            && token.range.start >= start
                            && token.range.end <= end
                    })
                    .map(|token| token.range)
                    .collect();
                let (Some(first), Some(last)) = (inside.first(), inside.last()) else {
                    return Vec::new();
                };
                statements
                    .into_iter()
                    .map(|statement| statement.start.max(first.start)..statement.end.min(last.end))
                    .filter(|range| range.start < range.end)
                    .collect()
            }
        }
    }

    /// Checks the text for problems if it is due; see [`problems::Problems::check`].
    pub fn check_problems(
        &mut self,
        catalog: Option<&TreeCatalog<'_>>,
        options: &LintOptions,
        now: Instant,
    ) -> problems::CheckOutcome {
        let can_ask_database = matches!(self.session, SessionState::Connected(_))
            && self.cancellable_run().is_none()
            && self.transaction == TransactionStatus::Idle;
        let Some(dialect) = self.dialect() else {
            return problems::CheckOutcome::default();
        };
        let input = problems::CheckInput {
            text: &self.text,
            dialect,
            catalog: match catalog {
                Some(catalog) => catalog,
                None => &completion::NoCatalog,
            },
            options,
            can_ask_database,
        };
        self.problems.check(&input, now)
    }

    /// Moves the cursor to the next problem after it (or the previous one before it),
    /// wrapping around.
    pub fn jump_to_problem(&mut self, forward: bool) {
        let (start, end) = ordered(&self.selection);
        let from = if forward { end } else { start };
        if let Some(problem) = self.problems.next_from(from, forward) {
            let at = problem.range.start;
            self.selection = at..at;
            self.pending_cursor = Some(at);
        }
    }

    /// Records that the user changed the text at `now`.
    pub fn edited(&mut self, now: Instant) {
        self.error_marker = None;
        self.format_error = None;
        self.problems.edited(now);
        if let Storage::Config(file) = &mut self.storage {
            file.keep = true;
            file.unsaved_since.get_or_insert(now);
        }
    }

    /// The save to send if the text has been unsaved for at least `delay` (or at all, for a
    /// zero delay), marking it saved. Otherwise how long until it would be due. A console
    /// showing a `.sql` file is only saved when asked, by [`QueryConsole::save_file`].
    pub fn take_save(
        &mut self,
        now: Instant,
        delay: Duration,
    ) -> Result<Command, Option<Duration>> {
        let Storage::Config(file) = &mut self.storage else {
            return Err(None);
        };
        let since = file.unsaved_since.ok_or(None)?;
        let waited = now.saturating_duration_since(since);
        if waited < delay {
            return Err(Some(delay - waited));
        }
        file.unsaved_since = None;
        file.save_error = None;
        Ok(Command::SaveConsole {
            key: self.key.clone(),
            text: self.text.clone(),
        })
    }

    /// What to do with the console's file in the config folder when the tab closes: write the
    /// last edits, or remove a file nobody typed anything into. Nothing for a `.sql` file,
    /// which the user saves or not.
    pub fn file_on_close(&mut self) -> Option<Command> {
        let Storage::Config(file) = &self.storage else {
            return None;
        };
        if !file.keep || self.text.trim().is_empty() {
            return Some(Command::DeleteConsoleFile(self.key.clone()));
        }
        self.take_save(Instant::now(), Duration::ZERO).ok()
    }

    /// Whether the console's file in the config folder is kept once the tab closes, to be
    /// reopened from the explorer.
    pub fn keeps_file(&self) -> bool {
        match &self.storage {
            Storage::Config(file) => file.keep && !self.text.trim().is_empty(),
            Storage::File(_) => false,
        }
    }

    pub fn save_error(&self) -> Option<&str> {
        match &self.storage {
            Storage::Config(file) => file.save_error.as_deref(),
            Storage::File(file) => file.save_error.as_deref(),
        }
    }

    /// The `.sql` file the console shows, if it shows one.
    pub fn file_path(&self) -> Option<&Path> {
        match &self.storage {
            Storage::File(file) => Some(file.path()),
            Storage::Config(_) => None,
        }
    }

    /// Whether the console shows a `.sql` file and has changes not written to it.
    pub fn has_unsaved_file(&self) -> bool {
        matches!(&self.storage, Storage::File(file) if file.is_unsaved(&self.text))
    }

    /// What another program did to the console's `.sql` file, if anything.
    pub fn disk(&self) -> Option<&Disk> {
        match &self.storage {
            Storage::File(file) if file.disk != Disk::Unchanged => Some(&file.disk),
            _ => None,
        }
    }

    /// Writes the text to the console's `.sql` file. `None` if it shows none, and Save as must
    /// ask where.
    pub fn save_file(&mut self) -> Option<Command> {
        let Storage::File(file) = &mut self.storage else {
            return None;
        };
        file.saving(self.text.clone());
        Some(Command::SaveSqlFile {
            console: self.key.clone(),
            path: file.path().to_owned(),
            text: self.text.clone(),
        })
    }

    /// Writes the text to `path`, and shows that file from now on. A console from the config
    /// folder stops saving itself there; its file is removed once this one is written.
    pub fn save_file_as(&mut self, path: PathBuf) -> Command {
        let mut file = match &self.storage {
            Storage::Config(_) => SqlFile::saved_from_config(path.clone()),
            Storage::File(_) => SqlFile::new(
                path.clone(),
                SqlFileContents {
                    text: String::new(),
                    modified: None,
                },
            ),
        };
        file.saving(self.text.clone());
        self.title = file.file_name();
        self.storage = Storage::File(file);
        Command::SaveSqlFile {
            console: self.key.clone(),
            path,
            text: self.text.clone(),
        }
    }

    /// Takes in how a save of the console's `.sql` file went. Returns the removal of its old
    /// file in the config folder, once there is nothing left there that isn't written.
    pub fn file_saved(&mut self, result: &Result<Option<SystemTime>, String>) -> Option<Command> {
        let Storage::File(file) = &mut self.storage else {
            return None;
        };
        file.saved(result)
            .then(|| Command::DeleteConsoleFile(self.key.clone()))
    }

    /// Takes in what the console's `.sql` file holds after another program changed it.
    pub fn file_changed(&mut self, contents: Option<&SqlFileContents>) {
        let Storage::File(file) = &mut self.storage else {
            return;
        };
        if let Reload::Replaced(text) = file.changed_on_disk(contents, &self.text) {
            self.replace_text(text);
        }
    }

    /// Drops the unsaved changes for what the `.sql` file now holds.
    pub fn reload_file(&mut self) {
        if let Storage::File(file) = &mut self.storage
            && let Some(text) = file.reload()
        {
            self.replace_text(text);
        }
    }

    /// Keeps the text over what the `.sql` file now holds, until it is saved over it.
    pub fn keep_mine(&mut self) {
        if let Storage::File(file) = &mut self.storage {
            file.keep_mine();
        }
    }

    /// The `.sql` file to look at for changes, and when it was last modified.
    pub fn watched_file(&self) -> Option<WatchedFile> {
        match &self.storage {
            Storage::File(file) => Some(file.watched()),
            Storage::Config(_) => None,
        }
    }

    /// The run a key bar action makes: `command`, then `then`'s command reading the key again, in
    /// place of the shown statement. When the key was renamed, the console's text reads the new
    /// key too, so running it again shows the key as it now is.
    pub fn key_command_run(&mut self, command: String, then: &KeyView) -> Option<SubmittedRun> {
        let run = self.run.as_ref()?;
        let shown = run.statements.get(run.selected)?;
        let read = then.read();
        let mut range = shown.range.clone();
        if shown.sql != read && self.text.get(range.clone()) == Some(shown.sql.as_str()) {
            let mut text = self.text.clone();
            text.replace_range(range.clone(), &read);
            range = range.start..range.start + read.len();
            self.replace_text(text);
        }
        let statement = |sql: String| SubmittedStatement {
            sql,
            range: range.clone(),
            prefix_len: 0,
            in_database: None,
            plan: None,
        };
        Some(SubmittedRun {
            statements: vec![statement(command), statement(read)],
            purpose: RunPurpose::Run,
            parameters: self.parameters.clone(),
            origin: RunOrigin::Console,
            destination: RunDestination::Grid,
            scans: ScanApproval::Ask,
        })
    }

    /// Replaces the whole text from outside the editor, as one undo step, keeping the cursor
    /// where it was if the new text is long enough.
    fn replace_text(&mut self, text: String) {
        let (_, end) = ordered(&self.selection);
        let mut cursor = end.min(text.len());
        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        self.replaced_text = Some(std::mem::replace(&mut self.text, text));
        self.selection = cursor..cursor;
        self.pending_cursor = Some(cursor);
        self.error_marker = None;
        self.problems.edited(Instant::now());
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// The tab's title: a `.sql` file's name ends with `•` while it has unsaved changes.
    pub fn tab_title(&self) -> String {
        if self.has_unsaved_file() {
            format!("{} \u{2022}", self.title)
        } else {
            self.title.clone()
        }
    }

    pub fn language(&self) -> QueryLanguage {
        self.language
    }

    /// The SQL dialect, for features with no meaning outside SQL: formatting, the linter,
    /// parameters, explain, and what the grid does with a result's rows. They are hidden
    /// without one.
    pub fn dialect(&self) -> Option<Dialect> {
        self.language.dialect()
    }

    pub fn transaction_mode(&self) -> TransactionMode {
        self.transactions
    }

    /// Whether the console's connection has uncommitted work that closing it would roll
    /// back.
    pub fn has_open_transaction(&self) -> bool {
        self.transaction != TransactionStatus::Idle
    }

    pub fn is_running(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| run.state == RunState::Running)
    }

    #[cfg(test)]
    pub fn session(&self) -> &SessionState {
        &self.session
    }

    /// A name for the shown chart's file, such as `total by month`.
    pub fn chart_file_name(&self) -> Option<String> {
        Some(self.shown_chart_tab()?.file_name())
    }

    /// The command that writes the shown chart to `path` as `format`, or `None` while no chart
    /// is drawn. The toolbar says how it went once it is written.
    pub fn export_chart(&mut self, format: PictureFormat, path: &Path) -> Option<Command> {
        let picture = self.shown_chart_tab()?.picture(format)?;
        self.export_status = None;
        self.chart_export = Some(path.to_owned());
        Some(Command::ExportPicture {
            path: path.to_owned(),
            picture,
        })
    }

    fn shown_chart_tab(&self) -> Option<&chart::ChartTab> {
        let run = self.run.as_ref()?;
        let chart = &run.statements.get(run.selected)?.chart;
        chart.is_shown().then_some(chart)
    }

    /// The shown result's chart as last laid out, if the chart is shown.
    #[cfg(test)]
    pub fn shown_chart(&self) -> Option<&dbrover_chart::Drawing> {
        self.shown_chart_tab()?.drawing()
    }

    /// The plan the shown result was read as, if it was an explain's.
    #[cfg(test)]
    pub fn shown_plan(&self) -> Option<&dbrover_plan::Plan> {
        let run = self.run.as_ref()?;
        run.statements.get(run.selected)?.plan.as_ref()?.plan()
    }

    /// Whether the latest run finished without a failure.
    #[cfg(test)]
    pub fn last_run_completed(&self) -> bool {
        self.run.as_ref().is_some_and(|run| {
            matches!(
                run.state,
                RunState::Finished {
                    outcome: RunOutcome::Completed,
                    ..
                }
            )
        })
    }

    /// The statement ⌘↵ would run, to shade in the editor: the one at the cursor, unless text
    /// is selected (the selection is what runs then, and is already highlighted).
    /// Whether the shown result has cells selected, without copying them.
    pub fn has_selected_cells(&self) -> bool {
        self.run
            .as_ref()
            .and_then(|run| run.statements.get(run.selected))
            .is_some_and(|statement| statement.grid.selected_rows().is_some())
    }

    /// The grid's selected cells in the shown result, for the keys and menu that run custom
    /// actions.
    pub fn selected_cells(&self) -> Option<GridCells> {
        let run = self.run.as_ref()?;
        run.statements.get(run.selected)?.selected_cells()
    }

    pub fn current_statement(&self) -> Option<Range<usize>> {
        let (start, end) = ordered(&self.selection);
        if start != end {
            return None;
        }
        let ranges = self.language.split(&self.text);
        command_at(&ranges, end).map(|index| ranges[index].clone())
    }

    /// What `scope` would run right now. Empty if there is nothing but whitespace and
    /// comments.
    pub fn statements_to_run(&self, scope: RunScope) -> Vec<SubmittedStatement> {
        let (start, end) = ordered(&self.selection);
        let submit = |offset: usize, ranges: Vec<Range<usize>>, source: &str| {
            ranges
                .into_iter()
                .map(|range| SubmittedStatement {
                    sql: source[range.clone()].to_owned(),
                    range: range.start + offset..range.end + offset,
                    prefix_len: 0,
                    in_database: None,
                    plan: None,
                })
                .collect::<Vec<_>>()
        };
        match scope {
            RunScope::Current if start != end => {
                let selected = &self.text[start..end];
                submit(start, self.language.split(selected), selected)
            }
            RunScope::Current => {
                let ranges = self.language.split(&self.text);
                let current = command_at(&ranges, end).map(|index| ranges[index].clone());
                submit(0, current.into_iter().collect(), &self.text)
            }
            RunScope::All => submit(0, self.language.split(&self.text), &self.text),
            RunScope::Explain(depth) => {
                let server = self.server();
                self.statements_to_run(RunScope::Current)
                    .into_iter()
                    .take(1)
                    .filter_map(|statement| {
                        let explained = self.language.explain(&statement.sql, server, depth)?;
                        Some(SubmittedStatement {
                            sql: explained.text,
                            prefix_len: explained.prefix_len,
                            plan: explained.plan,
                            ..statement
                        })
                    })
                    .collect()
            }
        }
    }

    pub fn begin_run(&mut self, run: RunId, statements: &[SubmittedStatement]) {
        self.to_file = None;
        self.error_marker = None;
        self.transaction_ended = None;
        let chart = self
            .run
            .as_ref()
            .and_then(|run| run.statements.get(run.selected))
            .map(|statement| statement.chart.carried_over())
            .unwrap_or_default();
        self.run = Some(RunView {
            id: run,
            statements: statements
                .iter()
                .map(|statement| {
                    let (sql, grid) = match &statement.in_database {
                        Some(arranged) => (
                            arranged.typed.clone(),
                            grid::GridView::in_database(arranged.sort, arranged.filter.clone()),
                        ),
                        None => (statement.sql.clone(), grid::GridView::default()),
                    };
                    StatementView {
                        sql,
                        range: statement.range.clone(),
                        prefix_len: statement.prefix_len,
                        columns: Vec::new(),
                        rows: Vec::new(),
                        more: false,
                        fetching: false,
                        result: None,
                        grid,
                        edits: edit::EditSession::default(),
                        key_bar: results::KeyBar::default(),
                        describing: None,
                        links: keys::KeyLinks::default(),
                        wrapped: statement.in_database.is_some(),
                        documents: None,
                        plan: statement.plan.map(plan::PlanView::new),
                        chart: chart.carried_over(),
                    }
                })
                .collect(),
            state: RunState::Running,
            selected: 0,
        });
    }

    /// The shown plan's statement, to explain again by running it: Explain analyse from the
    /// plan's bar. `None` unless a plan is shown and the language can analyse it.
    pub fn analyse_shown_plan(&self) -> Option<SubmittedRun> {
        let run = self.run.as_ref()?;
        let statement = run.statements.get(run.selected)?;
        statement.plan.as_ref()?;
        let typed = statement.sql.get(statement.prefix_len..)?;
        let explained = self
            .language
            .explain(typed, self.server(), ExplainDepth::Analysed)?;
        Some(SubmittedRun {
            statements: vec![SubmittedStatement {
                sql: explained.text,
                range: statement.range.clone(),
                prefix_len: explained.prefix_len,
                in_database: None,
                plan: explained.plan,
            }],
            purpose: RunPurpose::ExplainAnalyse,
            parameters: ParameterValues::default(),
            origin: RunOrigin::Console,
            destination: RunDestination::Grid,
            scans: ScanApproval::Ask,
        })
    }

    /// The server the console is connected to, which decides how some dialects explain.
    fn server(&self) -> Option<&dbrover_core::ServerInfo> {
        match &self.session {
            SessionState::Connected(server) => Some(server),
            _ => None,
        }
    }

    /// The run to cancel: one still running, or one whose rows are still open.
    pub fn cancellable_run(&self) -> Option<RunId> {
        let run = self.run.as_ref()?;
        let open_rows = run.statements.iter().any(|statement| statement.more);
        (run.state == RunState::Running || open_rows).then_some(run.id)
    }

    /// Marks the next page as requested. `None` if there is nothing more, or a fetch is
    /// already in flight.
    pub fn request_more(&mut self) -> Option<RunId> {
        let run = self.run.as_mut()?;
        let statement = run
            .statements
            .iter_mut()
            .find(|statement| statement.more && !statement.fetching)?;
        statement.fetching = true;
        Some(run.id)
    }

    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::ConsoleSessionChanged { console, state } if *console == self.id => {
                self.session_changed(state);
            }
            Event::TransactionChanged { console, status } if *console == self.id => {
                self.transaction = *status;
                // Statements skipped inside the transaction can be asked about now.
                if *status == TransactionStatus::Idle {
                    self.problems.forget_database(Instant::now());
                }
            }
            Event::TransactionEnded { console, result } if *console == self.id => {
                self.transaction_ended = Some(result.clone());
            }
            Event::ExportProgress {
                exporter: Exporter::Console(console),
                rows,
            } if *console == self.id => {
                if let Some((_, written)) = &mut self.exporting {
                    *written = *rows;
                }
            }
            Event::ExportFinished {
                exporter: Exporter::Console(console),
                path,
                result,
            } if *console == self.id => {
                self.exporting = None;
                let status = result
                    .as_ref()
                    .map(|&rows| self.exported(rows, path))
                    .map_err(Clone::clone);
                self.export_status = Some(status);
            }
            Event::PictureExported { path, result } if self.chart_export.as_ref() == Some(path) => {
                self.chart_export = None;
                self.export_status = Some(
                    result
                        .clone()
                        .map(|()| format!("Exported to {}", path.display())),
                );
            }
            Event::TableDescribed {
                data_source,
                table,
                result,
            } if *data_source == self.data_source() => self.table_described(table, result),
            Event::EditsApplied { console, result } if *console == self.id => {
                self.edits_applied(result);
            }
            Event::StatementsValidated { console, results } if *console == self.id => {
                self.problems.database_answered(results, Instant::now());
            }
            // Tables the checks didn't know may have loaded.
            Event::ChildrenLoaded { id, .. } if *id == self.data_source() => {
                self.problems.recheck(Instant::now());
            }
            Event::ConsoleSaveFailed { key, error } if *key == self.key => {
                // Shown until the next save is sent; that one writes the whole text again.
                if let Storage::Config(file) = &mut self.storage {
                    file.save_error = Some(error.clone());
                }
            }
            Event::StatementStarted {
                run,
                statement,
                columns,
                shape,
            } => {
                if let Some(view) = self.current_run_mut(*run)
                    && let Some(statement_view) = view.statements.get_mut(*statement)
                {
                    match shape {
                        ResultShape::Rows => statement_view.columns = columns.clone(),
                        // The documents' own fields are the columns, as they arrive.
                        ResultShape::Documents => {
                            statement_view.documents = Some(documents::DocumentResult::default());
                        }
                    }
                    view.selected = *statement;
                }
            }
            Event::RowsFetched {
                run,
                statement,
                rows,
                more,
            } => {
                if let Some(view) = self.current_run_mut(*run)
                    && let Some(statement_view) = view.statements.get_mut(*statement)
                {
                    statement_view.add_rows(rows);
                    statement_view.more = *more;
                    statement_view.fetching = false;
                }
            }
            Event::StatementFinished {
                run,
                statement,
                result,
            } => self.statement_finished(*run, *statement, result),
            Event::RunFinished {
                run,
                outcome,
                elapsed,
            } => {
                if let Some(view) = self.current_run_mut(*run) {
                    view.state = RunState::Finished {
                        outcome: *outcome,
                        elapsed: *elapsed,
                    };
                    // The run may have created or dropped tables, or ended a transaction.
                    self.problems.forget_database(Instant::now());
                }
            }
            _ => {}
        }
    }

    /// The statements of `run`, this console's latest, from the one at `from` on: what runs
    /// again once the user allows the scan its data source's cost guard held back there.
    pub fn rest_of_run(&self, run: RunId, from: usize) -> Option<SubmittedRun> {
        let view = self.run.as_ref().filter(|view| view.id == run)?;
        let statements = view
            .statements
            .get(from..)?
            .iter()
            .map(|statement| SubmittedStatement {
                sql: statement.sql.clone(),
                range: statement.range.clone(),
                prefix_len: statement.prefix_len,
                in_database: None,
                plan: None,
            })
            .collect();
        Some(SubmittedRun {
            statements,
            purpose: RunPurpose::Run,
            parameters: ParameterValues::default(),
            origin: RunOrigin::Console,
            destination: RunDestination::Grid,
            scans: ScanApproval::Ask,
        })
    }

    /// The latest run, if it is `run`: events for older runs are stale and ignored.
    fn current_run_mut(&mut self, run: RunId) -> Option<&mut RunView> {
        self.run.as_mut().filter(|view| view.id == run)
    }

    fn statement_finished(
        &mut self,
        run: RunId,
        statement: usize,
        result: &Result<StatementSummary, StatementFailure>,
    ) {
        let Some(view) = self.run.as_mut().filter(|view| view.id == run) else {
            return;
        };
        let Some(statement_view) = view.statements.get_mut(statement) else {
            return;
        };
        statement_view.result = Some(result.clone());
        statement_view.more = false;
        statement_view.fetching = false;
        if let (Ok(_), Some(plan)) = (result, &mut statement_view.plan) {
            plan.read(&statement_view.rows);
        }
        if let Err(StatementFailure {
            position: Some(position),
            ..
        }) = result
        {
            // A position inside an added prefix points at no text; mark the statement's start.
            // One past the statement, in what a wrapper added after it, marks its end.
            let offset = (statement_view.range.start
                + position.saturating_sub(statement_view.prefix_len))
            .min(statement_view.range.end);
            self.error_marker = Some(highlight::error_marker(&self.text, self.language, offset));
        }
        if result.is_err() {
            view.selected = statement;
        }
    }

    /// A connection that fails or closes ends whatever the console was doing with it.
    fn session_changed(&mut self, state: &SessionState) {
        self.session = state.clone();
        if matches!(state, SessionState::Connected(_)) {
            self.problems.forget_database(Instant::now());
        }
        if matches!(
            state,
            SessionState::Disconnected | SessionState::Failed(_) | SessionState::Lost(_)
        ) {
            // The database rolls back whatever a closed connection left uncommitted.
            self.transaction = TransactionStatus::Idle;
        }
        let failure = match state {
            SessionState::Failed(error) | SessionState::Lost(error) => error.clone(),
            SessionState::Disconnected => "the connection closed".to_owned(),
            SessionState::Connecting | SessionState::Connected(_) => return,
        };
        let Some(view) = self.run.as_mut() else {
            return;
        };
        for statement in &mut view.statements {
            statement.more = false;
            statement.fetching = false;
        }
        if view.state == RunState::Running {
            if let Some(pending) = view
                .statements
                .iter_mut()
                .find(|statement| statement.result.is_none())
            {
                pending.result = Some(Err(StatementFailure {
                    message: failure,
                    position: None,
                }));
            }
            view.state = RunState::Finished {
                outcome: RunOutcome::Failed,
                elapsed: Duration::ZERO,
            };
        }
    }
}

/// A selection's ends in text order.
fn ordered(range: &Range<usize>) -> (usize, usize) {
    (range.start.min(range.end), range.start.max(range.end))
}

#[cfg(test)]
mod tests;
