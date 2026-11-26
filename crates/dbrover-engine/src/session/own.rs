//! The ids the server gives DbRover's own connections, so a Server activity tab can name them
//! (*DbRover: Orders.sql*) and refuse to stop its own (W15).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dbrover_core::{Connection, DataSourceId, SessionId, describe_error};

use super::{SessionContext, SessionOwner};
use crate::OwnSession;

/// Shared by every session: each says its connection's id once connected, and forgets it
/// when the connection ends.
#[derive(Debug, Clone, Default)]
pub(crate) struct OwnSessions(Arc<Mutex<HashMap<SessionOwner, Known>>>);

#[derive(Debug, Clone)]
struct Known {
    data_source: DataSourceId,
    id: SessionId,
}

impl OwnSessions {
    /// Asks the connection its id and keeps it. A driver that can't say, or a server that
    /// doesn't answer, leaves the session unnamed: naming is a convenience, not worth failing
    /// a connection for.
    pub(super) async fn learn(&self, context: &SessionContext, connection: &mut dyn Connection) {
        match connection.own_session().await {
            Ok(Some(id)) => {
                self.lock().insert(
                    context.owner,
                    Known {
                        data_source: context.data_source.id,
                        id,
                    },
                );
            }
            Ok(None) => {}
            Err(error) => tracing::debug!(
                data_source = %context.name(),
                "could not ask the connection's session id: {}",
                describe_error(&error)
            ),
        }
    }

    pub(super) fn forget(&self, owner: SessionOwner) {
        self.lock().remove(&owner);
    }

    /// The id of `owner`'s connection, while it is connected.
    pub(super) fn of(&self, owner: SessionOwner) -> Option<SessionId> {
        self.lock().get(&owner).map(|known| known.id.clone())
    }

    /// DbRover's sessions on `data_source`, named as `reader`, an activity tab, sees them.
    ///
    /// Only the data source's own: two data sources may reach two servers whose ids overlap,
    /// and a session named after the wrong server's would mislead.
    pub(super) fn on(
        &self,
        data_source: DataSourceId,
        reader: SessionOwner,
    ) -> Vec<(SessionId, OwnSession)> {
        self.lock()
            .iter()
            .filter(|(_, known)| known.data_source == data_source)
            .map(|(owner, known)| {
                let own = match *owner {
                    owner if owner == reader => OwnSession::ThisTab,
                    SessionOwner::Console(console) => OwnSession::Console(console),
                    SessionOwner::DataSearch(_) => OwnSession::DataSearch,
                    SessionOwner::Explorer(_) | SessionOwner::Activity(_) => OwnSession::Explorer,
                };
                (known.id.clone(), own)
            })
            .collect()
    }

    /// The map is only ever held for a lookup or an insert, so a panic while holding it
    /// can't leave it half-changed; a poisoned lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, HashMap<SessionOwner, Known>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
