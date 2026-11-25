//! The selected session's details under the list: everything read about it, its statement, the
//! lock it waits for and who holds it, and its buttons.

use dbrover_core::{ServerSession, StopKind, spoken_duration};
use dbrover_engine::Activity;
use egui::{Grid, RichText, ScrollArea, Ui};

use super::stops::{self, StopContext};
use super::toolbar::icon_button;
use super::{ActivityAction, ActivityTab, ActivityView, own_name, state_text};
use crate::{icons, theme::Palette};

pub(super) fn show(
    tab: &mut ActivityTab,
    ui: &mut Ui,
    activity: &Activity,
    session: &ServerSession,
    listed: bool,
    view: &ActivityView<'_>,
) -> Option<ActivityAction> {
    let palette = &Palette::for_visuals(ui.visuals());
    ui.horizontal(|ui| {
        ui.strong(format!("Session {}", session.id));
        ui.label(state_text(session.state, palette));
        if !listed {
            ui.colored_label(palette.text_weak, "Ended: not in the last read");
        }
    });
    let action = buttons(tab, ui, activity, session, listed, view);
    ScrollArea::vertical()
        .id_salt(("activity_detail", tab.data_source))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            facts(ui, activity, session, view);
            statement(ui, session);
        });
    action
}

fn facts(ui: &mut Ui, activity: &Activity, session: &ServerSession, view: &ActivityView<'_>) {
    let own = activity
        .own(&session.id)
        .map(|own| own_name(own, view.consoles));
    let blocked_by = (!session.blocked_by.is_empty()).then(|| {
        session
            .blocked_by
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    });
    let mut rows: Vec<(&str, Option<String>)> = vec![
        ("User", session.user.clone()),
        ("Client", session.client.clone()),
        ("Database", session.database.clone()),
        ("Application", own.or_else(|| session.application.clone())),
        (
            "In this state for",
            session.in_state_for.map(spoken_duration),
        ),
        ("The server says", session.engine_state.clone()),
        (
            "Transaction open for",
            session.transaction_for.map(spoken_duration),
        ),
        ("Waits on", session.wait.clone()),
        ("Waits for", session.lock.as_ref().map(ToString::to_string)),
        ("Held by", blocked_by),
    ];
    rows.extend(
        session
            .extra
            .iter()
            .map(|(label, value)| (label.as_str(), Some(value.clone()))),
    );
    Grid::new(("activity_facts", session.id.to_string()))
        .num_columns(2)
        .spacing([16.0, 2.0])
        .show(ui, |ui| {
            for (label, value) in rows {
                if let Some(value) = value {
                    ui.weak(label);
                    ui.label(value);
                    ui.end_row();
                }
            }
        });
}

fn statement(ui: &mut Ui, session: &ServerSession) {
    let Some(statement) = &session.statement else {
        ui.weak("No statement shown.");
        return;
    };
    ui.add_space(ui.spacing().item_spacing.y);
    ui.weak(if statement.running {
        "Running"
    } else {
        "Last ran"
    });
    ui.label(RichText::new(&statement.text).monospace());
    if statement.cut_short {
        ui.weak("Cut short by the server, which keeps only the start of a long statement.");
    }
}

/// Copy, Open in console, Go to blocker, then Cancel query and Kill…, each disabled with the
/// reason when it can't be used.
fn buttons(
    tab: &mut ActivityTab,
    ui: &mut Ui,
    activity: &Activity,
    session: &ServerSession,
    listed: bool,
    view: &ActivityView<'_>,
) -> Option<ActivityAction> {
    let text = session
        .statement
        .as_ref()
        .map(|statement| statement.text.clone());
    let context = StopContext {
        data_source: view.name,
        safety: view.safety,
        own: activity.own(&session.id),
        others_stopped_by: activity.server.others_stopped_by.as_deref(),
    };
    let mut action = None;
    ui.horizontal(|ui| {
        ui.add_enabled_ui(text.is_some(), |ui| {
            if icon_button(ui, icons::COPY, "Copy statement").clicked() {
                action = text.clone().map(ActivityAction::Copy);
            }
            if icon_button(ui, icons::TERMINAL_WINDOW, "Open in console")
                .on_hover_text("In a new console, not run")
                .clicked()
            {
                action = text
                    .clone()
                    .map(|text| ActivityAction::OpenInConsole(tab.data_source, text));
            }
        });
        let blocker = session.blocked_by.first().and_then(|id| {
            activity
                .server
                .sessions
                .iter()
                .find(|listed| &listed.id == id)
        });
        if let Some(blocker) = blocker
            && icon_button(ui, icons::ARROW_UP, "Go to blocker").clicked()
        {
            tab.selected = Some(blocker.clone());
        }
        ui.separator();
        for (kind, glyph, label) in [
            (StopKind::CancelQuery, icons::STOP, "Cancel query"),
            (StopKind::EndSession, icons::X_CIRCLE, "Kill\u{2026}"),
        ] {
            let offered = if listed {
                stops::offered(&context, session, kind)
            } else {
                Err("The session has ended.".to_owned())
            };
            let busy = tab.stopping.is_some();
            let response = ui
                .add_enabled_ui(offered.is_ok() && !busy, |ui| icon_button(ui, glyph, label))
                .inner;
            let response = match &offered {
                Err(reason) => response.on_disabled_hover_text(reason),
                Ok(()) => response,
            };
            if response.clicked()
                && let Some(stop) = tab.request_stop(session, kind, view.safety)
            {
                action = Some(stop);
            }
        }
        if let Some((id, kind)) = &tab.stopping
            && id == &session.id
        {
            ui.spinner();
            ui.weak(format!("Sending {}\u{2026}", kind.label()));
        }
    });
    action
}
