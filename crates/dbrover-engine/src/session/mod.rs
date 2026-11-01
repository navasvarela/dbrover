//! Sessions: one task per live connection, owning it and answering requests in order.
//!
//! The explorer has one session per data source; each query console has its own, so a long
//! query never holds up browsing or another console.

mod catalog;
mod cost;
mod ddl;
mod edits;
mod export;
mod guard;
mod import;
mod loss;
mod parameters;
mod query;
mod transaction;
mod validate;

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dbrover_core::{
    CancelHandle, CatalogPath, ChildrenQuery, Connection, DataSource, DataSourceId, RelationName,
    SafetyLevel, TransactionStatus, describe_error,
};
use dbrover_language::QueryLanguage;
use dbrover_sql::{Dialect, ParameterValues};
use dbrover_store::{HistoryEntry, SecretStore};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::events::EventSink;
use crate::history::HistoryLog;
use crate::open::{Connector, OpenError, TypedSecrets, open};
use crate::password_commands::PasswordCommands;
use crate::passwords::{Prompting, SessionPasswords};
use crate::read_only;
use crate::tunnels::Tunnels;
use crate::{
    ConsoleId, Event, ExportAll, Exporter, RowEdit, RowsImport, RunId, RunOrigin, RunPurpose,
    ScanApproval, SessionState, TableExport, TransactionEnd, TransactionMode,
};
use guard::ReadOnly;
use loss::{Loss, Watched};

/// Work for a session's connection. Answered with events, in the order received.
#[derive(Debug)]
pub(crate) enum SessionRequest {
    Children(CatalogPath, ChildrenQuery),
    Definition(CatalogPath),
    Execute(Script),
    FetchMore(RunId),
    EndTransaction(TransactionEnd),
    DescribeTable(RelationName),
    DescribeTables(Vec<RelationName>),
    ApplyEdits(Vec<RowEdit>),
    DescribeRole(CatalogPath),
    ApplyDdl(Vec<String>),
    Ddl(CatalogPath),
    LoadTable(CatalogPath),
    /// Prepares statements without running them; see [`crate::Command::ValidateStatements`].
    Validate(Vec<String>),
    ExportAll(ExportAll),
    ExportTable(TableExport),
    ImportRows(RowsImport),
}

/// Statements to run in a console, in order.
#[derive(Debug)]
pub(crate) struct Script {
    pub(crate) run: RunId,
    pub(crate) statements: Vec<String>,
    /// The values of the statements' `:name` parameters.
    pub(crate) parameters: ParameterValues,
    pub(crate) transactions: TransactionMode,
    pub(crate) purpose: RunPurpose,
    pub(crate) origin: RunOrigin,
    pub(crate) scans: ScanApproval,
}

/// Who a session works for, which decides how its state changes are reported.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SessionOwner {
    Explorer(DataSourceId),
    Console(ConsoleId),
}

impl SessionOwner {
    /// Who the session's exports report to.
    pub(crate) fn exporter(self) -> Exporter {
        match self {
            Self::Explorer(data_source) => Exporter::DataSource(data_source),
            Self::Console(console) => Exporter::Console(console),
        }
    }
}

/// What a session needs from the engine.
pub(crate) struct Services {
    pub(crate) idle_ping_after: Duration,
    pub(crate) secrets: Arc<dyn SecretStore>,
    pub(crate) passwords: SessionPasswords,
    pub(crate) tunnels: Tunnels,
    pub(crate) commands: PasswordCommands,
    pub(crate) events: EventSink,
    pub(crate) history: HistoryLog,
}

/// A live connection, owned by its own task so a slow connect or query never holds up the
/// engine's command loop.
pub(crate) struct Session {
    /// Dropping this is the disconnect signal; nothing is ever sent on it. That way a session
    /// also disconnects when the engine drops it during shutdown.
    disconnect: oneshot::Sender<()>,
    /// Requests sent while still connecting wait here and run once connected.
    requests: UnboundedSender<SessionRequest>,
    /// The run most recently asked to stop. A watch rather than a request, because it must
    /// reach the task while it is busy with that very run.
    cancel: watch::Sender<Option<RunId>>,
    task: JoinHandle<()>,
}

