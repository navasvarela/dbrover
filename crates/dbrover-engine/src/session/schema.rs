//! Reading a whole schema for schema compare and snapshots (W13, VC3).

use dbrover_core::{Connection, describe_error};
use dbrover_schema::Schema;

use super::SessionContext;
use crate::Event;

/// Reports [`Event::SchemaDescribed`] for `schema`, normalised in the data source's dialect.
pub(super) async fn describe(
    context: &SessionContext,
    connection: &mut dyn Connection,
    schema: String,
) {
    let result = read(context, connection, &schema).await;
    if let Err(error) = &result {
        tracing::warn!(data_source = %context.name(), "could not read the schema {schema}: {error}");
    }
    context.send(Event::SchemaDescribed {
        data_source: context.data_source.id,
        schema,
        result,
    });
}

/// `schema` as compared, or why it couldn't be read.
async fn read(
    context: &SessionContext,
    connection: &mut dyn Connection,
    schema: &str,
) -> Result<Schema, String> {
    let dialect = context
        .dialect()
        .ok_or_else(|| context.needs_sql("its schemas can't be compared"))?;
    let description = connection
        .describe_schema(schema)
        .await
        .map_err(|error| describe_error(&error))?
        .ok_or_else(|| format!("There is no schema \u{201c}{schema}\u{201d}."))?;
    Ok(Schema::from_description(description, dialect))
}
