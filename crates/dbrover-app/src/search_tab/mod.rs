//! Search data: a tab per data source that finds the rows holding a value, across a scope's
//! tables, and opens each one on its row (W16, `docs/data-search.md`).
//!
//! A search is planned first, which lists the tables and writes their statements, then run;
//! between the two, a careful data source asks before more than one table is read.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use dbrover_core::{
    CaseMatch, CatalogPath, DataSearch, DataSourceId, MatchKind, NodeKind, Relations, RowsPerTable,
    SearchScope, SearchableTable, TablesToSearch, TimeLimit,
};
use dbrover_engine::{
    Command, Event, RunId, SearchEnd, SearchLimits, SearchPlan, SearchQuestion, SearchRequest,
    TableSearched,
};
use egui::Ui;

use crate::console::RowsToOpen;

mod confirm;
mod form;
mod highlight;
mod picker;
mod results;

/// Every search tab, by its data source, which is also the tab's key in the dock.
#[derive(Default)]
pub struct SearchTabs {
    tabs: BTreeMap<DataSourceId, SearchTab>,
    /// Tabs closed in the dock, whose connections the app closes.
    closed: Vec<DataSourceId>,
}

impl SearchTabs {
    /// Opens `data_source`'s tab on `scope`, or points an open one at it.
    pub fn open(
        &mut self,
        data_source: DataSourceId,
        scope: CatalogPath,
        defaults: SearchDefaults,
    ) {
        let tab = self
            .tabs
            .entry(data_source)
            .or_insert_with(|| SearchTab::new(data_source, defaults));
        tab.scope = SearchScope::Under(scope.clone());
        tab.base = scope;
        tab.focus_value = true;
    }

    /// Forgets the tab; its connection is closed by the command [`SearchTabs::take_closed`]
    /// hands the app.
    pub fn close(&mut self, data_source: DataSourceId) {
        if self.tabs.remove(&data_source).is_some() {
            self.closed.push(data_source);
        }
    }

    /// The commands closing the connections of the tabs closed since the last call.
    pub fn take_closed(&mut self) -> Vec<Command> {
        self.closed
            .drain(..)
            .map(Command::CloseDataSearch)
            .collect()
    }

    pub fn get_mut(&mut self, data_source: DataSourceId) -> Option<&mut SearchTab> {
        self.tabs.get_mut(&data_source)
    }

    pub fn apply(&mut self, event: &Event) {
        for tab in self.tabs.values_mut() {
            tab.apply(event);
        }
    }
}

/// What a new tab starts with, from Settings › Data search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchDefaults {
    pub limits: SearchLimits,
    pub views: Relations,
}

impl SearchDefaults {
    pub fn from_settings(settings: dbrover_store::DataSearchSettings) -> Self {
        let rows = std::num::NonZeroU32::new(settings.rows())
            .map_or(RowsPerTable::DEFAULT, RowsPerTable::new);
        Self {
            limits: SearchLimits {
                per_table: settings
                    .table_limit_seconds()
                    .and_then(TimeLimit::from_seconds),
                rows,
            },
            views: if settings.include_views {
                Relations::TablesAndViews
            } else {
                Relations::TablesOnly
            },
        }
    }
}

/// What the tab needs from the app each frame.
pub struct SearchView<'a> {
    pub name: &'a str,
}

/// What the user asked of the tab this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchAction {
    Plan {
        data_source: DataSourceId,
        run: RunId,
        request: SearchRequest,
    },
    Run {
        data_source: DataSourceId,
        run: RunId,
    },
    Cancel {
        data_source: DataSourceId,
        run: RunId,
    },
    ListTables {
        data_source: DataSourceId,
        tables: TablesToSearch,
    },
    /// Open a row, or a keyless table's rows with these values, in a new console, and run it.
    OpenRows(DataSourceId, RowsToOpen),
    /// Open a statement in a new console and run it.
    RunInConsole(DataSourceId, String),
    /// Open a statement in a new console, not run.
    OpenInConsole(DataSourceId, String),
    Copy(String),
}

/// Where the tab's search is.
enum Phase {
    Idle,
    /// Waiting for the engine's plan.
    Planning {
        run: RunId,
        request: SearchRequest,
    },
    /// A plan that waits for the user's yes.
    Asking(Search),
    Running(Search),
    Ended(Search),
    Failed(String),
}

/// A search's plan and what it has found.
struct Search {
    run: RunId,
    request: SearchRequest,
    plan: SearchPlan,
    /// Each table's outcome by its place in the plan, in the order they finished.
    searched: Vec<TableSearched>,
    /// The table being read.
    current: Option<usize>,
    started: Instant,
    end: Option<(SearchEnd, Duration)>,
    /// The tables whose rows are shown.
    expanded: HashSet<usize>,
    /// A timed-out table searched again with no limit: the retry's run and the table's place.
    retry: Option<(RunId, usize)>,
}

