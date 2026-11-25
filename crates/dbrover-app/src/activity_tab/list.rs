//! The list of sessions: blocking chains first, waiting sessions drawn under their blocker,
//! then everything else the filters keep.

use dbrover_core::{ServerSession, SessionState, blocking_order, spoken_duration};
use dbrover_engine::Activity;
use egui::{Label, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};

use super::{ActivityTab, ActivityView, own_name, state_text};
use crate::theme::Palette;

/// How far each level of a blocking chain is indented.
const LEVEL_INDENT: f32 = 14.0;

/// Draws the list; returns the session clicked, if any.
pub(super) fn show(
    tab: &ActivityTab,
    ui: &mut Ui,
    activity: &Activity,
    view: &ActivityView<'_>,
    height: f32,
) -> Option<ServerSession> {
    let sessions = &activity.server.sessions;
    let rows = blocking_order(sessions, |session| tab.keeps(session));
    if rows.is_empty() {
        ui.weak(if sessions.is_empty() {
            "No sessions."
        } else {
            "No session matches; idle and background sessions are hidden unless switched on."
        });
        return None;
    }
    let palette = &Palette::for_visuals(ui.visuals());
    let row_height = ui.spacing().interact_size.y;
    let selected = tab.selected.as_ref().map(|session| &session.id);
    let mut clicked = None;
    TableBuilder::new(ui)
        .id_salt(("activity_list", tab.data_source))
        .striped(true)
        .sense(Sense::click())
        .max_scroll_height(height)
        .auto_shrink([false, true])
        .column(Column::auto().at_least(70.0))
        .column(Column::auto().at_least(80.0).clip(true))
        .column(Column::auto().at_least(80.0).clip(true))
        .column(Column::auto().at_least(170.0).clip(true))
        .column(Column::auto().at_least(110.0))
        .column(Column::auto().at_least(60.0))
        .column(Column::remainder().at_least(200.0).clip(true))
        .header(row_height, |mut header| {
            for title in [
                "Session",
                "User",
                "Database",
                "Application",
                "State",
                "For",
                "Statement",
            ] {
                header.col(|ui| {
                    ui.strong(title);
                });
            }
        })
        .body(|body| {
            body.rows(row_height, rows.len(), |mut row| {
                let Some(place) = rows.get(row.index()) else {
                    return;
                };
                let Some(session) = sessions.get(place.session) else {
                    return;
                };
                row.set_selected(selected == Some(&session.id));
                row.col(|ui| {
                    ui.horizontal(|ui| session_id(ui, place, session, palette));
                });
                text_cell(&mut row, session.user.as_deref());
                text_cell(&mut row, session.database.as_deref());
                let application = activity
                    .own(&session.id)
                    .map(|own| own_name(own, view.consoles))
                    .or_else(|| session.application.clone());
                text_cell(&mut row, application.as_deref());
                row.col(|ui| {
                    ui.add(Label::new(state_text(session.state, palette)).selectable(false));
                });
                let time = session.in_state_for.map(spoken_duration);
                text_cell(&mut row, time.as_deref());
                let statement = session.statement.as_ref().map(|statement| {
                    statement
                        .text
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                });
                row.col(|ui| {
                    let text = RichText::new(statement.unwrap_or_default()).monospace();
                    ui.add(Label::new(text).selectable(false).truncate());
                });
                if row.response().clicked() {
                    clicked = Some(session.clone());
                }
            });
        });
    clicked
}

/// The session's id, indented under its blocker, red when it is in a cycle.
fn session_id(
    ui: &mut Ui,
    place: &dbrover_core::ActivityRow,
    session: &ServerSession,
    palette: &Palette,
) {
    ui.add_space(LEVEL_INDENT * place.depth as f32);
    let mut id = RichText::new(session.id.to_string()).monospace();
    if place.in_cycle {
        id = id.color(palette.error);
    }
    let label = ui.add(Label::new(id).selectable(false));
    if place.in_cycle {
        label.on_hover_text("Part of a cycle of sessions waiting on each other");
    }
}

fn text_cell(row: &mut egui_extras::TableRow<'_, '_>, text: Option<&str>) {
    row.col(|ui| {
        ui.add(Label::new(text.unwrap_or("")).selectable(false).truncate());
    });
}

impl ActivityTab {
    /// Whether the filters keep `session`. A session in a blocking chain is kept whatever this
    /// says (see [`blocking_order`]).
    pub(super) fn keeps(&self, session: &ServerSession) -> bool {
        let state_shown = match session.state {
            SessionState::Idle => self.show_idle,
            SessionState::Background => self.show_background,
            _ => true,
        };
        state_shown && self.matches_filter(session)
    }

    fn matches_filter(&self, session: &ServerSession) -> bool {
        let filter = self.filter.trim().to_lowercase();
        if filter.is_empty() {
            return true;
        }
        let statement = session.statement.as_ref().map(|statement| &statement.text);
        [
            session.user.as_ref(),
            session.application.as_ref(),
            session.database.as_ref(),
            statement,
        ]
        .into_iter()
        .flatten()
        .any(|text| text.to_lowercase().contains(&filter))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dbrover_core::{DataSourceId, SessionId, SessionStatement};

    use super::*;

    fn session(state: SessionState, statement: &str) -> ServerSession {
        ServerSession {
            id: SessionId::Number(1),
            user: Some("etl".to_owned()),
            application: Some("airflow".to_owned()),
            state,
            statement: Some(SessionStatement {
                text: statement.to_owned(),
                running: true,
                cut_short: false,
            }),
            ..ServerSession::default()
        }
    }

    #[test]
    fn hides_idle_and_background_sessions_until_asked_and_filters_by_text() {
        let mut tab = ActivityTab::new(DataSourceId::generate(), Some(Duration::from_secs(5)));
        let idle = session(SessionState::Idle, "SELECT 1");
        let background = session(SessionState::Background, "");
        let active = session(SessionState::Active, "INSERT INTO daily_totals SELECT …");

        assert!(!tab.keeps(&idle));
        assert!(!tab.keeps(&background));
        assert!(tab.keeps(&active));
        tab.show_idle = true;
        tab.show_background = true;
        assert!(tab.keeps(&idle) && tab.keeps(&background));

        tab.filter = "DAILY".to_owned();
        assert!(tab.keeps(&active));
        assert!(!tab.keeps(&idle));
        tab.filter = "airflow".to_owned();
        assert!(tab.keeps(&idle));
    }
}
