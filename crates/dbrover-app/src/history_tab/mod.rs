//! The query history, a tab per data source opened from its consoles, to put a past statement
//! back into one: the data source's statements or every one's, searchable and filtered by
//! where statements came from and when. Each statement is marked with where it came from: a
//! console, the grid, an object helper, an import, an export, or an MCP client.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use dbrover_core::DataSourceId;
use dbrover_engine::{Command, Event};
use dbrover_export::{CsvOptions, ExportSpec};
use dbrover_store::{
    ConsoleKey, HistoryEntry, HistoryOutcome, HistoryQuery, HistorySource, HistorySourceKind,
};
use egui::{Label, RichText, ScrollArea, TextEdit, Ui};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::theme::Palette;
use crate::widgets;
use filters::{Period, Scope, kind_filter, period_filter, scope_filter};

mod detail;
mod filters;

/// The share of the tab's height the list keeps while an entry's details show below it.
const LIST_SHARE: f32 = 0.45;
const TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";
/// How an entry of a data source no longer saved is named.
const REMOVED_DATA_SOURCE: &str = "Removed data source";

/// Every history tab, by its data source, which is also the tab's key in the dock.
#[derive(Default)]
pub struct HistoryTabs {
    tabs: BTreeMap<DataSourceId, HistoryTab>,
}

impl HistoryTabs {
    /// Opens `data_source`'s tab, or points the open one at `console`. Returns the first
    /// search to send for a new tab. `names` names the data sources, for a search of them all.
    pub fn open(
        &mut self,
        data_source: DataSourceId,
        console: Option<ConsoleKey>,
        names: HashMap<DataSourceId, String>,
    ) -> Option<Command> {
        if let Some(tab) = self.tabs.get_mut(&data_source) {
            tab.console = console.or(tab.console.take());
            return None;
        }
        let (tab, search) = HistoryTab::new(data_source, console, names);
        self.tabs.insert(data_source, tab);
        Some(search)
    }

    pub fn close(&mut self, data_source: DataSourceId) {
        self.tabs.remove(&data_source);
    }

    pub fn get_mut(&mut self, data_source: DataSourceId) -> Option<&mut HistoryTab> {
        self.tabs.get_mut(&data_source)
    }

    /// Takes in an engine event. Returns the searches to run again: after a run, every tab's,
    /// since the run's statements are recorded by then.
    pub fn apply(&mut self, event: &Event) -> Vec<Command> {
        for tab in self.tabs.values_mut() {
            tab.apply(event);
        }
        if matches!(event, Event::RunFinished { .. }) {
            self.tabs
                .values_mut()
                .map(HistoryTab::search_command)
                .collect()
        } else {
            Vec::new()
        }
    }
}

pub struct HistoryTab {
    data_source: DataSourceId,
    /// The console statements go into: the one the tab was last opened from, until that
    /// console closes. Not kept across a restart.
    console: Option<ConsoleKey>,
    /// Every data source's name, for the entries of a search of them all.
    names: HashMap<DataSourceId, String>,
    scope: Scope,
    search: String,
    /// Only statements from there; `None` for all.
    kind: Option<HistorySourceKind>,
    period: Period,
    /// The search last sent: an answer to any other is one the tab has moved on from.
    /// Kept rather than rebuilt from the filters, since a period's start moves with the clock.
    sent: HistoryQuery,
    /// The answer to the latest search; `None` while it is pending.
    results: Option<Result<Vec<HistoryEntry>, String>>,
    /// The entry whose details show; kept when the results are searched again.
    selected: Option<HistoryEntry>,
    export: ExportStatus,
}

/// Where the latest export of the history is.
enum ExportStatus {
    None,
    Writing(PathBuf),
    Done {
        path: PathBuf,
        result: Result<usize, String>,
    },
}

/// What the user did in the tab this frame.
pub enum HistoryAction {
    /// The search changed or needs refreshing; run this one.
    Search(HistoryQuery),
    /// Put this statement into a console of the tab's data source.
    Insert(String),
    /// Export the current search, every match of it, in this format, to a file to ask for.
    Export(ExportSpec),
}

impl HistoryTab {
    /// The tab for `data_source`, with the search for every recent statement to send.
    fn new(
        data_source: DataSourceId,
        console: Option<ConsoleKey>,
        names: HashMap<DataSourceId, String>,
    ) -> (Self, Command) {
        let mut tab = Self {
            sent: HistoryQuery::all(data_source),
            data_source,
            console,
            names,
            scope: Scope::ThisDataSource,
            search: String::new(),
            kind: None,
            period: Period::AnyTime,
            results: None,
            selected: None,
            export: ExportStatus::None,
        };
        let search = tab.search_command();
        (tab, search)
    }

    pub fn data_source(&self) -> DataSourceId {
        self.data_source
    }

    /// The console a statement goes into, if the tab has one.
    pub fn console(&self) -> Option<&ConsoleKey> {
        self.console.as_ref()
    }

