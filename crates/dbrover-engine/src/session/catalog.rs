use std::time::Instant;

use dbrover_core::{
    CatalogPath, ChildrenQuery, Connection, RelationName, TableDescription, describe_error,
};

use super::SessionContext;
use crate::{DescribedTable, Event};

pub(super) async fn children(
    context: &SessionContext,
    connection: &mut dyn Connection,
    path: CatalogPath,
    query: ChildrenQuery,
) {
    let name = context.name();
    let started = Instant::now();
    let result = connection.children_page(&path, &query).await;
    match &result {
        Ok(page) => tracing::debug!(
            data_source = %name,
            elapsed_ms = started.elapsed().as_millis(),
            partial = page.next.is_some(),
            "loaded {} nodes under {path}",
            page.nodes.len()
        ),
        Err(error) => tracing::warn!(
            data_source = %name,
            "could not load {path}: {}",
            describe_error(error)
        ),
    }
    context.send(Event::ChildrenLoaded {
        id: context.data_source.id,
        path,
        query,
        result: result.map_err(|error| describe_error(&error)),
    });
}

pub(super) async fn definition(
    context: &SessionContext,
    connection: &mut dyn Connection,
    path: CatalogPath,
) {
    let result = connection.definition(&path).await;
    if let Err(error) = &result {
        tracing::warn!(
            data_source = %context.name(),
            "could not load the definition of {path}: {}",
            describe_error(error)
        );
    }
    context.send(Event::DefinitionLoaded {
        id: context.data_source.id,
        path,
        result: result.map_err(|error| describe_error(&error)),
    });
}

pub(super) async fn describe_table(
    context: &SessionContext,
    connection: &mut dyn Connection,
    table: RelationName,
) {
    let result = described(context, connection, &table).await;
    context.send(Event::TableDescribed {
        data_source: context.data_source.id,
        table,
        result,
    });
}

pub(super) async fn describe_tables(
    context: &SessionContext,
    connection: &mut dyn Connection,
    tables: Vec<RelationName>,
) {
    let mut described_tables = Vec::with_capacity(tables.len());
    for table in tables {
        let result = described(context, connection, &table).await;
        described_tables.push(DescribedTable { table, result });
    }
    context.send(Event::TablesDescribed {
        data_source: context.data_source.id,
        tables: described_tables,
    });
}

/// The table's description, or the error describing it, logged.
async fn described(
    context: &SessionContext,
    connection: &mut dyn Connection,
    table: &RelationName,
) -> Result<Option<TableDescription>, String> {
    connection.describe_table(table).await.map_err(|error| {
        tracing::warn!(
            data_source = %context.name(),
            "could not describe {}: {}",
            table.to_sql(context.language().quoting()),
            describe_error(&error)
        );
        describe_error(&error)
    })
}

pub(super) async fn describe_role(
    context: &SessionContext,
    connection: &mut dyn Connection,
    path: CatalogPath,
) {
    let result = connection.describe_role(&path).await;
    if let Err(error) = &result {
        tracing::warn!(
            data_source = %context.name(),
            "could not describe the role {path}: {}",
            describe_error(error)
        );
    }
    context.send(Event::RoleDescribed {
        data_source: context.data_source.id,
        path,
        result: result.map_err(|error| describe_error(&error)),
    });
}
