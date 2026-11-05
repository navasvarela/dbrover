//! Schema compare tabs (W13): two schemas side by side, what differs between them, and the
//! script that turns the left into the right, opened in a console. See
//! `docs/schema-compare.md`.
//!
//! Like definition tabs, they aren't restored after a restart: their sides are read from live
//! connections, and a comparison read back from disk would be stale. Their keys are numbers
//! handed out while the app runs; [`crate::workspace::forget_transient_tabs`] drops them from
//! a saved layout.

#[cfg(test)]
mod tests;
mod view;

use std::collections::{BTreeMap, BTreeSet};

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

/// What the user asked of a compare tab this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareAction {
    /// Read these schemas, for **Compare** or **Compare again**.
    Read(Vec<LiveSchema>),
    /// Put the script into a new console of the left's data source, to read and run there.
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

    /// Takes in a schema the engine read, for every side waiting on it.
    pub fn apply(&mut self, event: &Event) {
        let Event::SchemaDescribed {
            data_source,
            schema,
            result,
        } = event
        else {
            return;
        };
        for tab in self.tabs.values_mut() {
            tab.read(*data_source, schema, result);
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
    data_source: Option<DataSourceId>,
    schema: String,
    state: SideState,
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
            data_source: Some(schema.data_source),
            schema: schema.schema,
            state: SideState::Unread,
        }
    }

    fn picked(&self) -> Option<LiveSchema> {
        let data_source = self.data_source?;
        let schema = self.schema.trim();
        (!schema.is_empty()).then(|| LiveSchema {
            data_source,
            schema: schema.to_owned(),
        })
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
        match (left.schema.trim(), right.schema.trim()) {
            ("", _) => "Schema compare".to_owned(),
            (left, "") => format!("Compare {left}"),
            (left, right) => format!("{left} \u{2192} {right}"),
        }
    }

    /// Both sides, if both are picked.
    fn picked(&self) -> Option<[LiveSchema; 2]> {
        let [left, right] = &self.sides;
        Some([left.picked()?, right.picked()?])
    }

    /// Starts reading both sides, as **Compare** does.
    fn start_reading(&mut self) -> Option<CompareAction> {
        let picked = self.picked()?;
        for side in &mut self.sides {
            side.state = SideState::Reading;
        }
        self.outcome = None;
        Some(CompareAction::Read(picked.into()))
    }

    fn swap(&mut self) {
        self.sides.swap(0, 1);
        self.outcome = None;
    }

    fn read(&mut self, data_source: DataSourceId, schema: &str, result: &Result<Schema, String>) {
        let mut changed = false;
        for side in &mut self.sides {
            let waiting = side.is_reading()
                && side.data_source == Some(data_source)
                && side.schema.trim() == schema;
            if waiting {
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

/// "schema staging on shop-dev", as the script's header names a side.
fn describe(side: &Side, candidates: &[Candidate]) -> String {
    let name = side
        .data_source
        .and_then(|id| candidates.iter().find(|candidate| candidate.id == id))
        .map_or("a removed data source", |candidate| candidate.name.as_str());
    format!("schema {} on {name}", side.schema.trim())
}
