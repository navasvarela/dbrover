//! The keys of a connection's tables and their indexes, from `DescribeTable`, kept for the
//! connection's life: DynamoDB never changes a table's keys, and an index added later is
//! described again when a statement names it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{KeySchemaElement, KeyType, TableDescription};
use dbrover_core::DriverError;

use crate::errors::{self, Refused};

/// A table's or an index's primary key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Keys {
    pub(crate) partition: String,
    pub(crate) sort: Option<String>,
}

impl Keys {
    fn of(schema: &[KeySchemaElement]) -> Option<Self> {
        let named = |key_type: KeyType| {
            schema
                .iter()
                .find(|element| *element.key_type() == key_type)
                .map(|element| element.attribute_name().to_owned())
        };
        Some(Self {
            partition: named(KeyType::Hash)?,
            sort: named(KeyType::Range),
        })
    }

    /// The key's attributes, partition first.
    pub(crate) fn names(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.partition).chain(&self.sort)
    }
}

/// A table's key and its secondary indexes', by the index's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableKeys {
    pub(crate) table: Keys,
    pub(crate) indexes: HashMap<String, Keys>,
}

impl TableKeys {
    pub(crate) fn of(description: &TableDescription) -> Option<Self> {
        let global = description
            .global_secondary_indexes()
            .iter()
            .filter_map(|index| {
                Some((
                    index.index_name()?.to_owned(),
                    Keys::of(index.key_schema())?,
                ))
            });
        let local = description
            .local_secondary_indexes()
            .iter()
            .filter_map(|index| {
                Some((
                    index.index_name()?.to_owned(),
                    Keys::of(index.key_schema())?,
                ))
            });
        Some(Self {
            table: Keys::of(description.key_schema())?,
            indexes: global.chain(local).collect(),
        })
    }

    /// The attributes an item read from `index` (or the table) starts with: the index's keys,
    /// then the table's.
    pub(crate) fn leading(&self, index: Option<&str>) -> Vec<String> {
        let index_keys = index.and_then(|index| self.indexes.get(index));
        let mut names: Vec<String> = Vec::new();
        for name in index_keys
            .into_iter()
            .flat_map(Keys::names)
            .chain(self.table.names())
        {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }
}

/// The keys described so far.
#[derive(Debug, Default)]
pub(crate) struct KeyCache(Mutex<HashMap<String, Arc<TableKeys>>>);

impl KeyCache {
    /// `table`'s keys, described the first time they are asked for, or again when `index`
    /// isn't among them.
    ///
    /// # Errors
    ///
    /// [`DriverError::Connection`] with DynamoDB's message when the table can't be described.
    pub(crate) async fn of(
        &self,
        client: &Client,
        table: &str,
        index: Option<&str>,
    ) -> Result<Arc<TableKeys>, DriverError> {
        if let Some(keys) = self.cached(table)
            && index.is_none_or(|index| keys.indexes.contains_key(index))
        {
            return Ok(keys);
        }
        let answer = client
            .describe_table()
            .table_name(table)
            .send()
            .await
            .map_err(|error| errors::connection(Refused::from(error)))?;
        let keys = answer
            .table()
            .and_then(TableKeys::of)
            .ok_or_else(|| errors::connection(Refused(format!("{table} has no key schema"))))?;
        let keys = Arc::new(keys);
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(table.to_owned(), Arc::clone(&keys));
        Ok(keys)
    }

    fn cached(&self, table: &str) -> Option<Arc<TableKeys>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(table)
            .cloned()
    }
}
