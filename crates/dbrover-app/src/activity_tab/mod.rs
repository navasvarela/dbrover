//! Server activity: a tab per data source listing what its server is doing, who waits on whose
//! locks, and Cancel query and Kill under the data source's safety level (W15,
//! `docs/server-activity.md`).

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use dbrover_core::{DataSourceId, SafetyLevel, ServerSession, SessionId, SessionState, StopKind};
use dbrover_engine::{Activity, Command, ConsoleId, Event, OwnSession};
use egui::{RichText, Ui};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::repaint;
use refresh::Refresh;

mod confirm;
mod detail;
mod list;
mod refresh;
mod stops;
mod toolbar;

/// The share of the tab's height the list keeps while a session's details show below it.
const LIST_SHARE: f32 = 0.55;
const TIME_FORMAT: &str = "%H:%M:%S";

/// Every activity tab, by its data source, which is also the tab's key in the dock.
#[derive(Default)]
pub struct ActivityTabs {
    tabs: BTreeMap<DataSourceId, ActivityTab>,
    /// Tabs closed in the dock, whose connections the app closes.
    closed: Vec<DataSourceId>,
}

impl ActivityTabs {
    /// Opens `data_source`'s tab reading every `every` (paused when `None`). Returns whether
    /// it is new; an open one is left as it is.
    pub fn open(&mut self, data_source: DataSourceId, every: Option<Duration>) -> bool {
        if self.tabs.contains_key(&data_source) {
            return false;
        }
        self.tabs
            .insert(data_source, ActivityTab::new(data_source, every));
        true
    }

    /// Forgets the tab; its connection is closed by the command [`ActivityTabs::take_closed`]
    /// hands the app.
    pub fn close(&mut self, data_source: DataSourceId) {
        if self.tabs.remove(&data_source).is_some() {
            self.closed.push(data_source);
        }
    }

    /// The commands closing the connections of the tabs closed since the last call.
    pub fn take_closed(&mut self) -> Vec<Command> {
        self.closed.drain(..).map(Command::CloseActivity).collect()
    }

    pub fn get_mut(&mut self, data_source: DataSourceId) -> Option<&mut ActivityTab> {
        self.tabs.get_mut(&data_source)
    }

    pub fn apply(&mut self, event: &Event) {
        for tab in self.tabs.values_mut() {
            tab.apply(event);
        }
    }
}

/// What the tab needs from the app each frame.
pub struct ActivityView<'a> {
    pub name: &'a str,
    pub safety: SafetyLevel,
    /// Open consoles' titles, to name DbRover's own sessions.
    pub consoles: &'a HashMap<ConsoleId, String>,
}

/// What the user asked of the tab this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityAction {
    Read(DataSourceId),
    Stop {
        data_source: DataSourceId,
        session: SessionId,
        kind: StopKind,
    },
    /// Open a statement in a new console of the data source, not run.
    OpenInConsole(DataSourceId, String),
    Copy(String),
}

/// A stop waiting for the user's yes.
struct Asking {
    session: ServerSession,
    kind: StopKind,
}

/// The outcome of the last stop, shown under the toolbar.
struct Notice {
    text: String,
    failed: bool,
}

pub struct ActivityTab {
    data_source: DataSourceId,
    refresh: Refresh,
    /// The last list read, kept while a read fails so the tab doesn't empty.
    activity: Option<Activity>,
    read_at: Option<Timestamp>,
    /// Why the last read failed, until one succeeds.
    error: Option<String>,
    show_idle: bool,
    show_background: bool,
    filter: String,
    /// The selected session as last read; kept once it has ended, to say so.
    selected: Option<ServerSession>,
    asking: Option<Asking>,
    stopping: Option<(SessionId, StopKind)>,
    notice: Option<Notice>,
}

impl ActivityTab {
    fn new(data_source: DataSourceId, every: Option<Duration>) -> Self {
        Self {
            data_source,
            refresh: Refresh::new(every),
            activity: None,
            read_at: None,
            error: None,
            show_idle: false,
            show_background: false,
            filter: String::new(),
            selected: None,
            asking: None,
            stopping: None,
            notice: None,
        }
    }

    fn apply(&mut self, event: &Event) {
        match event {
            Event::ActivityRead {
                data_source,
                result,
            } if *data_source == self.data_source => {
                self.refresh.answered(Instant::now());
                match result {
                    Ok(activity) => self.read(activity.clone()),
                    Err(error) => self.error = Some(error.clone()),
                }
            }
            Event::SessionStopped {
                data_source,
                session,
                kind,
                result,
            } if *data_source == self.data_source => {
                self.stopping = None;
                self.notice = Some(stopped_notice(session, *kind, result));
                self.refresh.read_now();
            }
            // A connection that fails answers no read; the timer tries again, connecting anew.
            Event::ActivitySessionChanged {
                data_source,
                state: dbrover_engine::SessionState::Failed(reason),
            } if *data_source == self.data_source => {
                self.refresh.answered(Instant::now());
                self.error = Some(reason.clone());
            }
            _ => {}
        }
    }

