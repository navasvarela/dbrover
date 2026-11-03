//! MongoDB driver for DbRover, on the official `mongodb` driver (`docs/nosql.md`, N2).
//!
//! A console's commands are the shell's syntax, read by `dbrover_language::mongo` into a tree
//! and sent as the server commands the shell's methods send ([`plan`]). A connection is a client
//! (the driver's pool) and the database the console is in, which `use` changes. MongoDB has no
//! transactions outside a replica set's sessions, so a connection is never inside one.

mod answer;
mod catalog;
mod convert;
mod cursor;
mod errors;
mod options;
mod plan;
mod shell;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use bson::{Bson, Document, doc};
use dbrover_core::{
    CancelHandle, CatalogNode, CatalogPath, ChildrenPage, ChildrenQuery, Connection, DriverError,
    MongoConfig, Password, Rejection, RejectionKind, RelationName, ResultCursor, Route, ServerInfo,
    TableConstraints, TableDescription, TableName, TransactionStatus, describe_error,
};
use dbrover_language::mongo::{self, Command};
use mongodb::{Client, Database};

use crate::answer::BulkResult;
use crate::catalog::Level;
use crate::cursor::{DocumentCursor, Table};
use crate::plan::{Plan, Step};

/// Connects to the server or cluster in `config`, through `route`.
///
/// # Errors
///
/// [`DriverError::Connect`] if the connection string can't be read, no server answers in time,
/// the TLS handshake or certificate check fails, or the server refuses the user or password.
pub async fn connect(
    config: &MongoConfig,
    password: Option<&Password>,
    route: Route,
) -> Result<MongoConnection, DriverError> {
    let options = options::client_options(config, password, route)
        .await
        .map_err(errors::connect)?;
    let client = Client::with_options(options).map_err(errors::connect)?;
    let admin = client.database("admin");
    // The driver connects lazily; a ping makes it find a server and sign in now, so a wrong
    // password is reported by Test connection rather than by the first command.
    admin
        .run_command(doc! { "ping": 1 })
        .await
        .map_err(|error| errors::connect(errors::Server(error)))?;
    let version = match admin.run_command(doc! { "buildInfo": 1 }).await {
        Ok(info) => info.get_str("version").unwrap_or("unknown").to_owned(),
        Err(_) => "unknown".to_owned(),
    };
    Ok(MongoConnection {
        client,
        database: config.server.database.clone(),
        server_info: ServerInfo::new("MongoDB", version),
        running: Arc::new(Running::default()),
    })
}

/// The `comment` of the command a connection ran last, which ⌘. looks for among the server's
/// operations.
#[derive(Debug, Default)]
struct Running(Mutex<Option<String>>);

impl Running {
    /// A comment unique to a new run, kept until the next.
    fn start(&self) -> String {
        let comment = format!("dbrover {}", uuid::Uuid::new_v4());
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(comment.clone());
        comment
    }