impl Session {
    pub(crate) fn start(data_source: DataSource, owner: SessionOwner, services: Services) -> Self {
        let Services {
            idle_ping_after,
            secrets,
            passwords,
            tunnels,
            commands,
            events,
            history,
        } = services;
        let (disconnect, disconnect_signal) = oneshot::channel();
        let (requests, request_receiver) = mpsc::unbounded_channel();
        let (cancel, cancel_signal) = watch::channel(None);
        let read_only = (data_source.safety == SafetyLevel::ReadOnly).then(|| ReadOnly {
            mode: read_only::mode(
                data_source.config.driver(),
                read_only::Storage::of(&data_source.config),
            ),
        });
        let data_source = read_only::session_config(data_source);
        let context = SessionContext {
            data_source,
            idle_ping_after,
            read_only,
            owner,
            events,
            // What the user runs belongs in the history: a console's statements and the
            // explorer's DDL, never its catalog queries, which don't record anything.
            history,
        };
        let signals = Signals {
            disconnect: disconnect_signal,
            cancel: cancel_signal,
        };
        let task = tokio::spawn(run(
            context,
            Credentials {
                secrets,
                passwords,
                tunnels,
                commands,
            },
            signals,
            request_receiver,
        ));
        Self {
            disconnect,
            requests,
            cancel,
            task,
        }
    }

    /// Whether the session has ended, by disconnecting or by failing to connect.
    pub(crate) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub(crate) fn request(&self, request: SessionRequest) {
        // Fails only if the session has ended; its Disconnected or Failed event tells the UI
        // to forget anything it was waiting for.
        if self.requests.send(request).is_err() {
            tracing::debug!("dropped a request for a session that has ended");
        }
    }

    pub(crate) fn cancel(&self, run: RunId) {
        self.cancel.send_replace(Some(run));
    }

    /// Disconnects and waits until the connection is closed.
    pub(crate) async fn stop(self) {
        drop(self.disconnect);
        drop(self.requests);
        drop(self.cancel);
        if let Err(error) = self.task.await {
            tracing::error!("session task failed: {error}");
        }
    }
}

struct SessionContext {
    /// As connected to: a read-only data source's SQLite file is opened read-only.
    data_source: DataSource,
    /// How long the connection may sit unused before it is pinged ahead of its next request.
    idle_ping_after: Duration,
    /// Set for a read-only data source: every statement is checked, and runs read-only.
    read_only: Option<ReadOnly>,
    owner: SessionOwner,
    events: EventSink,
    history: HistoryLog,
}

impl SessionContext {
    fn name(&self) -> &str {
        &self.data_source.name
    }

    fn send(&self, event: Event) {
        self.events.send(event);
    }

    fn record(&self, entry: HistoryEntry) {
        self.history.record(entry);
    }

    /// The language of the commands this session runs.
    fn language(&self) -> QueryLanguage {
        QueryLanguage::from(self.data_source.config.driver())
    }

    /// The SQL dialect, for what only a SQL engine is sent: grid edits, DDL, imports, a table's
    /// export, manual transactions and parameters. `None` for a language without one.
    fn dialect(&self) -> Option<Dialect> {
        self.language().dialect()
    }

    /// Why a request that needs SQL can't run on this data source.
    fn needs_sql(&self, what: &str) -> String {
        format!(
            "\u{201c}{}\u{201d} speaks {}, not SQL, so {what}.",
            self.name(),
            self.language().display_name()
        )
    }

    /// Tells a console about its connection's transaction. The explorer never opens one.
    fn notify_transaction(&self, status: TransactionStatus) {
        if let SessionOwner::Console(console) = self.owner {
            self.send(Event::TransactionChanged { console, status });
        }
    }

    fn notify(&self, state: SessionState) {
        self.send(match self.owner {
            SessionOwner::Explorer(id) => Event::SessionChanged { id, state },
            SessionOwner::Console(console) => Event::ConsoleSessionChanged { console, state },
        });
    }
}

/// What can interrupt a session's work from outside.
struct Signals {
    disconnect: oneshot::Receiver<()>,
    cancel: watch::Receiver<Option<RunId>>,
}

/// The session was told to disconnect while working; its loop must stop.
struct Disconnected;

