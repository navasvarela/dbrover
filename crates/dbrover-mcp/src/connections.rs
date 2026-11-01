//! One connection per data source, opened on first use and closed after a while unused, so
//! a server that lives as long as a client session doesn't hold connections all that time.
//! A DuckDB file's is closed as soon as each call ends ([`Keep`]).

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dbrover_core::{
    Connection, ConnectionConfig, DataSource, DataSourceId, ServerInfo, describe_error,
};
use dbrover_engine::{OpenError, PasswordCommands, Tunnels, open_data_source_for_mcp};
use dbrover_store::SecretStore;
use tokio::sync::{MappedMutexGuard, Mutex, MutexGuard};

/// "A few idle minutes", as the plan puts it: long enough that an assistant's run of calls
/// reuses one connection, short enough not to hold it through a whole idle session.
const IDLE_AFTER: Duration = Duration::from_secs(5 * 60);
/// How often idle connections are looked for. Closing one up to this late costs nothing.
const SWEEP_EVERY: Duration = Duration::from_secs(30);

pub(crate) struct Connections {
    secrets: Arc<dyn SecretStore>,
    tunnels: Tunnels,
    /// Only those the user trusted in the app: the server can't ask anyone.
    commands: PasswordCommands,
    slots: HashMap<DataSourceId, Slot>,
}

struct Slot {
    data_source: DataSource,
    keep: Keep,
    /// Held for a whole call, so calls on one data source run one at a time, each in its own
    /// transaction on the one connection.
    open: Mutex<Option<OpenConnection>>,
    /// What the server reported when last connected, for `list_data_sources`.
    server: std::sync::Mutex<Option<ServerInfo>>,
}

/// How long a connection stays open once a call is done with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// Until unused for [`IDLE_AFTER`], so an assistant's run of calls reuses it.
    UntilIdle,
    /// Only while a call runs. A program that has a DuckDB file open, even read-only, locks
    /// out every program that would write it, the app included: held until idle, the file
    /// couldn't be opened for writing for minutes after an assistant's last question.
    DuringCalls,
}

impl Keep {
    fn of(config: &ConnectionConfig) -> Self {
        match config {
            ConnectionConfig::DuckDb(_) => Self::DuringCalls,
            _ => Self::UntilIdle,
        }
    }
}

struct OpenConnection {
    connection: Box<dyn Connection>,
    last_used: Instant,
    /// Set when its state is unknown, as after a timeout that a cancel didn't end or a
    /// failed rollback; the next call opens a new one instead.
    broken: bool,
}

/// A data source's connection, borrowed for one call.
pub(crate) struct Checkout<'a> {
    open: MappedMutexGuard<'a, OpenConnection>,
}

impl Checkout<'_> {
    pub(crate) fn discard(&mut self) {
        self.open.broken = true;
    }
}

impl Deref for Checkout<'_> {
    type Target = dyn Connection;

    fn deref(&self) -> &Self::Target {
        self.open.connection.as_ref()
    }
}

impl DerefMut for Checkout<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.open.connection.as_mut()
    }
}

impl Drop for Checkout<'_> {
    fn drop(&mut self) {
        self.open.last_used = Instant::now();
    }
}

impl Connections {
    pub(crate) fn new(
        secrets: Arc<dyn SecretStore>,
        tunnels: Tunnels,
        commands: PasswordCommands,
        data_sources: &[DataSource],
    ) -> Arc<Self> {
        let slots = data_sources
            .iter()
            .map(|data_source| {
                let slot = Slot {
                    data_source: data_source.clone(),
                    keep: Keep::of(&data_source.config),
                    open: Mutex::new(None),
                    server: std::sync::Mutex::new(None),
                };
                (data_source.id, slot)
            })
            .collect();
        Arc::new(Self {
            secrets,
            tunnels,
            commands,
            slots,
        })
    }