    fn read(&mut self, activity: Activity) {
        if let Some(selected) = &mut self.selected
            && let Some(now) = activity
                .server
                .sessions
                .iter()
                .find(|session| session.id == selected.id)
        {
            *selected = now.clone();
        }
        self.activity = Some(activity);
        self.read_at = Some(Timestamp::now());
        self.error = None;
    }

    /// Whether the selected session was in the last read.
    fn selected_is_listed(&self) -> bool {
        match (&self.selected, &self.activity) {
            (Some(selected), Some(activity)) => activity
                .server
                .sessions
                .iter()
                .any(|session| session.id == selected.id),
            _ => false,
        }
    }

    pub fn show(&mut self, ui: &mut Ui, view: &ActivityView<'_>) -> Option<ActivityAction> {
        let mut action = self.timer(ui);
        toolbar::show(self, ui);
        self.banners(ui, view);
        ui.separator();
        let Some(activity) = self.activity.clone() else {
            if self.error.is_none() {
                ui.spinner();
            }
            return action;
        };
        let height = if self.selected.is_some() {
            ui.available_height() * LIST_SHARE
        } else {
            f32::INFINITY
        };
        if let Some(clicked) = list::show(self, ui, &activity, view, height) {
            self.selected = Some(clicked);
        }
        if let Some(selected) = self.selected.clone() {
            ui.separator();
            let listed = self.selected_is_listed();
            action = detail::show(self, ui, &activity, &selected, listed, view).or(action);
        }
        confirm::show(self, ui.ctx(), view).or(action)
    }

    /// Sends a read when one is due, and wakes the tab for the next. Only runs while the tab
    /// is drawn, which is while it is shown.
    fn timer(&mut self, ui: &Ui) -> Option<ActivityAction> {
        let now = Instant::now();
        let due = self.refresh.is_due(now);
        if due {
            self.refresh.sent();
        }
        if let Some(left) = self.refresh.left(now) {
            repaint::request_repaint_after_pause(ui.ctx(), left);
        }
        due.then_some(ActivityAction::Read(self.data_source))
    }

    /// What the login can't see, why the last read failed, and how the last stop went.
    fn banners(&self, ui: &mut Ui, view: &ActivityView<'_>) {
        let palette = &crate::theme::Palette::for_visuals(ui.visuals());
        if let Some(error) = &self.error {
            ui.colored_label(palette.error, format!("Could not read: {error}"));
        }
        if let Some(activity) = &self.activity {
            for restriction in &activity.server.restrictions {
                ui.colored_label(palette.warning, restriction_text(restriction));
            }
        }
        if view.safety == SafetyLevel::ReadOnly {
            ui.weak(format!(
                "\u{201c}{}\u{201d} is read-only: its sessions are shown, not stopped.",
                view.name
            ));
        }
        if let Some(notice) = &self.notice {
            let color = if notice.failed {
                palette.error
            } else {
                palette.success
            };
            ui.colored_label(color, &notice.text);
        }
    }

    fn read_time(&self) -> Option<String> {
        let read_at = self.read_at?;
        Some(
            read_at
                .to_zoned(TimeZone::system())
                .strftime(TIME_FORMAT)
                .to_string(),
        )
    }

    /// Asks first, or sends at once, as the safety level says.
    fn request_stop(
        &mut self,
        session: &ServerSession,
        kind: StopKind,
        safety: SafetyLevel,
    ) -> Option<ActivityAction> {
        if stops::asks_first(safety, kind) {
            self.asking = Some(Asking {
                session: session.clone(),
                kind,
            });
            return None;
        }
        Some(self.send_stop(session.id.clone(), kind))
    }

    fn send_stop(&mut self, session: SessionId, kind: StopKind) -> ActivityAction {
        self.stopping = Some((session.clone(), kind));
        self.notice = None;
        ActivityAction::Stop {
            data_source: self.data_source,
            session,
            kind,
        }
    }
}

/// How DbRover names one of its own sessions, in the Application column.
fn own_name(own: OwnSession, consoles: &HashMap<ConsoleId, String>) -> String {
    match own {
        OwnSession::ThisTab => "DbRover: this tab".to_owned(),
        OwnSession::Explorer => "DbRover: explorer".to_owned(),
        OwnSession::DataSearch => "DbRover: data search".to_owned(),
        OwnSession::Console(console) => match consoles.get(&console) {
            Some(title) => format!("DbRover: {title}"),
            None => "DbRover: a console".to_owned(),
        },
    }
}

