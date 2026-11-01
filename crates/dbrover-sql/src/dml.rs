//! The statements the grid writes about a table's rows: one `UPDATE`, `INSERT` or `DELETE`
//! per row, naming the row by its primary key as it was loaded, and the `SELECT` that follows a
//! foreign key to the rows it names.
//!
//! Names are always quoted, so a statement means exactly that table and column whatever the
//! names spell. Values are string literals or `NULL`: the engines turn a string literal into
//! the column's type (PostgreSQL from an untyped literal, SQLite by type affinity, the others by
//! implicit conversion), which is how text from the grid (ADR-0004) goes back in without
//! DbRover knowing the types.

use dbrover_core::{Cell, ColumnDescription, Quoting, TableDescription, TableName, ValueKind};

use crate::Dialect;

/// A table as the database resolved it, or as the user named it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Table<'a> {
    /// `None` for a name the user gave without one, which the database resolves by its
    /// search path.
    pub schema: Option<&'a str>,
    pub name: &'a str,
}

impl<'a> From<&'a TableDescription> for Table<'a> {
    fn from(table: &'a TableDescription) -> Self {
        Self {
            schema: Some(&table.schema),
            name: &table.name,
        }
    }
}

impl<'a> From<&'a TableName> for Table<'a> {
    fn from(table: &'a TableName) -> Self {
        Self {
            schema: Some(&table.schema),
            name: &table.name,
        }
    }
}

/// A column and the value it has, or is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnValue<'a> {
    pub column: &'a str,
    pub value: &'a Cell,
}

/// A column of [`RowSql::insert_typed`], and the kind of value it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypedColumn<'a> {
    pub name: &'a str,
    pub kind: ValueKind,
}

/// Writes the statements that change one table's rows, in one dialect.
#[derive(Debug, Clone, Copy)]
pub struct RowSql<'a> {
    dialect: Dialect,
    table: Table<'a>,
    /// The table's columns, for a dialect that converts no string literal to a column's type
    /// (CQL), whose values are written as their types read them.
    columns: &'a [ColumnDescription],
}

impl<'a> RowSql<'a> {
    pub fn new(dialect: Dialect, table: impl Into<Table<'a>>) -> Self {
        Self {
            dialect,
            table: table.into(),
            columns: &[],
        }
    }

    /// For the described `table`, whose columns' types say how CQL's values are written.
    pub fn for_table(dialect: Dialect, table: &'a TableDescription) -> Self {
        Self {
            columns: &table.columns,
            ..Self::new(dialect, table)
        }
    }

    /// `UPDATE … SET … WHERE <key>`.
    pub fn update(&self, set: &[ColumnValue<'_>], key: &[ColumnValue<'_>]) -> String {
        let assignments: Vec<String> = set
            .iter()
            .map(|assigned| {
                format!(
                    "{} = {}",
                    self.name(assigned.column),
                    self.column_literal(assigned.column, assigned.value)
                )
            })
            .collect();
        format!(
            "UPDATE {} SET {} WHERE {}",
            self.table_name(),
            assignments.join(", "),
            self.condition(key)
        )
    }

