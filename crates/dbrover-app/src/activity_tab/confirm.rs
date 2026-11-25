//! The question before a stop: which session on which data source, whose it is, what it is
//! doing, and the statement that will be sent.

use dbrover_core::{ServerSession, StopKind, spoken_duration};
use egui::{Id, RichText};

use super::{ActivityAction, ActivityTab, ActivityView};
use crate::dialog::{self, DialogSize};
use crate::{form, icons};

/// How much of the session's statement the question shows.
const STATEMENT_SHOWN: usize = 300;

pub(super) fn show(
    tab: &mut ActivityTab,
    ctx: &egui::Context,
    view: &ActivityView<'_>,
) -> Option<ActivityAction> {
    let asking = tab.asking.as_ref()?;
    let session = &asking.session;
    let kind = asking.kind;
    let stop = session.stop(kind)?;
    let id = Id::new(("activity_confirm", tab.data_source));
    let modal = dialog::modal(ctx, id, DialogSize::Confirm).show(ctx, |ui| {
        form::Header::new(icons::WARNING, &heading(kind, session, view.name))
            .danger()
            .show(ui);
        let application = tab
            .activity
            .as_ref()
            .and_then(|activity| activity.own(&session.id))
            .map(|own| super::own_name(own, view.consoles))
            .or_else(|| session.application.clone());
        ui.label(whose(session, application.as_deref()));
        if let Some(statement) = &session.statement {
            let shown: String = statement.text.chars().take(STATEMENT_SHOWN).collect();
            let more = if shown.len() < statement.text.len() {
                "\u{2026}"
            } else {
                ""
            };
            ui.label(RichText::new(format!("{shown}{more}")).monospace());
        }
        if kind == StopKind::EndSession {
            ui.label("Its open transaction is rolled back and its locks are released.");
        }
        ui.weak("This sends:");
        ui.label(RichText::new(&stop.statement).monospace());
        form::footer(ui, |ui| {
            if form::danger_button(ui, kind.label(), true).clicked() {
                Some(true)
            } else if ui.button("Keep it").clicked() {
                Some(false)
            } else {
                None
            }
        })
    });
    let answer = if modal.should_close() {
        Some(false)
    } else {
        modal.inner
    }?;
    let asking = tab.asking.take()?;
    answer.then(|| tab.send_stop(asking.session.id, asking.kind))
}

/// *Kill session 4211 on "production"?*
fn heading(kind: StopKind, session: &ServerSession, data_source: &str) -> String {
    match kind {
        StopKind::CancelQuery => format!(
            "Cancel the statement of session {} on \u{201c}{data_source}\u{201d}?",
            session.id
        ),
        StopKind::EndSession => format!(
            "Kill session {} on \u{201c}{data_source}\u{201d}?",
            session.id
        ),
    }
}

/// *app on orders-api, idle in transaction for 41 min, transaction open for 41 min.*
fn whose(session: &ServerSession, application: Option<&str>) -> String {
    let user = session.user.as_deref().unwrap_or("An unknown user");
    let mut text = match application {
        Some(application) => format!("{user} on {application}"),
        None => user.to_owned(),
    };
    text.push_str(&format!(", {}", session.state.label()));
    if let Some(time) = session.in_state_for {
        text.push_str(&format!(" for {}", spoken_duration(time)));
    }
    if let Some(time) = session.transaction_for {
        text.push_str(&format!(", transaction open for {}", spoken_duration(time)));
    }
    text.push('.');
    text
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dbrover_core::{SessionId, SessionState};

    use super::*;

    #[test]
    fn names_the_session_the_data_source_and_whose_it_is() {
        let session = ServerSession {
            id: SessionId::Number(4211),
            user: Some("app".to_owned()),
            application: Some("orders-api".to_owned()),
            state: SessionState::IdleInTransaction,
            in_state_for: Some(Duration::from_secs(41 * 60)),
            transaction_for: Some(Duration::from_secs(42 * 60)),
            ..ServerSession::default()
        };

        assert_eq!(
            heading(StopKind::EndSession, &session, "production"),
            "Kill session 4211 on \u{201c}production\u{201d}?"
        );
        assert_eq!(
            whose(&session, session.application.as_deref()),
            "app on orders-api, idle in transaction for 41 min, transaction open for 42 min."
        );
    }
}
