//! The object helpers' DDL on the explorer's connection: applying generated statements,
//! loading a table's, view's or role's `CREATE` statement for Copy DDL, and loading a table
//! for Modify table.

use std::time::Instant;

use dbrover_core::{
    CatalogNode, CatalogPath, Connection, DriverError, FolderKind, Identifier, ManagedUsers,
    NodeKind, PrincipalDescription, RelationName, Row, TableDescription, describe_error,
};
use dbrover_sql::{
    ColumnSpec, DdlOperation, Dialect, ExistingTable, ForeignKeySwitch, ObjectName, RoleSpec,
    TableSpec, UserModel, account_ddl, rebuild_refusal,
};
use dbrover_store::{HistoryEntry, HistoryOutcome, HistorySource, redact_secrets};
use jiff::Timestamp;

use super::{Disconnected, SessionContext, Worker, transaction};
use crate::{DdlFailure, Event};

/// Runs `statements` and reports [`Event::DdlApplied`]: in one transaction where the
/// dialect's DDL is transactional, one after the other otherwise.
pub(super) async fn apply(
    worker: &mut Worker<'_>,
    statements: &[String],
) -> Result<(), Disconnected> {
    let context = worker.context;
    let result = if context.read_only.is_some() {
        tracing::warn!(data_source = %context.name(), "DDL refused: read-only");
        Err(DdlFailure {
            statement: None,
            message: format!(
                "Refused: \u{201c}{}\u{201d} is read-only, so nothing in it can be created or \
                 changed.",
                context.name()
            ),
            applied: 0,
        })
    } else if let Some(dialect) = context.dialect() {
        let capabilities = dialect.capabilities();
        match capabilities.foreign_key_switch {
            _ if !capabilities.transactional_ddl => run_each(worker, statements).await?,
            Some(switch) => with_foreign_keys_off(worker, dialect, statements, switch).await?,
            None => in_transaction(worker, dialect, statements, None).await?,
        }
    } else {
        Err(DdlFailure {
            statement: None,
            message: context.needs_sql("its objects can't be created or changed here"),
            applied: 0,
        })
    };
    match &result {
        Ok(()) => tracing::info!(
            data_source = %context.name(),
            "applied {} DDL statements",
            statements.len()
        ),
        Err(failure) => tracing::warn!(
            data_source = %context.name(),
            "DDL failed after {} statements stayed applied: {}",
            failure.applied,
            failure.message
        ),
    }
    context.send(Event::DdlApplied {
        data_source: context.data_source.id,
        result,
    });
    Ok(())
}

/// Runs the statements in a transaction with foreign keys switched off, as SQLite's
/// documentation says to change a table's schema: the switch does nothing inside a
/// transaction, and with foreign keys on, dropping the table a rebuild replaces would run the
/// `ON DELETE` actions of other tables' keys. Where they were on, they are checked before
/// committing, and switched on again afterwards, whatever happened.
async fn with_foreign_keys_off(
    worker: &mut Worker<'_>,
    dialect: Dialect,
    statements: &[String],
    switch: ForeignKeySwitch,
) -> Result<Result<(), DdlFailure>, Disconnected> {
    let unwrapped = |error: &DriverError| DdlFailure {
        statement: None,
        message: describe_error(error),
        applied: 0,
    };
    let enforced = match first_rows(worker, switch.query).await? {
        Ok(rows) => rows.first().and_then(|row| row.first()).cloned().flatten() == Some("1".into()),
        Err(error) => return Ok(Err(unwrapped(&error))),
    };
    if !enforced {
        return in_transaction(worker, dialect, statements, None).await;
    }
    if let Err(error) = transaction::run_counting(worker, switch.off, None).await? {
        return Ok(Err(unwrapped(&error)));
    }
    let result = in_transaction(worker, dialect, statements, Some(switch.check)).await?;
    if let Err(error) = transaction::run_counting(worker, switch.on, None).await? {
        tracing::warn!(
            data_source = %worker.context.name(),
            "could not switch foreign keys on again: {}",
            describe_error(&error)
        );
    }
    Ok(result)
}