impl Signals {
    /// Awaits `work` unless the session is told to disconnect first. If `run` is cancelled
    /// meanwhile, asks the database to stop it and keeps waiting: the work then fails
    /// promptly, and the connection is left in a known state instead of mid-result.
    async fn guard<T>(
        &mut self,
        work: impl Future<Output = T>,
        run: Option<RunId>,
        cancel_handle: &dyn CancelHandle,
    ) -> Result<T, Disconnected> {
        tokio::pin!(work);
        loop {
            tokio::select! {
                // Polling `work` first guarantees the statement has been started before a
                // cancel is acted on. Otherwise a cancel could reach the database before the
                // statement does, find nothing running, and be lost.
                biased;
                output = &mut work => return Ok(output),
                _ = &mut self.disconnect => return Err(Disconnected),
                changed = self.cancel.changed() => {
                    if changed.is_err() {
                        return Err(Disconnected);
                    }
                    if run.is_some() && *self.cancel.borrow_and_update() == run {
                        cancel_running_statement(cancel_handle).await;
                    }
                }
            }
        }
    }

    /// Whether `run` has been asked to stop.
    fn is_cancelled(&self, run: RunId) -> bool {
        *self.cancel.borrow() == Some(run)
    }
}

async fn cancel_running_statement(cancel_handle: &dyn CancelHandle) {
    if let Err(error) = cancel_handle.cancel().await {
        tracing::warn!(
            "could not cancel the running statement: {}",
            describe_error(&error)
        );
    }
}

/// Where a session finds its secrets and tunnel.
struct Credentials {
    secrets: Arc<dyn SecretStore>,
    passwords: SessionPasswords,
    tunnels: Tunnels,
    commands: PasswordCommands,
}

async fn run(
    context: SessionContext,
    credentials: Credentials,
    mut signals: Signals,
    mut requests: UnboundedReceiver<SessionRequest>,
) {
    let mut pending = None;
    loop {
        let Some(connection) = connect(&context, &credentials, &mut signals.disconnect).await
        else {
            return;
        };
        let loss = Loss::default();
        let mut connection: Box<dyn Connection> = Box::new(Watched::new(connection, loss.clone()));
        let cancel_handle = connection.cancel_handle();
        let mut worker = Worker {
            context: &context,
            connection: connection.as_mut(),
            cancel_handle: cancel_handle.as_ref(),
            signals: &mut signals,
            loss: &loss,
            last_used: Instant::now(),
            parked: None,
            transaction: TransactionStatus::Idle,
            read_only_open: None,
        };
        let ended = worker.serve(&mut requests, pending.take()).await;
        if let Some(parked) = worker.parked.take() {
            query::discard(&context, parked, cancel_handle.as_ref()).await;
        }
        let closed = connection.close().await;
        let Ended::Lost { reason, unserved } = ended else {
            if let Err(error) = closed {
                tracing::warn!(data_source = %context.name(), "error while disconnecting: {}", describe_error(&error));
            }
            tracing::info!(data_source = %context.name(), "disconnected");
            context.notify(SessionState::Disconnected);
            return;
        };
        tracing::warn!(data_source = %context.name(), "the connection was lost: {reason}");
        context.notify(SessionState::Lost(reason.clone()));
        match context.owner {
            // A console's transaction, temporary tables and settings went with the
            // connection; it reconnects only when asked, knowing that.
            //
            // The session stays, refusing every request, until Reconnect or closing the
            // console replaces it. Ending here instead would leave a request sent meanwhile in
            // a channel nobody reads, unanswered.
            SessionOwner::Console(_) => {
                if let Some(request) = unserved {
                    loss::refuse(&context, request, &reason);
                }
                while let Some(request) = next_request_while_lost(&mut signals, &mut requests).await
                {
                    loss::refuse(&context, request, &reason);
                }
                return;
            }
            // The explorer has nothing to lose by reconnecting, so it does on its next
            // request.
            SessionOwner::Explorer(_) => {
                pending = match unserved {
                    Some(request) => Some(request),
                    None => match next_request_while_lost(&mut signals, &mut requests).await {
                        Some(request) => Some(request),
                        None => {
                            context.notify(SessionState::Disconnected);
                            return;
                        }
                    },
                };
            }
        }
    }
}