    fn current(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// A console's or the explorer's connection to a MongoDB server or cluster.
pub struct MongoConnection {
    client: Client,
    /// The database commands run in: the data source's, or the last one `use` chose.
    database: String,
    server_info: ServerInfo,
    running: Arc<Running>,
}

impl MongoConnection {
    /// Runs what `plan` sends, tagged with a new run's comment.
    async fn run(&self, plan: Plan) -> Result<Box<dyn ResultCursor>, DriverError> {
        let comment = Bson::String(self.running.start());
        let database = self.client.database(&plan.database);
        match plan.step {
            Step::Cursor { mut command, rows } => {
                with_batch_size(&mut command);
                tag(&mut command, &comment);
                let cursor = DocumentCursor::start(database, command, rows, comment).await?;
                Ok(Box::new(cursor))
            }
            Step::Command {
                mut command,
                answer,
            } => {
                tag(&mut command, &comment);
                let reply = run_command(database, command).await?;
                let table = answer::table(&answer, reply)
                    .map_err(|error| errors::statement(error, None))?;
                Ok(Box::new(table))
            }
            Step::Bulk { writes, ordered } => {
                let mut result = BulkResult::default();
                let mut failures = Vec::new();
                for (index, (mut command, answer)) in writes.into_iter().enumerate() {
                    tag(&mut command, &comment);
                    let reply = run_write(database.clone(), command).await;
                    match reply {
                        Ok(reply) => result.add(index, &answer, &reply),
                        Err(error) if ordered => return Err(error),
                        Err(error) => {
                            failures.push(format!("operation {index}: {}", describe_error(&error)));
                        }
                    }
                }
                if let Some(first) = failures.first() {
                    return Err(errors::statement(
                        format!("{} of the writes failed; {first}", failures.len()),
                        None,
                    ));
                }
                Ok(Box::new(result.table()))
            }
        }
    }
}

/// Asks for documents a grid page at a time, unless the command asks for another batch size.
fn with_batch_size(command: &mut Document) {
    let first = command.keys().next().cloned().unwrap_or_default();
    match first.as_str() {
        "find" if !command.contains_key("batchSize") => {
            command.insert("batchSize", cursor::BATCH_SIZE);
        }
        "aggregate" | "listCollections" | "listIndexes" => {
            let mut options = command.get_document("cursor").cloned().unwrap_or_default();
            if !options.contains_key("batchSize") {
                options.insert("batchSize", cursor::BATCH_SIZE);
            }
            command.insert("cursor", options);
        }
        _ => {}
    }
}

/// Tags `command` with the run's comment, unless it carries one of its own: what ⌘. looks for
/// with `$currentOp`. Every command takes a `comment` since MongoDB 4.4; an `explain` passes it
/// to the command it explains.
fn tag(command: &mut Document, comment: &Bson) {
    let target = match command.get_mut("explain") {
        Some(Bson::Document(explained)) => explained,
        _ => command,
    };
    if !target.contains_key("comment") {
        target.insert("comment", comment.clone());
    }
}

/// Runs `command` in a task of its own, which finishes even if this future is dropped: the
/// driver warns that dropping its futures part-way can leave a connection in an unknown state.
async fn run_command(database: Database, command: Document) -> Result<Document, DriverError> {
    tokio::spawn(async move { database.run_command(command).await })
        .await
        .map_err(|error| errors::statement(error, None))?
        .map_err(errors::command)
}

/// Runs one write of a bulk write, its refused writes reported as its failure.
async fn run_write(database: Database, command: Document) -> Result<Document, DriverError> {
    let reply = run_command(database, command).await?;
    answer::check_write(&reply).map_err(|error| errors::statement(error, None))?;
    Ok(reply)
}

#[async_trait]
impl Connection for MongoConnection {
    fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    async fn ping(&mut self) -> Result<(), DriverError> {
        self.client
            .database("admin")
            .run_command(doc! { "ping": 1 })
            .await
            .map(drop)
            .map_err(errors::catalog)
    }

    async fn children(&mut self, path: &CatalogPath) -> Result<Vec<CatalogNode>, DriverError> {
        let whole = ChildrenQuery {
            filter: None,
            after: None,
            limit: None,
        };
        Ok(self.children_page(path, &whole).await?.nodes)
    }

    /// The databases and a collection's indexes whole, and a database's collections a page
    /// at a time, by name.
    async fn children_page(
        &mut self,
        path: &CatalogPath,
        query: &ChildrenQuery,
    ) -> Result<ChildrenPage, DriverError> {
        let level = Level::of(path).ok_or_else(|| DriverError::InvalidPath(path.clone()))?;
        match level {
            Level::Databases => Ok(ChildrenPage::whole(
                catalog::databases(&self.client).await?,
                query,
            )),
            Level::Collections { database } => {
                catalog::collections(&self.client.database(database), path, query).await
            }
            Level::Indexes {
                database,
                collection,
            } => {
                let indexes =
                    catalog::indexes(&self.client.database(database), collection, path).await?;
                Ok(ChildrenPage::whole(indexes, query))
            }
        }
    }

    /// A collection's or a view's commands to make it again.
    async fn definition(&mut self, path: &CatalogPath) -> Result<String, DriverError> {
        let invalid = || DriverError::InvalidPath(path.clone());
        let Some(Level::Indexes {
            database,
            collection,
        }) = Level::of(path)
        else {
            return Err(invalid());
        };
        catalog::definition(&self.client.database(database), collection)
            .await?
            .ok_or_else(invalid)
    }

    async fn execute(&mut self, text: &str) -> Result<Box<dyn ResultCursor>, DriverError> {
        let plan = match mongo::parse(text).map_err(errors::parse)? {
            Command::Use(database) => {
                self.database = database;
                return Ok(Box::new(Table::empty()));
            }
            Command::Show(show) => plan::show(show, &self.database),
            Command::Call(call) => plan::call(&call, &self.database)
                .map_err(|error| errors::statement(error, Some(call.method.range.start)))?,
        };
        self.run(plan).await
    }

    /// MongoDB has no tables, so no rows to edit through one.
    async fn describe_table(
        &mut self,
        _table: &RelationName,
    ) -> Result<Option<TableDescription>, DriverError> {
        Ok(None)
    }

    async fn describe_constraints(
        &mut self,
        _table: &TableName,
    ) -> Result<TableConstraints, DriverError> {
        Ok(TableConstraints::default())
    }

    /// Reads the command, and has the server plan a read without running it: `explain` with
    /// `queryPlanner` verbosity catches an unknown operator or a malformed pipeline. A write
    /// is only read, since DbRover doesn't send it until it runs.
    async fn validate(&mut self, text: &str) -> Result<Option<Rejection>, DriverError> {
        let call = match mongo::parse(text) {
            Ok(Command::Call(call)) => call,
            Ok(Command::Use(_) | Command::Show(_)) => return Ok(None),
            Err(error) => {
                return Ok(Some(Rejection {
                    position: Some(error.range.start),
                    message: error.message,
                    kind: RejectionKind::Syntax,
                }));
            }
        };
        let position = Some(call.method.range.start);
        let plan = match plan::call(&call, &self.database) {
            Ok(plan) => plan,
            Err(error) => {
                return Ok(Some(Rejection {
                    position,
                    message: error.to_string(),
                    kind: RejectionKind::Other,
                }));
            }
        };
        let Some(read) = plan::plannable_read(&plan.step) else {
            return Ok(None);
        };
        let explain =
            plan::explain_command(read.clone(), Some(Bson::String("queryPlanner".to_owned())));
        match run_command(self.client.database(&plan.database), explain).await {
            Ok(_) => Ok(None),
            Err(DriverError::Statement { source, .. }) => Ok(Some(Rejection {
                position,
                message: source.to_string(),
                kind: RejectionKind::Other,
            })),
            Err(error) => Err(error),
        }
    }

    async fn transaction_status(&mut self) -> Result<TransactionStatus, DriverError> {
        Ok(TransactionStatus::Idle)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(MongoCancel {
            client: self.client.clone(),
            running: Arc::clone(&self.running),
        })
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        self.client.shutdown().await;
        Ok(())
    }
}

/// Stops a connection's running command: finds its operations among the user's own with
/// `$currentOp`, by the run's comment, and ends each with `killOp`. The client is a pool, so
/// this goes over another of its connections, as the same user, who may always kill their own
/// operations.
struct MongoCancel {
    client: Client,
    running: Arc<Running>,
}

impl MongoCancel {
    /// The ids of the operations tagged with `comment`, a cursor's later batches included.
    async fn operations(&self, admin: &Database, comment: &str) -> Result<Vec<Bson>, DriverError> {
        let reply = admin
            .run_command(doc! {
                "aggregate": 1,
                "pipeline": [
                    { "$currentOp": {} },
                    { "$match": { "$or": [
                        { "command.comment": comment },
                        { "cursor.originatingCommand.comment": comment },
                    ] } },
                    { "$project": { "opid": 1 } },
                ],
                "cursor": {},
            })
            .await
            .map_err(|error| DriverError::Connection(Box::new(errors::Server(error))))?;
        let batch = reply
            .get_document("cursor")
            .and_then(|cursor| cursor.get_array("firstBatch"))
            .cloned()
            .unwrap_or_default();
        Ok(batch
            .into_iter()
            .filter_map(|operation| operation.as_document()?.get("opid").cloned())
            .collect())
    }
}

#[async_trait]
impl CancelHandle for MongoCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        let Some(comment) = self.running.current() else {
            return Ok(());
        };
        let admin = self.client.database("admin");
        for id in self.operations(&admin, &comment).await? {
            admin
                .run_command(doc! { "killOp": 1, "op": id })
                .await
                .map_err(|error| DriverError::Connection(Box::new(errors::Server(error))))?;
        }
        Ok(())
    }
}
