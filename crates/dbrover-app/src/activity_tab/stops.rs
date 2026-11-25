//! Which stops a session's buttons offer, why one isn't, and which ask first: the data source's
//! safety level applied to Cancel query and Kill (W15, `docs/server-activity.md`).

use dbrover_core::{SafetyLevel, ServerSession, SessionState, StopKind};
use dbrover_engine::OwnSession;

/// What the tab knows about where a session is, to decide its buttons.
pub(super) struct StopContext<'a> {
    pub(super) data_source: &'a str,
    pub(super) safety: SafetyLevel,
    pub(super) own: Option<OwnSession>,
    /// What would let the login stop other users' sessions, when it may stop only its own.
    pub(super) others_stopped_by: Option<&'a str>,
}

/// Whether `kind` may be sent to `session`; otherwise why not, for the button's hover.
pub(super) fn offered(
    context: &StopContext<'_>,
    session: &ServerSession,
    kind: StopKind,
) -> Result<(), String> {
    if context.safety == SafetyLevel::ReadOnly {
        return Err(format!(
            "\u{201c}{}\u{201d} is read-only, so its sessions aren't stopped.",
            context.data_source
        ));
    }
    if context.own == Some(OwnSession::ThisTab) {
        return Err("This is this tab's own session, which reads the list.".to_owned());
    }
    if session.stop(kind).is_some() {
        return Ok(());
    }
    let running = matches!(session.state, SessionState::Active | SessionState::Waiting);
    Err(match (kind, context.others_stopped_by) {
        (StopKind::CancelQuery, _) if !running => "Nothing is running to cancel.".to_owned(),
        (_, Some(privilege)) => {
            format!("Stopping another user's session needs {privilege}.")
        }
        (StopKind::CancelQuery, None) => {
            "This database can't cancel another session's statement; Kill ends the session."
                .to_owned()
        }
        (StopKind::EndSession, None) => "This session can't be stopped from here.".to_owned(),
    })
}

/// Whether `kind` asks before it is sent: both on a careful data source, Kill everywhere, since
/// it rolls back someone's transaction. Cancel on a normal data source goes at once.
pub(super) fn asks_first(safety: SafetyLevel, kind: StopKind) -> bool {
    safety == SafetyLevel::Careful || kind == StopKind::EndSession
}

#[cfg(test)]
mod tests {
    use dbrover_core::{SessionId, SessionStop};

    use super::*;

    fn context(safety: SafetyLevel) -> StopContext<'static> {
        StopContext {
            data_source: "production",
            safety,
            own: None,
            others_stopped_by: None,
        }
    }

    fn running(stops: &[StopKind]) -> ServerSession {
        ServerSession {
            id: SessionId::Number(7),
            state: SessionState::Active,
            stops: stops
                .iter()
                .map(|&kind| SessionStop {
                    kind,
                    statement: "…".to_owned(),
                })
                .collect(),
            ..ServerSession::default()
        }
    }

    #[test]
    fn offers_what_the_driver_wrote_unless_read_only_or_the_tabs_own() {
        let both = running(&[StopKind::CancelQuery, StopKind::EndSession]);
        assert_eq!(
            offered(&context(SafetyLevel::Careful), &both, StopKind::EndSession),
            Ok(())
        );
        assert_eq!(
            offered(
                &context(SafetyLevel::ReadOnly),
                &both,
                StopKind::CancelQuery
            ),
            Err("\u{201c}production\u{201d} is read-only, so its sessions aren't stopped.".into())
        );
        let own = StopContext {
            own: Some(OwnSession::ThisTab),
            ..context(SafetyLevel::Normal)
        };
        assert!(offered(&own, &both, StopKind::EndSession).is_err());
        let console = StopContext {
            own: Some(OwnSession::Console(dbrover_engine::ConsoleId(1))),
            ..context(SafetyLevel::Normal)
        };
        assert_eq!(offered(&console, &both, StopKind::EndSession), Ok(()));
    }

    #[test]
    fn says_why_a_stop_is_missing() {
        let idle = ServerSession {
            state: SessionState::Idle,
            ..running(&[StopKind::EndSession])
        };
        assert_eq!(
            offered(&context(SafetyLevel::Normal), &idle, StopKind::CancelQuery),
            Err("Nothing is running to cancel.".into())
        );
        let others = StopContext {
            others_stopped_by: Some("pg_signal_backend"),
            ..context(SafetyLevel::Normal)
        };
        assert_eq!(
            offered(&others, &running(&[]), StopKind::EndSession),
            Err("Stopping another user's session needs pg_signal_backend.".into())
        );
        assert!(
            offered(
                &context(SafetyLevel::Normal),
                &running(&[StopKind::EndSession]),
                StopKind::CancelQuery
            )
            .unwrap_err()
            .contains("Kill ends the session")
        );
    }

    #[test]
    fn kill_always_asks_and_cancel_only_on_careful() {
        let cases = [
            (SafetyLevel::Normal, StopKind::CancelQuery, false),
            (SafetyLevel::Normal, StopKind::EndSession, true),
            (SafetyLevel::Careful, StopKind::CancelQuery, true),
            (SafetyLevel::Careful, StopKind::EndSession, true),
        ];
        for (safety, kind, asks) in cases {
            assert_eq!(asks_first(safety, kind), asks, "{safety:?} {kind:?}");
        }
    }
}