    pub fn set_console(&mut self, console: ConsoleKey) {
        self.console = Some(console);
    }

    fn query(&self, now: Timestamp) -> HistoryQuery {
        HistoryQuery {
            data_source: match self.scope {
                Scope::ThisDataSource => Some(self.data_source()),
                Scope::Every => None,
            },
            text: self.search.clone(),
            kind: self.kind,
            since: self.period.since(now),
            until: None,
        }
    }

    /// The search for the current filters, to rerun when new statements may have been
    /// recorded.
    fn search_command(&mut self) -> Command {
        Command::SearchHistory(self.next_search())
    }

    /// The search for the current filters, remembered as the one sent.
    fn next_search(&mut self) -> HistoryQuery {
        self.sent = self.query(Timestamp::now());
        self.sent.clone()
    }

    /// Exports what the tab lists, without its limit, to `path`.
    pub fn export(&mut self, path: PathBuf, spec: ExportSpec) -> Command {
        self.export = ExportStatus::Writing(path.clone());
        Command::ExportHistory {
            query: self.sent.clone(),
            path,
            spec,
            names: self.names.clone(),
        }
    }

    fn apply(&mut self, event: &Event) {
        match event {
            Event::HistoryFound { query, result } if *query == self.sent => {
                self.results = Some(result.clone());
            }
            Event::HistoryExported { path, result } if matches!(&self.export, ExportStatus::Writing(writing) if writing == path) =>
            {
                self.export = ExportStatus::Done {
                    path: path.clone(),
                    result: result.clone(),
                };
            }
            _ => {}
        }
    }

    pub fn show(&mut self, ui: &mut Ui) -> Option<HistoryAction> {
        let searched = ui
            .horizontal(|ui| {
                let filtered = scope_filter(ui, &mut self.scope)
                    | kind_filter(ui, &mut self.kind)
                    | period_filter(ui, &mut self.period);
                let typed = ui
                    .add(
                        TextEdit::singleline(&mut self.search)
                            .hint_text("Search statements")
                            .desired_width(f32::INFINITY),
                    )
                    .changed();
                filtered || typed
            })
            .inner;
        let exported = ui
            .horizontal(|ui| {
                let format = export_menu(ui);
                self.export_status(ui);
                format
            })
            .inner;
        if let Some(format) = exported {
            return Some(HistoryAction::Export(format));
        }
        if searched {
            self.results = None;
            return Some(HistoryAction::Search(self.next_search()));
        }
        ui.weak(
            "Click a statement for its details; double-click it, or click Insert, to put it \
             into the console.",
        );
        ui.separator();
        let listed = match &self.results {
            None => {
                ui.spinner();
                None
            }
            Some(Err(error)) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
                None
            }
            Some(Ok(entries)) if entries.is_empty() => {
                ui.weak("Nothing found.");
                None
            }
            Some(Ok(entries)) => {
                let (action, clicked) = self.entries(ui, entries);
                if clicked.is_some() {
                    self.selected = clicked;
                }
                action
            }
        };
        let Some(selected) = &self.selected else {
            return listed;
        };
        ui.separator();
        let detailed = ScrollArea::vertical()
            .id_salt("history_detail")
            .show(ui, |ui| detail::show(ui, selected))
            .inner;
        listed.or(detailed.map(|detail::DetailAction::Insert(sql)| HistoryAction::Insert(sql)))
    }

    fn export_status(&self, ui: &mut Ui) {
        match &self.export {
            ExportStatus::None => {}
            ExportStatus::Writing(path) => {
                ui.spinner();
                ui.weak(format!("Exporting to {}\u{2026}", path.display()));
            }
            ExportStatus::Done {
                path,
                result: Ok(written),
            } => {
                let statements = if *written == 1 {
                    "statement"
                } else {
                    "statements"
                };
                ui.weak(format!(
                    "Exported {written} {statements} to {}",
                    path.display()
                ));
            }
            ExportStatus::Done {
                result: Err(error), ..
            } => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        }
    }

    /// Lists the entries. Returns what to do, and the entry clicked, if any.
    fn entries(
        &self,
        ui: &mut Ui,
        entries: &[HistoryEntry],
    ) -> (Option<HistoryAction>, Option<HistoryEntry>) {
        let row_height = ui.spacing().interact_size.y;
        let time_zone = TimeZone::system();
        let mut chosen = None;
        let mut clicked = None;
        let height = if self.selected.is_some() {
            ui.available_height() * LIST_SHARE
        } else {
            f32::INFINITY
        };
        ScrollArea::vertical()
            .auto_shrink([false, self.selected.is_some()])
            .max_height(height)
            .show_rows(ui, row_height, entries.len(), |ui, rows| {
                for entry in &entries[rows] {
                    let selected = self.selected.as_ref() == Some(entry);
                    let data_source = (self.scope == Scope::Every).then(|| {
                        self.names
                            .get(&entry.data_source)
                            .map_or(REMOVED_DATA_SOURCE, String::as_str)
                    });
                    let row = EntryRow {
                        entry,
                        data_source,
                        selected,
                    };
                    match row.show(ui, &time_zone) {
                        Some(RowClick::Insert) => {
                            chosen = Some(HistoryAction::Insert(entry.sql.clone()));
                        }
                        Some(RowClick::Select) => clicked = Some(entry.clone()),
                        None => {}
                    }
                }
            });
        (chosen, clicked)
    }
}