/// How a connected session stopped serving.
enum Ended {
    /// Told to disconnect.
    Disconnected,
    /// The connection is gone. `unserved` is the request the idle ping found that out
    /// before; a request that ran into it was answered with its failure.
    Lost {
        reason: String,
        unserved: Option<SessionRequest>,
    },
}

/// The next request for a session whose connection is gone, or `None` if it is told to
/// disconnect first.
async fn next_request_while_lost(
    signals: &mut Signals,
    requests: &mut UnboundedReceiver<SessionRequest>,
) -> Option<SessionRequest> {
    loop {
        tokio::select! {
            _ = &mut signals.disconnect => return None,
            // Nothing is running to cancel.
            changed = signals.cancel.changed() => changed.ok()?,
            request = requests.recv() => return request,
        }
    }
}

/// Opens the connection, reporting progress. `None` if it failed or was cancelled.
async fn connect(
    context: &SessionContext,
    credentials: &Credentials,
    disconnect_signal: &mut oneshot::Receiver<()>,
) -> Option<Box<dyn Connection>> {
    let name = context.name();
    context.notify(SessionState::Connecting);
    tracing::info!(data_source = %name, "connecting to {}", context.data_source.config);
    let started = Instant::now();
    let typed = TypedSecrets::default();
    let attempt = tokio::select! {
        attempt = open(
            &context.data_source,
            &typed,
            Connector {
                secrets: &credentials.secrets,
                tunnels: &credentials.tunnels,
                commands: &credentials.commands,
                prompting: Prompting::Ask(&credentials.passwords),
            },
        ) => attempt,
        _ = disconnect_signal => {
            tracing::info!(data_source = %name, "connection attempt cancelled");
            context.notify(SessionState::Disconnected);
            return None;
        }
    };

    match attempt {
        Ok(connection) => {
            let server = connection.server_info().clone();
            tracing::info!(
                data_source = %name,
                elapsed_ms = started.elapsed().as_millis(),
                "connected to {server}"
            );
            context.notify(SessionState::Connected(server));
            Some(connection)
        }
        Err(error) => {
            let description = describe_error(&error);
            tracing::warn!(data_source = %name, "connection failed: {description}");
            context.notify(SessionState::Failed(description));
            let data_source = context.data_source.id;
            if let Some(key) = error.unknown_host_key() {
                context.send(Event::HostKeyUnknown {
                    data_source,
                    key: key.clone(),
                });
            }
            if let Some(command) = error.untrusted_password_command() {
                context.send(Event::PasswordCommandUntrusted {
                    data_source,
                    command: command.clone(),
                });
            }
            if let OpenError::PasswordNeeded { secret, reason } = error {
                context.send(Event::PasswordNeeded {
                    data_source,
                    secret,
                    reason,
                });
            }
            None
        }
    }
}

/// A connected session's state while it serves requests.
struct Worker<'a> {
    context: &'a SessionContext,
    connection: &'a mut dyn Connection,
    cancel_handle: &'a dyn CancelHandle,
    signals: &'a mut Signals,
    /// Why the connection was lost, once any call has found out.
    loss: &'a Loss,
    /// When the connection last did something, to know when to ping before using it.
    last_used: Instant,
    /// The last run's final statement, with rows still unread.
    parked: Option<query::Parked>,
    /// The connection's transaction status as last reported to the UI.
    transaction: TransactionStatus,
    /// The read-only transaction a read-only data source's statement runs in, while open.
    read_only_open: Option<read_only::ReadOnlyMode>,
}