/// The first page of rows `sql` answers.
async fn first_rows(
    worker: &mut Worker<'_>,
    sql: &str,
) -> Result<Result<Vec<Row>, DriverError>, Disconnected> {
    let connection = &mut *worker.connection;
    let work = async move {
        let mut cursor = connection.execute(sql).await?;
        let rows = cursor.next_page(BROKEN_KEYS_SHOWN).await?;
        cursor.finish().await?;
        Ok(rows)
    };
    worker.signals.guard(work, None, worker.cancel_handle).await
}

/// How many rows with a broken foreign key a failed check reads: enough to say there are
/// some, and name the first table.
const BROKEN_KEYS_SHOWN: usize = 100;

/// Opens a transaction, runs the statements and commits them, or rolls everything back at the
/// first failure. With `check`, a query that answers a row for each row whose foreign key
/// points at nothing, the transaction is rolled back too if it answers any.
async fn in_transaction(
    worker: &mut Worker<'_>,
    dialect: Dialect,
    statements: &[String],
    check: Option<&str>,
) -> Result<Result<(), DdlFailure>, Disconnected> {
    let unwrapped = |error: &DriverError| DdlFailure {
        statement: None,
        message: describe_error(error),
        applied: 0,
    };
    let begin = dialect.begin_transaction();
    if let Err(error) = transaction::run_counting(worker, begin, None).await? {
        return Ok(Err(unwrapped(&error)));
    }
    let failure = match run_each(worker, statements).await? {
        Ok(()) => match broken_keys(worker, check).await? {
            Some(failure) => failure,
            None => match transaction::run_counting(worker, "COMMIT", None).await? {
                Ok(_) => return Ok(Ok(())),
                Err(error) => unwrapped(&error),
            },
        },
        Err(failure) => failure,
    };
    if let Err(error) = transaction::run_counting(worker, "ROLLBACK", None).await? {
        tracing::warn!(
            data_source = %worker.context.name(),
            "ROLLBACK failed after DDL failed: {}",
            describe_error(&error)
        );
    }
    Ok(Err(DdlFailure {
        applied: 0,
        ..failure
    }))
}

/// Runs the foreign key `check`, if any, and says why the change can't be kept if a row's key
/// points at nothing.
async fn broken_keys(
    worker: &mut Worker<'_>,
    check: Option<&str>,
) -> Result<Option<DdlFailure>, Disconnected> {
    let Some(check) = check else {
        return Ok(None);
    };
    let message = match first_rows(worker, check).await? {
        Ok(rows) if rows.is_empty() => return Ok(None),
        Ok(rows) => {
            // `PRAGMA foreign_key_check` answers the table, the row, the table it points at.
            let table = rows
                .first()
                .and_then(|row| row.first().cloned().flatten())
                .unwrap_or_default();
            format!(
                "Undone: afterwards, rows of {table} would have foreign keys that point at \
                 nothing ({} found).",
                rows.len()
            )
        }
        Err(error) => describe_error(&error),
    };
    Ok(Some(DdlFailure {
        statement: None,
        message,
        applied: 0,
    }))
}

/// Runs the statements in order, recording each in the history, and stops at the first that
/// fails: the statements before it are applied, in whatever transaction is open.
async fn run_each(
    worker: &mut Worker<'_>,
    statements: &[String],
) -> Result<Result<(), DdlFailure>, Disconnected> {
    for (index, sql) in statements.iter().enumerate() {
        tracing::info!(data_source = %worker.context.name(), "{}", redact_secrets(sql));
        let started_at = Timestamp::now();
        let started = Instant::now();
        let result = transaction::run_counting(worker, sql, None).await?;
        let outcome = match &result {
            Ok(_) => HistoryOutcome::Ok,
            Err(_) => HistoryOutcome::Error,
        };
        worker.context.record(HistoryEntry {
            data_source: worker.context.data_source.id,
            executed_at: started_at,
            sql: sql.clone(),
            duration: Some(started.elapsed()),
            rows: None,
            outcome,
            source: HistorySource::ObjectHelper,
        });
        if let Err(error) = result {
            let message = match &error {
                DriverError::Statement { source, .. } => describe_error(source.as_ref()),
                other => describe_error(other),
            };
            return Ok(Err(DdlFailure {
                statement: Some(index),
                message,
                applied: index,
            }));
        }
    }
    Ok(Ok(()))
}