fn restriction_text(restriction: &dbrover_core::Restriction) -> String {
    let shown_by = &restriction.shown_by;
    match restriction.hidden {
        dbrover_core::Hidden::OthersSessions => format!(
            "Only this login's own sessions are listed. Seeing other users' needs {shown_by}."
        ),
        dbrover_core::Hidden::OthersStatements => {
            format!(
                "Statements this login may not read are left out: showing them needs {shown_by}."
            )
        }
        dbrover_core::Hidden::Blocking => {
            format!("Who blocks whom can't be read: that needs {shown_by}.")
        }
    }
}

fn stopped_notice(
    session: &SessionId,
    kind: StopKind,
    result: &Result<dbrover_core::StopOutcome, String>,
) -> Notice {
    use dbrover_core::StopOutcome;
    let (text, failed) = match (result, kind) {
        (Ok(StopOutcome::Stopped), StopKind::CancelQuery) => (
            format!("Cancelled the statement of session {session}."),
            false,
        ),
        (Ok(StopOutcome::Stopped), StopKind::EndSession) => {
            (format!("Ended session {session}."), false)
        }
        (Ok(StopOutcome::AlreadyEnded), StopKind::CancelQuery) => (
            format!("Session {session}'s statement had already ended; nothing was cancelled."),
            false,
        ),
        (Ok(StopOutcome::AlreadyEnded), StopKind::EndSession) => {
            (format!("Session {session} had already ended."), false)
        }
        (Err(error), _) => (
            format!("{} on session {session} failed: {error}", kind.label()),
            true,
        ),
    };
    Notice { text, failed }
}

/// The colour a state is written in: waiting and holding a transaction open warn, idle and
/// the server's own work are quiet.
fn state_text(state: SessionState, palette: &crate::theme::Palette) -> RichText {
    let text = RichText::new(state.label());
    match state {
        SessionState::Active => text.color(palette.primary),
        SessionState::Waiting => text.color(palette.error),
        SessionState::IdleInTransaction => text.color(palette.warning),
        SessionState::Idle | SessionState::Background | SessionState::Hidden => {
            text.color(palette.text_weak)
        }
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::StopOutcome;

    use super::*;

    #[test]
    fn says_how_a_stop_went() {
        let id = SessionId::Number(4211);
        let notice = |kind, result| stopped_notice(&id, kind, &result);

        assert_eq!(
            notice(StopKind::EndSession, Ok(StopOutcome::Stopped)).text,
            "Ended session 4211."
        );
        assert_eq!(
            notice(StopKind::EndSession, Ok(StopOutcome::AlreadyEnded)).text,
            "Session 4211 had already ended."
        );
        let failed = notice(StopKind::CancelQuery, Err("must be superuser".to_owned()));
        assert!(failed.failed);
        assert_eq!(
            failed.text,
            "Cancel query on session 4211 failed: must be superuser"
        );
    }

    #[test]
    fn names_dbrovers_own_sessions() {
        let consoles = HashMap::from([(ConsoleId(3), "Orders.sql".to_owned())]);
        assert_eq!(
            own_name(OwnSession::Console(ConsoleId(3)), &consoles),
            "DbRover: Orders.sql"
        );
        assert_eq!(
            own_name(OwnSession::ThisTab, &consoles),
            "DbRover: this tab"
        );
        assert_eq!(
            own_name(OwnSession::Console(ConsoleId(9)), &consoles),
            "DbRover: a console"
        );
    }

    #[test]
    fn keeps_the_list_when_a_read_fails_and_tells_why() {
        let data_source = DataSourceId::generate();
        let mut tabs = ActivityTabs::default();
        assert!(tabs.open(data_source, Some(Duration::from_secs(5))));
        assert!(!tabs.open(data_source, None));
        let read = Activity {
            server: dbrover_core::ServerActivity::default(),
            own: Vec::new(),
        };
        tabs.apply(&Event::ActivityRead {
            data_source,
            result: Ok(read.clone()),
        });
        tabs.apply(&Event::ActivityRead {
            data_source,
            result: Err("timed out".to_owned()),
        });

        let tab = tabs.get_mut(data_source).unwrap();
        assert_eq!(tab.activity, Some(read));
        assert_eq!(tab.error.as_deref(), Some("timed out"));

        tabs.close(data_source);
        let closed = tabs.take_closed();
        assert!(
            matches!(closed.as_slice(), [Command::CloseActivity(id)] if *id == data_source),
            "{closed:?}"
        );
        assert!(tabs.take_closed().is_empty());
    }
}
