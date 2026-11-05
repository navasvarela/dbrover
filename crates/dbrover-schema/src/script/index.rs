//! `CREATE INDEX` from a compared index.
//!
//! E-track's `IndexSpec` takes plain columns only, for the New index dialog. A compared index
//! keeps its key as the database printed it (expressions, `DESC`, operator classes), with
//! `INCLUDE` and a partial index's condition, so it is written here, from that text.

use dbrover_sql::{Dialect, ObjectName, quote_name};

use crate::Index;

pub(crate) fn create(name: &str, index: &Index, table: &ObjectName, dialect: Dialect) -> String {
    let unique = if index.unique { "UNIQUE " } else { "" };
    let mut sql = format!(
        "CREATE {unique}INDEX {} ON {} USING {} ({})",
        quote_name(dialect, name),
        table.to_sql(dialect),
        index.method,
        index.key.join(", ")
    );
    if !index.include.is_empty() {
        sql.push_str(&format!(" INCLUDE ({})", index.include.join(", ")));
    }
    if let Some(predicate) = &index.predicate {
        sql.push_str(&format!(" WHERE {predicate}"));
    }
    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_key_as_the_database_printed_it() {
        let index = Index {
            table: "orders".to_owned(),
            unique: true,
            method: "btree".to_owned(),
            key: vec![
                "customer_id DESC NULLS LAST".to_owned(),
                "lower(note)".to_owned(),
            ],
            include: vec!["total".to_owned()],
            predicate: Some("(total > (0)::numeric)".to_owned()),
        };

        let sql = create(
            "Orders idx",
            &index,
            &ObjectName::new(Some("staging"), "orders"),
            Dialect::Postgres,
        );

        assert_eq!(
            sql,
            "CREATE UNIQUE INDEX \"Orders idx\" ON staging.orders USING btree \
             (customer_id DESC NULLS LAST, lower(note)) INCLUDE (total) \
             WHERE (total > (0)::numeric)"
        );
    }
}