/// The constraints Copy DDL adds after a rebuilt `CREATE TABLE`, by the detail PostgreSQL's
/// and SQL Server's catalogs give them. The primary key is in the table's description already,
/// and so are NOT NULL (PostgreSQL 18 lists those as constraints too) and SQL Server's
/// defaults, in its columns.
const COPIED_CONSTRAINTS: &[&str] = &["foreign key", "unique", "check", "exclusion"];

/// Reports [`Event::DdlLoaded`] for the table or view at `path`.
pub(super) async fn load(
    context: &SessionContext,
    connection: &mut dyn Connection,
    path: CatalogPath,
) {
    let kind = path.last().map(|segment| segment.kind);
    let result = if kind == Some(NodeKind::Table) && rebuilds_table_ddl(context) {
        rebuild(context, connection, &path).await
    } else if matches!(kind, Some(NodeKind::Role | NodeKind::User))
        && sql_dialect(context)
            .is_ok_and(|dialect| dialect.capabilities().users == UserModel::Accounts)
    {
        rebuild_account(connection, &path).await
    } else if kind == Some(NodeKind::Role) {
        rebuild_role(context, connection, &path).await
    } else {
        connection
            .definition(&path)
            .await
            .map_err(|error| describe_error(&error))
    };
    if let Err(error) = &result {
        tracing::warn!(data_source = %context.name(), "could not load the DDL of {path}: {error}");
    }
    context.send(Event::DdlLoaded {
        data_source: context.data_source.id,
        path,
        result,
    });
}

fn rebuilds_table_ddl(context: &SessionContext) -> bool {
    context
        .dialect()
        .is_some_and(|dialect| dialect.capabilities().rebuilds_table_ddl)
}

/// The dialect to write a statement in, or why there is none.
fn sql_dialect(context: &SessionContext) -> Result<Dialect, String> {
    context
        .dialect()
        .ok_or_else(|| context.needs_sql("DbRover can't write its DDL"))
}