    /// `INSERT` of one row with these values; the columns not given get their defaults. With
    /// no values at all, every column gets its default.
    pub fn insert(&self, values: &[ColumnValue<'_>]) -> String {
        if values.is_empty() {
            return format!(
                "INSERT INTO {} {}",
                self.table_name(),
                self.dialect.default_row()
            );
        }
        let columns: Vec<String> = values.iter().map(|value| self.name(value.column)).collect();
        let literals: Vec<String> = values
            .iter()
            .map(|value| self.column_literal(value.column, value.value))
            .collect();
        format!(
            "INSERT INTO {} ({}) VALUES ({})",
            self.table_name(),
            columns.join(", "),
            literals.join(", ")
        )
    }

    /// One `INSERT` of several rows, each with a value for every one of `columns`, in order.
    /// The columns not named get their defaults.
    pub fn insert_rows(&self, columns: &[&str], rows: &[Vec<Cell>]) -> String {
        let names: Vec<String> = columns.iter().map(|column| self.name(column)).collect();
        let values: Vec<String> = rows
            .iter()
            .map(|row| {
                let literals: Vec<String> = row.iter().map(|value| self.literal(value)).collect();
                format!("({})", literals.join(", "))
            })
            .collect();
        format!(
            "INSERT INTO {} ({}) VALUES\n{}",
            self.table_name(),
            names.join(", "),
            values.join(",\n")
        )
    }

    /// One `INSERT` of `rows`, each with a value for every one of `columns`, in order: on one
    /// line for a single row, else a row a line. A number in a numeric column is written bare,
    /// which reads better in a file meant to be read; every other value is a string literal.
    pub fn insert_typed(&self, columns: &[TypedColumn<'_>], rows: &[Vec<Cell>]) -> String {
        let names: Vec<String> = columns
            .iter()
            .map(|column| self.name(column.name))
            .collect();
        let values: Vec<String> = rows
            .iter()
            .map(|row| {
                let literals: Vec<String> = row
                    .iter()
                    .zip(columns)
                    .map(|(value, column)| self.typed_literal(value, column.kind))
                    .collect();
                format!("({})", literals.join(", "))
            })
            .collect();
        let separator = if values.len() == 1 { " " } else { "\n" };
        format!(
            "INSERT INTO {} ({}) VALUES{separator}{}",
            self.table_name(),
            names.join(", "),
            values.join(",\n")
        )
    }

    /// `DELETE … WHERE <key>`.
    pub fn delete(&self, key: &[ColumnValue<'_>]) -> String {
        format!(
            "DELETE FROM {} WHERE {}",
            self.table_name(),
            self.condition(key)
        )
    }

    /// `SELECT *` of the whole table, in the table's column order.
    pub fn select_all(&self) -> String {
        format!("SELECT * FROM {}", self.table_name())
    }

    /// `SELECT * … WHERE <each column equals its value>`: the rows a foreign key's values name,
    /// or the rows whose key names a given row.
    pub fn select(&self, filter: &[ColumnValue<'_>]) -> String {
        format!(
            "SELECT * FROM {} WHERE {}",
            self.table_name(),
            self.condition(filter)
        )
    }

    fn quoting(&self) -> Quoting {
        self.dialect.quoting()
    }

    /// Always quoted, so it means exactly this table whatever the names spell, and
    /// schema-qualified when the schema is known, so whatever the search path.
    fn table_name(&self) -> String {
        match self.table.schema {
            Some(schema) => format!("{}.{}", self.name(schema), self.name(self.table.name)),
            None => self.name(self.table.name),
        }
    }

    fn name(&self, name: &str) -> String {
        self.quoting().identifier(name)
    }

    fn literal(&self, value: &Cell) -> String {
        match value {
            None => "NULL".to_owned(),
            Some(text) => self.quoting().string_literal(text),
        }
    }

    /// `value` for `column`: a literal of its CQL type where the dialect needs one and the
    /// table's columns are known, a string literal otherwise.
    fn column_literal(&self, column: &str, value: &Cell) -> String {
        let cql_type = (self.dialect == Dialect::Cassandra)
            .then(|| {
                self.columns
                    .iter()
                    .find(|described| described.name == column)
            })
            .flatten();
        match (cql_type, value) {
            (Some(described), Some(text)) => crate::cql_value::literal(&described.data_type, text),
            _ => self.literal(value),
        }
    }

    fn typed_literal(&self, value: &Cell, kind: ValueKind) -> String {
        let numeric = matches!(
            kind,
            ValueKind::Integer | ValueKind::Decimal | ValueKind::Float
        );
        match value {
            Some(text) if numeric && is_plain_number(text) => text.clone(),
            value => self.literal(value),
        }
    }

    /// Every key column equal to its value; `IS NULL` for a `NULL`, which `=` never matches.
    fn condition(&self, key: &[ColumnValue<'_>]) -> String {
        key.iter()
            .map(|part| match part.value {
                None => format!("{} IS NULL", self.name(part.column)),
                value => format!(
                    "{} = {}",
                    self.name(part.column),
                    self.column_literal(part.column, value)
                ),
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    }
}

/// Whether `text` is a number every dialect reads as a numeric literal: digits with an
/// optional minus sign, fraction and exponent. `NaN`, `Infinity`, `1,5` and `.5` are not.
fn is_plain_number(text: &str) -> bool {
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => {
            let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
            (mantissa, Some(exponent))
        }
        None => (unsigned, None),
    };
    let mantissa_is_number = match mantissa.split_once('.') {
        Some((whole, fraction)) => digits(whole) && digits(fraction),
        None => digits(mantissa),
    };
    mantissa_is_number && exponent.is_none_or(digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: Table<'static> = Table {
        schema: Some("shop"),
        name: "order \"items\"",
    };

    fn text(value: &str) -> Cell {
        Some(value.to_owned())
    }

    /// The statements for one row of `TABLE`, in each dialect: an update, an insert of two
    /// values, an insert of nothing but defaults, and a delete.
    #[test]
    fn writes_each_statement_in_every_dialect() {
        let (id, note, empty) = (text("7"), text(r"it's C:\x"), None);
        let key = [ColumnValue {
            column: "id",
            value: &id,
        }];
        let values = [
            ColumnValue {
                column: "note",
                value: &note,
            },
            ColumnValue {
                column: "gone",
                value: &empty,
            },
        ];
        let expected = [
            (
                Dialect::Postgres,
                [
                    r#"UPDATE "shop"."order ""items""" SET "note" = 'it''s C:\x', "gone" = NULL WHERE "id" = '7'"#,
                    r#"INSERT INTO "shop"."order ""items""" ("note", "gone") VALUES ('it''s C:\x', NULL)"#,
                    r#"INSERT INTO "shop"."order ""items""" DEFAULT VALUES"#,
                    r#"DELETE FROM "shop"."order ""items""" WHERE "id" = '7'"#,
                ],
            ),
            (
                Dialect::Mysql,
                [
                    r#"UPDATE `shop`.`order "items"` SET `note` = 'it''s C:\\x', `gone` = NULL WHERE `id` = '7'"#,
                    r#"INSERT INTO `shop`.`order "items"` (`note`, `gone`) VALUES ('it''s C:\\x', NULL)"#,
                    r#"INSERT INTO `shop`.`order "items"` () VALUES ()"#,
                    r#"DELETE FROM `shop`.`order "items"` WHERE `id` = '7'"#,
                ],
            ),
            (
                Dialect::SqlServer,
                [
                    r#"UPDATE [shop].[order "items"] SET [note] = N'it''s C:\x', [gone] = NULL WHERE [id] = N'7'"#,
                    r#"INSERT INTO [shop].[order "items"] ([note], [gone]) VALUES (N'it''s C:\x', NULL)"#,
                    r#"INSERT INTO [shop].[order "items"] DEFAULT VALUES"#,
                    r#"DELETE FROM [shop].[order "items"] WHERE [id] = N'7'"#,
                ],
            ),
        ];
        for (dialect, [update, insert, defaults, delete]) in expected {
            let sql = RowSql::new(dialect, TABLE);
            assert_eq!(sql.update(&values, &key), update, "{dialect:?}");
            assert_eq!(sql.insert(&values), insert, "{dialect:?}");
            assert_eq!(sql.insert(&[]), defaults, "{dialect:?}");
            assert_eq!(sql.delete(&key), delete, "{dialect:?}");
        }
        for dialect in [Dialect::Sqlite, Dialect::DuckDb] {
            assert_eq!(
                RowSql::new(dialect, TABLE).insert(&[]),
                r#"INSERT INTO "shop"."order ""items""" DEFAULT VALUES"#
            );
        }
    }

    #[test]
    fn inserts_several_rows_at_once() {
        let rows = vec![vec![text("1"), None], vec![text("2"), text("it's")]];

        assert_eq!(
            RowSql::new(Dialect::Mysql, TABLE).insert_rows(&["id", "note"], &rows),
            "INSERT INTO `shop`.`order \"items\"` (`id`, `note`) VALUES\n('1', NULL),\n('2', 'it''s')"
        );
    }

    #[test]
    fn inserts_numbers_bare_in_numeric_columns_only() {
        let columns = [
            TypedColumn {
                name: "id",
                kind: ValueKind::Integer,
            },
            TypedColumn {
                name: "price",
                kind: ValueKind::Decimal,
            },
            TypedColumn {
                name: "code",
                kind: ValueKind::Text,
            },
        ];
        let sql = RowSql::new(Dialect::Postgres, TABLE);
        let one = vec![vec![text("7"), text("-1.5e3"), text("007")]];
        assert_eq!(
            sql.insert_typed(&columns, &one),
            r#"INSERT INTO "shop"."order ""items""" ("id", "price", "code") VALUES (7, -1.5e3, '007')"#
        );
        let two = vec![
            vec![text("8"), text("NaN"), None],
            vec![None, text("1,5"), text("x")],
        ];
        assert_eq!(
            sql.insert_typed(&columns, &two),
            concat!(
                r#"INSERT INTO "shop"."order ""items""" ("id", "price", "code") VALUES"#,
                "\n(8, 'NaN', NULL),\n(NULL, '1,5', 'x')"
            )
        );
    }

    #[test]
    fn names_a_table_without_its_schema_when_none_is_given() {
        let table = Table {
            schema: None,
            name: "order items",
        };
        assert_eq!(
            RowSql::new(Dialect::SqlServer, table).insert(&[]),
            "INSERT INTO [order items] DEFAULT VALUES"
        );
    }

    #[test]
    fn tells_plain_numbers_from_other_text() {
        for number in ["0", "-12", "3.25", "1e5", "-2.5E-3", "007"] {
            assert!(is_plain_number(number), "{number}");
        }
        for other in [
            "", "-", ".5", "5.", "1,5", "NaN", "Infinity", "1e", "0x1F", " 1", "+1",
        ] {
            assert!(!is_plain_number(other), "{other:?}");
        }
    }

    #[test]
    fn selects_the_rows_a_key_names() {
        let (region, id) = (text("eu"), text("42"));
        let filter = [
            ColumnValue {
                column: "region",
                value: &region,
            },
            ColumnValue {
                column: "id",
                value: &id,
            },
        ];

        assert_eq!(
            RowSql::new(Dialect::SqlServer, TABLE).select(&filter),
            r#"SELECT * FROM [shop].[order "items"] WHERE [region] = N'eu' AND [id] = N'42'"#
        );
    }

    #[test]
    fn names_a_null_key_part_with_is_null() {
        let (id, part) = (text("1"), None);
        let key = [
            ColumnValue {
                column: "id",
                value: &id,
            },
            ColumnValue {
                column: "part",
                value: &part,
            },
        ];

        assert_eq!(
            RowSql::new(Dialect::Postgres, TABLE).delete(&key),
            r#"DELETE FROM "shop"."order ""items""" WHERE "id" = '1' AND "part" IS NULL"#
        );
    }
}