    /// Closes connections unused for [`IDLE_AFTER`], until the last other handle is dropped.
    pub(crate) fn sweep_idle(self: &Arc<Self>) {
        let connections = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(SWEEP_EVERY);
            loop {
                ticks.tick().await;
                let Some(connections) = connections.upgrade() else {
                    return;
                };
                connections.close_idle().await;
            }
        });
    }

    async fn close_idle(&self) {
        for slot in self.slots.values() {
            // A slot in use isn't idle; it is looked at again next time.
            let Ok(mut open) = slot.open.try_lock() else {
                continue;
            };
            if open
                .as_ref()
                .is_some_and(|open| open.broken || open.last_used.elapsed() >= IDLE_AFTER)
                && let Some(open) = open.take()
            {
                tracing::info!(data_source = %slot.data_source.name, "closing an idle connection");
                close(&slot.data_source, open.connection).await;
            }
        }
    }

    /// Closes the data source's connection now that a call is done with it, if it is kept only
    /// during calls. Another call may have taken it up meanwhile; it is closed after that one.
    pub(crate) async fn release(&self, id: DataSourceId) {
        let Some(slot) = self
            .slots
            .get(&id)
            .filter(|slot| slot.keep == Keep::DuringCalls)
        else {
            return;
        };
        let open = slot.open.lock().await.take();
        if let Some(open) = open {
            close(&slot.data_source, open.connection).await;
        }
    }

    /// Closes every connection, waiting for calls in progress to finish first.
    pub(crate) async fn close_all(&self) {
        for slot in self.slots.values() {
            if let Some(open) = slot.open.lock().await.take() {
                close(&slot.data_source, open.connection).await;
            }
        }
    }

    /// The data source's connection, opening it if needed. Waits while another call uses it.
    ///
    /// # Errors
    ///
    /// [`OpenError`] if connecting fails.
    pub(crate) async fn checkout(&self, id: DataSourceId) -> Result<Checkout<'_>, OpenError> {
        let Some(slot) = self.slots.get(&id) else {
            // Callers look data sources up in the same served project the slots come from.
            unreachable!("no connection slot for a data source of the served project");
        };
        let mut guard = slot.open.lock().await;
        if let Some(broken) = guard.take_if(|open| open.broken) {
            close(&slot.data_source, broken.connection).await;
        }
        let open = match guard.take() {
            Some(open) => open,
            None => OpenConnection {
                connection: self.connect(slot).await?,
                last_used: Instant::now(),
                broken: false,
            },
        };
        Ok(Checkout {
            open: MutexGuard::map(guard, |slot| slot.insert(open)),
        })
    }

    async fn connect(&self, slot: &Slot) -> Result<Box<dyn Connection>, OpenError> {
        match &slot.data_source.mcp_login {
            Some(login) => tracing::info!(
                data_source = %slot.data_source.name,
                "connecting as the MCP login {}",
                login.user
            ),
            None => tracing::info!(data_source = %slot.data_source.name, "connecting"),
        }
        let connection = open_data_source_for_mcp(
            &slot.data_source,
            &self.secrets,
            &self.tunnels,
            &self.commands,
        )
        .await?;
        tracing::info!(
            data_source = %slot.data_source.name,
            "connected to {}",
            connection.server_info()
        );
        if let Ok(mut server) = slot.server.lock() {
            *server = Some(connection.server_info().clone());
        }
        Ok(connection)
    }

    /// What the data source's server reported when last connected, if it has been.
    pub(crate) fn server_info(&self, id: DataSourceId) -> Option<ServerInfo> {
        self.slots
            .get(&id)
            .and_then(|slot| slot.server.lock().ok()?.clone())
    }
}

async fn close(data_source: &DataSource, connection: Box<dyn Connection>) {
    if let Err(error) = connection.close().await {
        tracing::warn!(
            data_source = %data_source.name,
            "could not close the connection cleanly: {}",
            describe_error(&error)
        );
    }
}