/// `CREATE TABLE` from the table's description, through the same builder as New table,
/// followed by an `ALTER TABLE … ADD CONSTRAINT` per constraint, as the catalog defines them:
/// the order `pg_dump` writes them in, so a foreign key to a table created later still works
/// once both exist.
async fn rebuild(
    context: &SessionContext,
    connection: &mut dyn Connection,
    path: &CatalogPath,
) -> Result<String, String> {
    let dialect = sql_dialect(context)?;
    let description = describe_at(connection, path).await?;
    let spec = TableSpec {
        name: ObjectName::new(Some(&description.schema), &description.name),
        columns: description
            .columns
            .into_iter()
            .map(ColumnSpec::from)
            .collect(),
        primary_key: description.primary_key,
        unique_keys: Vec::new(),
        foreign_keys: Vec::new(),
        checks: Vec::new(),
    };
    let mut statements = DdlOperation::CreateTable(spec)
        .statements(dialect)
        .map_err(|error| error.to_string())?;
    let constraints = connection
        .children(&path.child(
            NodeKind::Folder(FolderKind::Constraints),
            FolderKind::Constraints.label(),
        ))
        .await
        .map_err(|error| describe_error(&error))?;
    for constraint in constraints.iter().filter(|node| is_copied(node)) {
        let definition = connection
            .definition(&constraint.path)
            .await
            .map_err(|error| describe_error(&error))?;
        statements.push(definition.trim_end().trim_end_matches(';').to_owned());
    }
    Ok(statements
        .iter()
        .map(|statement| format!("{statement};\n"))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Describes the table at `path`, named exactly as the tree shows it.
async fn describe_at(
    connection: &mut dyn Connection,
    path: &CatalogPath,
) -> Result<TableDescription, String> {
    let not_a_table = || format!("{path} is not a table");
    let schema = path.name_of(NodeKind::Schema).ok_or_else(not_a_table)?;
    let table = path.name_of(NodeKind::Table).ok_or_else(not_a_table)?;
    let exactly = |name: &str| Identifier {
        name: name.to_owned(),
        quoted: true,
    };
    let relation = RelationName {
        schema: Some(exactly(schema)),
        name: exactly(table),
    };
    connection
        .describe_table(&relation)
        .await
        .map_err(|error| describe_error(&error))?
        .ok_or_else(not_a_table)
}

/// Reports [`Event::TableLoaded`] for the table at `path`: its description and constraints,
/// as Modify table starts from them.
pub(super) async fn load_table(
    context: &SessionContext,
    connection: &mut dyn Connection,
    path: CatalogPath,
) {
    let result = existing_table(context, connection, &path).await;
    if let Err(error) = &result {
        tracing::warn!(data_source = %context.name(), "could not load the table {path}: {error}");
    }
    context.send(Event::TableLoaded {
        data_source: context.data_source.id,
        path,
        result,
    });
}

async fn existing_table(
    context: &SessionContext,
    connection: &mut dyn Connection,
    path: &CatalogPath,
) -> Result<ExistingTable, String> {
    let dialect = sql_dialect(context)?;
    let description = describe_at(connection, path).await?;
    let constraints = connection
        .describe_constraints(&description.table_name())
        .await
        .map_err(|error| describe_error(&error))?;
    let mut table = ExistingTable::from_catalog(description, constraints, dialect);
    if dialect.capabilities().rebuilds_tables {
        let definition = connection
            .definition(path)
            .await
            .map_err(|error| describe_error(&error))?;
        table.rebuild_refusal = rebuild_refusal(&definition);
        table.dependents = dependents(connection, path).await?;
    }
    Ok(table)
}

/// The `CREATE` statements of the table's indexes and triggers, which a rebuild creates
/// again. Indexes made for its keys have none: the rebuilt table's keys make them.
async fn dependents(
    connection: &mut dyn Connection,
    table: &CatalogPath,
) -> Result<Vec<String>, String> {
    let mut statements = Vec::new();
    for folder in [FolderKind::Indexes, FolderKind::Triggers] {
        let nodes = connection
            .children(&table.child(NodeKind::Folder(folder), folder.label()))
            .await
            .map_err(|error| describe_error(&error))?;
        for node in nodes.iter().filter(|node| node.has_definition) {
            let definition = connection
                .definition(&node.path)
                .await
                .map_err(|error| describe_error(&error))?;
            statements.push(definition.trim_end().trim_end_matches(';').to_owned());
        }
    }
    Ok(statements)
}

/// `CREATE ROLE` for the role at `path`, from its description, through the same builder as
/// New role: everything but its password, which the catalog only shows to superusers and
/// which would be a secret on the clipboard.
async fn rebuild_role(
    context: &SessionContext,
    connection: &mut dyn Connection,
    path: &CatalogPath,
) -> Result<String, String> {
    let dialect = sql_dialect(context)?;
    let role = connection
        .describe_role(path)
        .await
        .map_err(|error| describe_error(&error))?;
    let statements = DdlOperation::CreateRole {
        name: role.name.clone(),
        spec: RoleSpec::from(&role),
        password: None,
    }
    .statements(dialect)
    .map_err(|error| error.to_string())?;
    Ok(statements
        .iter()
        .map(|statement| format!("{statement};\n"))
        .collect())
}

/// `CREATE USER` or `CREATE ROLE` and its grants for the MySQL or MariaDB account at `path`
/// (W18), from its description: everything but its password, whose hash is a secret on the
/// clipboard.
async fn rebuild_account(
    connection: &mut dyn Connection,
    path: &CatalogPath,
) -> Result<String, String> {
    let management = connection
        .user_management()
        .await
        .map_err(|error| describe_error(&error))?;
    let ManagedUsers::Accounts(server) = management.server else {
        return Err("This server's users aren't MySQL's accounts.".to_owned());
    };
    let PrincipalDescription::Account(account) = connection
        .describe_principal(path)
        .await
        .map_err(|error| describe_error(&error))?
    else {
        return Err(format!("{path} isn't an account."));
    };
    let statements = account_ddl(server.flavor, &account).map_err(|error| error.to_string())?;
    Ok(statements
        .iter()
        .map(|statement| format!("{statement};\n"))
        .collect())
}

fn is_copied(constraint: &CatalogNode) -> bool {
    constraint.has_definition
        && constraint
            .detail
            .as_deref()
            .is_some_and(|detail| COPIED_CONSTRAINTS.contains(&detail))
}