impl Search {
    fn new(run: RunId, request: SearchRequest, plan: SearchPlan) -> Self {
        Self {
            run,
            request,
            plan,
            searched: Vec::new(),
            current: None,
            started: Instant::now(),
            end: None,
            expanded: HashSet::new(),
            retry: None,
        }
    }

    fn outcome(&self, index: usize) -> Option<&TableSearched> {
        self.searched.iter().find(|table| table.index == index)
    }

    /// Records a table's outcome, replacing an earlier one (a retry's).
    fn record(&mut self, searched: TableSearched) {
        self.searched.retain(|table| table.index != searched.index);
        if matches!(
            searched.outcome,
            dbrover_engine::TableOutcome::Matched { .. }
        ) && self.expanded.len() < results::EXPANDED_AT_FIRST
        {
            self.expanded.insert(searched.index);
        }
        self.searched.push(searched);
    }
}

/// The question waiting for an answer.
enum Question {
    /// Search a timed-out table again, with no time limit.
    Retry(usize),
}

pub struct SearchTab {
    data_source: DataSourceId,
    text: String,
    matching: MatchKind,
    case: CaseMatch,
    views: Relations,
    scope: SearchScope,
    /// The node the tab was opened on, whose tables **Change…** lists.
    base: CatalogPath,
    limits: SearchLimits,
    next_run: u64,
    phase: Phase,
    question: Option<Question>,
    picker: Option<picker::Picker>,
    /// Actions decided while applying events, sent with the next frame's.
    pending: Vec<SearchAction>,
    /// Focus the value's field on the next frame: the tab was just opened or pointed anew.
    focus_value: bool,
}

impl SearchTab {
    fn new(data_source: DataSourceId, defaults: SearchDefaults) -> Self {
        Self {
            data_source,
            text: String::new(),
            matching: MatchKind::Contains,
            case: CaseMatch::Ignore,
            views: defaults.views,
            scope: SearchScope::Under(CatalogPath::root()),
            base: CatalogPath::root(),
            limits: defaults.limits,
            next_run: 1,
            phase: Phase::Idle,
            question: None,
            picker: None,
            pending: Vec::new(),
            focus_value: true,
        }
    }

    fn next_run(&mut self) -> RunId {
        let run = RunId(self.next_run);
        self.next_run += 1;
        run
    }

    fn is_busy(&self) -> bool {
        matches!(self.phase, Phase::Planning { .. } | Phase::Running(_))
            || matches!(&self.phase, Phase::Ended(search) if search.retry.is_some())
    }

    /// The run being read now, which Cancel stops.
    fn cancellable(&self) -> Option<RunId> {
        match &self.phase {
            Phase::Planning { run, .. } => Some(*run),
            Phase::Running(search) => Some(search.run),
            Phase::Ended(search) => search.retry.map(|(run, _)| run),
            _ => None,
        }
    }

    /// Plans a search of the value typed, where the scope says.
    fn search(&mut self) -> Option<SearchAction> {
        let search = DataSearch::new(&self.text, self.matching, self.case).ok()?;
        let run = self.next_run();
        let request = SearchRequest {
            search,
            tables: TablesToSearch {
                scope: self.scope.clone(),
                views: self.views,
            },
            limits: self.limits,
        };
        self.phase = Phase::Planning {
            run,
            request: request.clone(),
        };
        Some(SearchAction::Plan {
            data_source: self.data_source,
            run,
            request,
        })
    }

    /// Searches one timed-out table again, alone and with no time limit.
    fn retry(&mut self, index: usize) -> Option<SearchAction> {
        let run = self.next_run();
        let Phase::Ended(search) = &mut self.phase else {
            return None;
        };
        let table = search.plan.tables.get(index)?;
        let request = SearchRequest {
            tables: TablesToSearch {
                scope: SearchScope::Tables(vec![table.table.path.clone()]),
                views: Relations::TablesAndViews,
            },
            limits: SearchLimits {
                per_table: None,
                ..search.request.limits
            },
            ..search.request.clone()
        };
        search.retry = Some((run, index));
        Some(SearchAction::Plan {
            data_source: self.data_source,
            run,
            request,
        })
    }

    fn apply(&mut self, event: &Event) {
        match event {
            Event::DataSearchPlanned {
                data_source,
                run,
                result,
            } if *data_source == self.data_source => self.planned(*run, result),
            Event::DataSearchTableStarted {
                data_source,
                run,
                index,
            } if *data_source == self.data_source => {
                if let Phase::Running(search) = &mut self.phase
                    && search.run == *run
                {
                    search.current = Some(*index);
                }
            }
            Event::DataSearchTableSearched {
                data_source,
                run,
                searched,
            } if *data_source == self.data_source => self.searched(*run, searched),
            Event::DataSearchEnded {
                data_source,
                run,
                end,
            } if *data_source == self.data_source => self.ended(*run, end),
            Event::SearchTablesListed {
                data_source,
                result,
            } if *data_source == self.data_source => {
                if let Some(picker) = &mut self.picker {
                    picker.listed(result.clone());
                }
            }
            Event::DataSearchSessionChanged {
                data_source,
                state: dbrover_engine::SessionState::Failed(reason),
            } if *data_source == self.data_source => {
                if self.is_busy() {
                    self.phase = Phase::Failed(reason.clone());
                }
                if let Some(picker) = &mut self.picker {
                    picker.listed(Err(reason.clone()));
                }
            }
            _ => {}
        }
    }

