//! Schema compare tabs (W13): two schemas side by side, what differs between them, and the
//! script that turns the left into the right, opened in a console. A side is a live schema or
//! a snapshot file (VC3). See `docs/schema-compare.md`.
//!
//! Like definition tabs, they aren't restored after a restart: their sides are read from live
//! connections, and a comparison read back from disk would be stale. Their keys are numbers
//! handed out while the app runs; [`crate::workspace::forget_transient_tabs`] drops them from
//! a saved layout.

#[cfg(test)]
mod tests;
mod view;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dbrover_core::DataSourceId;
use dbrover_engine::Event;
use dbrover_schema::{
    CompareError, Comparison, ObjectKind, Schema, Script, Sides, compare, script,
};
use dbrover_sql::Dialect;
use serde::{Deserialize, Serialize};

/// A compare tab's key in the dock, unique while the app runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CompareId(u64);

/// Every open compare tab, by its key.
#[derive(Default)]
pub struct SchemaCompares {
    tabs: BTreeMap<CompareId, CompareTab>,
    next: u64,
}

/// A data source a side may read from: one whose schemas can be compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: DataSourceId,
    pub name: String,
    pub dialect: Dialect,
}

/// A live schema to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSchema {
    pub data_source: DataSourceId,
    pub schema: String,
}

/// Where a side's schema is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    Live(LiveSchema),
    Snapshot(PathBuf),
}

/// Which side of a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideIndex {
    Left,
    Right,
}

impl SideIndex {
    fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
        }
    }
}

/// What the user asked of a compare tab this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareAction {
    /// Read these sides, for **Compare** or **Compare again**.
    Read(Vec<Reading>),
    /// Ask for a snapshot file for a side, and make it that side.
    PickSnapshot { tab: CompareId, side: SideIndex },
    /// Save a snapshot of a live side, asking where.
    SaveSnapshot(LiveSchema),
    /// Put the script into a new console of `data_source`, to read and run there.
    OpenInConsole {
        data_source: DataSourceId,
        text: String,
    },
}

impl SchemaCompares {
    /// A tab with `left` on its left and the right still to pick.
    pub fn open(&mut self, left: LiveSchema) -> CompareId {
        let id = CompareId(self.next);
        self.next += 1;
        let tab = CompareTab {
            sides: [Side::live(left), Side::default()],
            ..CompareTab::default()
        };
        self.tabs.insert(id, tab);
        id
    }

    pub fn close(&mut self, id: CompareId) {
        self.tabs.remove(&id);
    }

    pub fn title(&self, id: CompareId) -> String {
        self.tabs
            .get(&id)
            .map_or_else(|| "Schema compare".to_owned(), CompareTab::title)
    }

    /// Makes a side the snapshot file at `path`, read at the next **Compare**.
    pub fn set_snapshot(&mut self, id: CompareId, side: SideIndex, path: PathBuf) {
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.sides[side.index()] = Side {
                source: Source::Snapshot(path),
                state: SideState::Unread,
            };
            tab.outcome = None;
            tab.selected = None;
        }
    }

    /// Takes in a schema the engine read, live or from a file, for every side waiting on it.
    pub fn apply(&mut self, event: &Event) {
        let (reading, result) = match event {
            Event::SchemaDescribed {
                data_source,
                schema,
                result,
            } => (
                Reading::Live(LiveSchema {
                    data_source: *data_source,
                    schema: schema.clone(),
                }),
                result,
            ),
            Event::SchemaSnapshotRead { path, result } => (Reading::Snapshot(path.clone()), result),
            _ => return,
        };
        for tab in self.tabs.values_mut() {
            tab.read(&reading, result);
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        id: CompareId,
        candidates: &[Candidate],
    ) -> Option<CompareAction> {
        let tab = self.tabs.get_mut(&id)?;
        view::show(ui, id, tab, candidates)
    }
}

/// One compare tab: its two sides, and once both are read, what differs.
#[derive(Default)]
struct CompareTab {
    /// Left, then right.
    sides: [Side; 2],
    outcome: Option<Outcome>,
    selected: Option<(ObjectKind, String)>,
    /// The kinds the differences list hides.
    hidden: BTreeSet<ObjectKind>,
}

#[derive(Default)]
struct Side {
    source: Source,
    state: SideState,
}

enum Source {
    Live {
        data_source: Option<DataSourceId>,
        schema: String,
    },
    Snapshot(PathBuf),
}

impl Default for Source {
    fn default() -> Self {
        Self::Live {
            data_source: None,
            schema: String::new(),
        }
    }
}

#[derive(Default)]
enum SideState {
    /// Picked, or being picked, and not read yet.
    #[default]
    Unread,
    Reading,
    Read(Schema),
    Failed(String),
}