/// **Export history** with its two formats. Returns the one chosen.
fn export_menu(ui: &mut Ui) -> Option<ExportSpec> {
    let mut chosen = None;
    widgets::menu_button(ui, "Export history", |ui| {
        for (spec, label) in [
            (ExportSpec::Csv(CsvOptions::default()), "As CSV\u{2026}"),
            (ExportSpec::Json, "As JSON\u{2026}"),
        ] {
            if ui.button(label).clicked() {
                chosen = Some(spec);
            }
        }
    })
    .response
    .on_hover_text("Write every statement this search finds to a file");
    chosen
}

/// What a click on an entry's row asked for.
enum RowClick {
    Insert,
    Select,
}

/// One entry of the list.
struct EntryRow<'a> {
    entry: &'a HistoryEntry,
    /// The entry's data source's name, shown when the list holds every data source's.
    data_source: Option<&'a str>,
    /// Whether its details show, which emphasises its statement.
    selected: bool,
}

impl EntryRow<'_> {
    fn show(&self, ui: &mut Ui, time_zone: &TimeZone) -> Option<RowClick> {
        let entry = self.entry;
        ui.horizontal(|ui| {
            let insert = ui.small_button("Insert").clicked();
            let time = entry.executed_at.to_zoned(time_zone.clone());
            ui.weak(time.strftime(TIME_FORMAT).to_string());
            outcome_label(ui, entry);
            source_badge(ui, &entry.source);
            if let Some(name) = self.data_source {
                ui.label(RichText::new(name).small());
            }
            let first_line = entry.sql.lines().next().unwrap_or_default();
            let mut text = RichText::new(first_line).monospace();
            if self.selected {
                text = text.strong();
            }
            let statement = ui
                .add(Label::new(text).truncate().sense(egui::Sense::click()))
                .on_hover_text(&entry.sql);
            if insert || statement.double_clicked() {
                Some(RowClick::Insert)
            } else if statement.clicked() {
                Some(RowClick::Select)
            } else {
                None
            }
        })
        .inner
    }
}

/// A short tag naming where the statement came from, with the details on hover. Statements
/// from MCP clients stand out, since nobody at the keyboard ran them.
fn source_badge(ui: &mut Ui, source: &HistorySource) {
    let palette = &Palette::for_visuals(ui.visuals());
    let (text, color, hover) = match source {
        HistorySource::Console => (
            "Console",
            palette.text_weak,
            "Run in a query console".to_owned(),
        ),
        HistorySource::GridEdit(_) => (
            "Grid",
            palette.text_weak,
            "Submitted from the grid".to_owned(),
        ),
        HistorySource::ObjectHelper => (
            "Object",
            palette.text_weak,
            "Run by an object helper in the explorer".to_owned(),
        ),
        HistorySource::Import { file } => (
            "Import",
            palette.text_weak,
            format!("Imported from {}", file.display()),
        ),
        HistorySource::Export => (
            "Export",
            palette.text_weak,
            "Read by Export \u{203a} All rows".to_owned(),
        ),
        HistorySource::Mcp { client } => (
            "MCP",
            palette.secondary,
            match client {
                Some(client) => format!("Run by the MCP client {client}, through dbrover mcp"),
                None => "Run by an MCP client, through dbrover mcp".to_owned(),
            },
        ),
        HistorySource::Action { id } => (
            "Action",
            palette.text_weak,
            format!("Run by the action {id} from actions.toml"),
        ),
        HistorySource::DbRover => (
            "DbRover",
            palette.text_weak,
            "Run by a DbRover that didn't record where".to_owned(),
        ),
    };
    ui.label(RichText::new(text).small().strong().color(color))
        .on_hover_text(hover);
}

fn outcome_label(ui: &mut Ui, entry: &HistoryEntry) {
    let palette = &Palette::for_visuals(ui.visuals());
    let (text, color) = match entry.outcome {
        HistoryOutcome::Ok => ("ok", palette.success),
        HistoryOutcome::Error => ("error", palette.error),
        HistoryOutcome::Cancelled => ("cancelled", palette.warning),
    };
    let details = match (entry.duration, entry.rows) {
        (Some(duration), Some(rows)) => format!("{} ms · {rows} rows", duration.as_millis()),
        (Some(duration), None) => format!("{} ms", duration.as_millis()),
        (None, Some(rows)) => format!("{rows} rows"),
        (None, None) => String::new(),
    };
    ui.colored_label(color, text).on_hover_text(details);
}
