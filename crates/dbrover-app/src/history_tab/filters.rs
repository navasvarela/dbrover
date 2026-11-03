//! The History window's filters: which data sources, where the statements came from, and
//! when.

use dbrover_store::HistorySourceKind;
use egui::{ComboBox, Ui};
use jiff::{SignedDuration, Timestamp};

/// Whose statements the tab lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    /// The console's data source.
    ThisDataSource,
    /// Every data source in the history file, removed ones included.
    Every,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Self::ThisDataSource => "This data source",
            Self::Every => "Every data source",
        }
    }
}

/// How far back the tab looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Period {
    AnyTime,
    LastHour,
    LastDay,
    LastWeek,
    LastMonth,
}

impl Period {
    const ALL: [Self; 5] = [
        Self::AnyTime,
        Self::LastHour,
        Self::LastDay,
        Self::LastWeek,
        Self::LastMonth,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::AnyTime => "Any time",
            Self::LastHour => "Last hour",
            Self::LastDay => "Last 24 hours",
            Self::LastWeek => "Last 7 days",
            Self::LastMonth => "Last 30 days",
        }
    }

    /// When the period starts, counting back from `now`.
    pub(super) fn since(self, now: Timestamp) -> Option<Timestamp> {
        const HOUR: i64 = 60 * 60;
        let seconds = match self {
            Self::AnyTime => return None,
            Self::LastHour => HOUR,
            Self::LastDay => 24 * HOUR,
            Self::LastWeek => 7 * 24 * HOUR,
            Self::LastMonth => 30 * 24 * HOUR,
        };
        now.checked_sub(SignedDuration::from_secs(seconds)).ok()
    }
}

/// Returns whether the choice changed.
pub(super) fn scope_filter(ui: &mut Ui, scope: &mut Scope) -> bool {
    let mut changed = false;
    ComboBox::from_id_salt("history_scope")
        .selected_text(scope.label())
        .show_ui(ui, |ui| {
            for choice in [Scope::ThisDataSource, Scope::Every] {
                changed |= ui.selectable_value(scope, choice, choice.label()).changed();
            }
        })
        .response
        .on_hover_text("Whose statements to list");
    changed
}

/// Returns whether the choice changed.
pub(super) fn period_filter(ui: &mut Ui, period: &mut Period) -> bool {
    let mut changed = false;
    ComboBox::from_id_salt("history_period")
        .selected_text(period.label())
        .show_ui(ui, |ui| {
            for choice in Period::ALL {
                changed |= ui
                    .selectable_value(period, choice, choice.label())
                    .changed();
            }
        })
        .response
        .on_hover_text("When the statements ran");
    changed
}

const ALL_SOURCES: &str = "All sources";

/// What the filter and the badges call each kind of source.
fn kind_label(kind: HistorySourceKind) -> &'static str {
    match kind {
        HistorySourceKind::Console => "Consoles",
        HistorySourceKind::GridEdit => "Grid edits",
        HistorySourceKind::ObjectHelper => "Object helpers",
        HistorySourceKind::Import => "Imports",
        HistorySourceKind::Export => "Exports",
        HistorySourceKind::Mcp => "MCP clients",
        HistorySourceKind::Action => "Actions",
        HistorySourceKind::DbRover => "Earlier DbRover",
    }
}

/// Returns whether the choice changed.
pub(super) fn kind_filter(ui: &mut Ui, kind: &mut Option<HistorySourceKind>) -> bool {
    let selected = kind.map_or(ALL_SOURCES, kind_label);
    let mut changed = false;
    ComboBox::from_id_salt("history_source")
        .selected_text(selected)
        .show_ui(ui, |ui| {
            changed |= ui.selectable_value(kind, None, ALL_SOURCES).changed();
            for choice in HistorySourceKind::ALL {
                changed |= ui
                    .selectable_value(kind, Some(choice), kind_label(choice))
                    .changed();
            }
        })
        .response
        .on_hover_text("Where the statements were run from");
    changed
}