    fn planned(&mut self, run: RunId, result: &Result<SearchPlan, String>) {
        if let Phase::Ended(search) = &mut self.phase
            && search.retry.is_some_and(|(retry, _)| retry == run)
        {
            match result {
                Ok(_) => self.pending.push(SearchAction::Run {
                    data_source: self.data_source,
                    run,
                }),
                Err(_) => search.retry = None,
            }
            return;
        }
        let Phase::Planning {
            run: planning,
            request,
        } = &self.phase
        else {
            return;
        };
        if *planning != run {
            return;
        }
        let request = request.clone();
        self.phase = match result {
            Ok(plan) if plan.question.is_some() => {
                Phase::Asking(Search::new(run, request, plan.clone()))
            }
            Ok(plan) => {
                self.pending.push(SearchAction::Run {
                    data_source: self.data_source,
                    run,
                });
                Phase::Running(Search::new(run, request, plan.clone()))
            }
            Err(error) => Phase::Failed(error.clone()),
        };
    }

    fn searched(&mut self, run: RunId, searched: &TableSearched) {
        match &mut self.phase {
            Phase::Running(search) if search.run == run => {
                search.current = None;
                search.record(searched.clone());
            }
            Phase::Ended(search) => {
                if let Some((retry, index)) = search.retry
                    && retry == run
                {
                    search.record(TableSearched {
                        index,
                        ..searched.clone()
                    });
                }
            }
            _ => {}
        }
    }

    fn ended(&mut self, run: RunId, end: &SearchEnd) {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Running(mut search) if search.run == run => {
                search.current = None;
                search.end = Some((end.clone(), search.started.elapsed()));
                self.phase = Phase::Ended(search);
            }
            Phase::Ended(mut search) if search.retry.is_some_and(|(retry, _)| retry == run) => {
                search.retry = None;
                self.phase = Phase::Ended(search);
            }
            other => self.phase = other,
        }
    }

    /// The careful data source's yes: run the plan.
    fn confirmed(&mut self) -> Option<SearchAction> {
        let Phase::Asking(search) = std::mem::replace(&mut self.phase, Phase::Idle) else {
            return None;
        };
        let run = search.run;
        self.phase = Phase::Running(Search {
            started: Instant::now(),
            ..search
        });
        Some(SearchAction::Run {
            data_source: self.data_source,
            run,
        })
    }

    pub fn show(&mut self, ui: &mut Ui, view: &SearchView<'_>) -> Vec<SearchAction> {
        let mut actions = std::mem::take(&mut self.pending);
        actions.extend(form::show(self, ui, view));
        ui.separator();
        actions.extend(results::show(self, ui, view));
        actions.extend(confirm::show(self, ui.ctx(), view));
        actions.extend(picker::show(self, ui.ctx(), view));
        if self.is_busy() {
            // The elapsed time and the spinner move while the engine works.
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        actions
    }
}

/// The scope as a line under the field: *all of production*, *shop › public*, *3 tables*.
fn scope_label(scope: &SearchScope, data_source: &str) -> String {
    match scope {
        SearchScope::Under(path) if path.is_root() => format!("all of {data_source}"),
        SearchScope::Under(path) => path_label(path),
        SearchScope::Tables(paths) if paths.len() == 1 => {
            paths.first().map_or_else(String::new, path_label)
        }
        SearchScope::Tables(paths) => format!("{} tables", paths.len()),
    }
}

/// A catalog path without its folders: `shop › public › orders`.
fn path_label(path: &CatalogPath) -> String {
    path.segments()
        .iter()
        .filter(|segment| !matches!(segment.kind, NodeKind::Folder(_)))
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join(" \u{203a} ")
}

/// A table's name as the results list it: `public.orders`.
fn table_label(table: &SearchableTable) -> String {
    format!("{}.{}", table.name.schema, table.name.name)
}

/// The question a plan asks, in words: *Search 38 tables of "production" for "o'brien"?*
fn question_heading(question: SearchQuestion, data_source: &str, value: &str) -> String {
    match question {
        SearchQuestion::Careful { tables } => format!(
            "Search {tables} tables of \u{201c}{data_source}\u{201d} for \u{201c}{value}\u{201d}?"
        ),
    }
}