/// What comparing the two sides gave.
enum Outcome {
    Compared {
        comparison: Comparison,
        script: Option<Script>,
    },
    /// Two engines, which can't be compared.
    Refused(CompareError),
}

impl Side {
    fn live(schema: LiveSchema) -> Self {
        Self {
            source: Source::Live {
                data_source: Some(schema.data_source),
                schema: schema.schema,
            },
            state: SideState::Unread,
        }
    }

    /// What to read for this side, once it is picked whole.
    fn reading(&self) -> Option<Reading> {
        match &self.source {
            Source::Live {
                data_source,
                schema,
            } => {
                let data_source = (*data_source)?;
                let schema = schema.trim();
                (!schema.is_empty()).then(|| {
                    Reading::Live(LiveSchema {
                        data_source,
                        schema: schema.to_owned(),
                    })
                })
            }
            Source::Snapshot(path) => Some(Reading::Snapshot(path.clone())),
        }
    }

    /// The live schema this side reads, for saving a snapshot of it.
    fn live_schema(&self) -> Option<LiveSchema> {
        match self.reading()? {
            Reading::Live(live) => Some(live),
            Reading::Snapshot(_) => None,
        }
    }

    fn data_source(&self) -> Option<DataSourceId> {
        match &self.source {
            Source::Live { data_source, .. } => *data_source,
            Source::Snapshot(_) => None,
        }
    }

    /// The schema's name as the tab's title shows it: a live schema's, or a snapshot's file.
    fn short_name(&self) -> String {
        match &self.source {
            Source::Live { schema, .. } => schema.trim().to_owned(),
            Source::Snapshot(path) => file_name(path),
        }
    }

    fn schema(&self) -> Option<&Schema> {
        match &self.state {
            SideState::Read(schema) => Some(schema),
            _ => None,
        }
    }

    fn is_reading(&self) -> bool {
        matches!(self.state, SideState::Reading)
    }
}

impl CompareTab {
    fn title(&self) -> String {
        let [left, right] = &self.sides;
        match (left.short_name(), right.short_name()) {
            (left, _) if left.is_empty() => "Schema compare".to_owned(),
            (left, right) if right.is_empty() => format!("Compare {left}"),
            (left, right) => format!("{left} \u{2192} {right}"),
        }
    }

    /// Both sides, if both are picked.
    fn readings(&self) -> Option<[Reading; 2]> {
        let [left, right] = &self.sides;
        Some([left.reading()?, right.reading()?])
    }

    /// Starts reading both sides, as **Compare** does.
    fn start_reading(&mut self) -> Option<CompareAction> {
        let readings = self.readings()?;
        for side in &mut self.sides {
            side.state = SideState::Reading;
        }
        self.outcome = None;
        Some(CompareAction::Read(readings.into()))
    }

    fn swap(&mut self) {
        self.sides.swap(0, 1);
        self.outcome = None;
    }

    fn read(&mut self, reading: &Reading, result: &Result<Schema, String>) {
        let mut changed = false;
        for side in &mut self.sides {
            if side.is_reading() && side.reading().as_ref() == Some(reading) {
                side.state = match result {
                    Ok(read) => SideState::Read(read.clone()),
                    Err(error) => SideState::Failed(error.clone()),
                };
                changed = true;
            }
        }
        if changed {
            self.outcome = None;
        }
    }

    /// Compares the sides once both are read, naming them in the script's header by
    /// `candidates`' names.
    fn compare_if_read(&mut self, candidates: &[Candidate]) {
        if self.outcome.is_some() {
            return;
        }
        let [left, right] = &self.sides;
        let (Some(left_schema), Some(right_schema)) = (left.schema(), right.schema()) else {
            return;
        };
        let sides = Sides {
            left: describe(left, candidates),
            right: describe(right, candidates),
        };
        self.outcome = Some(match compare(left_schema, right_schema) {
            Ok(comparison) => Outcome::Compared {
                script: script(left_schema, right_schema, &sides).ok().flatten(),
                comparison,
            },
            Err(error) => Outcome::Refused(error),
        });
    }
}

/// "schema staging on shop-dev", or "snapshot public.schema.toml", as the script's header and
/// the definitions' headings name a side.
fn describe(side: &Side, candidates: &[Candidate]) -> String {
    match &side.source {
        Source::Live {
            data_source,
            schema,
        } => {
            let name = data_source
                .and_then(|id| candidates.iter().find(|candidate| candidate.id == id))
                .map_or("a removed data source", |candidate| candidate.name.as_str());
            format!("schema {} on {name}", schema.trim())
        }
        Source::Snapshot(path) => format!("snapshot {}", file_name(path)),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}