impl Worker<'_> {
    /// Answers `first`, if given, then requests, until told to disconnect or the connection
    /// is lost.
    async fn serve(
        &mut self,
        requests: &mut UnboundedReceiver<SessionRequest>,
        mut first: Option<SessionRequest>,
    ) -> Ended {
        loop {
            let request = match first.take() {
                Some(request) => request,
                None => match self.next_request(requests).await {
                    Some(request) => request,
                    None => return Ended::Disconnected,
                },
            };
            if let Some(reason) = self.ping_if_idle().await {
                return Ended::Lost {
                    reason,
                    unserved: Some(request),
                };
            }
            if self.handle(request).await.is_err() {
                return Ended::Disconnected;
            }
            // A statement whose rows are no longer parked is done with its read-only
            // transaction.
            if self.parked.is_none() && guard::close(self).await.is_err() {
                return Ended::Disconnected;
            }
            self.last_used = Instant::now();
            if let Some(reason) = self.loss.reason() {
                return Ended::Lost {
                    reason,
                    unserved: None,
                };
            }
        }
    }

    /// Pings a connection that has been idle for a while, so a statement isn't sent down a
    /// connection that is gone. Not while rows are parked: their statement still holds the
    /// connection, so it is in use. Returns why the connection was lost, if it was.
    async fn ping_if_idle(&mut self) -> Option<String> {
        if self.parked.is_some() || self.last_used.elapsed() < self.context.idle_ping_after {
            return None;
        }
        if let Err(error) = self.connection.ping().await {
            tracing::debug!(data_source = %self.context.name(), "ping failed: {}", describe_error(&error));
        }
        self.last_used = Instant::now();
        self.loss.reason()
    }

    /// The next request, meanwhile discarding parked rows whose run gets cancelled.
    async fn next_request(
        &mut self,
        requests: &mut UnboundedReceiver<SessionRequest>,
    ) -> Option<SessionRequest> {
        loop {
            tokio::select! {
                _ = &mut self.signals.disconnect => return None,
                changed = self.signals.cancel.changed() => {
                    changed.ok()?;
                    let cancelled = *self.signals.cancel.borrow_and_update();
                    if let Some(parked) = self.parked.take_if(|parked| Some(parked.run()) == cancelled) {
                        query::discard(self.context, parked, self.cancel_handle).await;
                        guard::close(self).await.ok()?;
                    }
                }
                request = requests.recv() => return request,
            }
        }
    }

    async fn handle(&mut self, request: SessionRequest) -> Result<(), Disconnected> {
        if let SessionRequest::FetchMore(run) = request {
            if let Some(parked) = self.parked.take_if(|parked| parked.run() == run) {
                self.parked = query::fetch_more(self, parked).await?;
            }
            return Ok(());
        }
        // Validation is only a check, so it never costs the user their unread rows.
        if let SessionRequest::Validate(statements) = request {
            return validate::validate(self, statements).await;
        }
        // Anything else needs the connection, which parked rows are still holding.
        if let Some(parked) = self.parked.take() {
            query::discard(self.context, parked, self.cancel_handle).await;
        }
        match request {
            SessionRequest::Children(path, query) => {
                let work = catalog::children(self.context, &mut *self.connection, path, query);
                self.signals.guard(work, None, self.cancel_handle).await
            }
            SessionRequest::Definition(path) => {
                let work = catalog::definition(self.context, &mut *self.connection, path);
                self.signals.guard(work, None, self.cancel_handle).await
            }
            SessionRequest::Execute(script) => {
                self.parked = query::run_script(self, &script).await?;
                Ok(())
            }
            SessionRequest::EndTransaction(end) => transaction::end(self, end).await,
            SessionRequest::DescribeTable(table) => {
                let work = catalog::describe_table(self.context, &mut *self.connection, table);
                self.signals.guard(work, None, self.cancel_handle).await
            }
            SessionRequest::DescribeTables(tables) => {
                let work = catalog::describe_tables(self.context, &mut *self.connection, tables);
                self.signals.guard(work, None, self.cancel_handle).await
            }
            SessionRequest::DescribeRole(path) => {
                let work = catalog::describe_role(self.context, &mut *self.connection, path);
                self.signals.guard(work, None, self.cancel_handle).await
            }
            SessionRequest::ApplyEdits(statements) => edits::apply(self, &statements).await,
            SessionRequest::ApplyDdl(statements) => ddl::apply(self, &statements).await,
            SessionRequest::ExportAll(export) => export::write_all(self, export).await,
            SessionRequest::ExportTable(export) => export::write_table(self, export).await,
            SessionRequest::ImportRows(import) => import::import(self, &import).await,
            SessionRequest::Ddl(path) => {
                let work = ddl::load(self.context, &mut *self.connection, path);
                self.signals.guard(work, None, self.cancel_handle).await
            }
            SessionRequest::LoadTable(path) => {
                let work = ddl::load_table(self.context, &mut *self.connection, path);
                self.signals.guard(work, None, self.cancel_handle).await
            }
            SessionRequest::FetchMore(_) | SessionRequest::Validate(_) => Ok(()),
        }
    }
}
