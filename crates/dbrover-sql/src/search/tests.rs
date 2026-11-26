use dbrover_core::{CatalogPath, NodeKind, RelationKind, SearchColumn, SearchableTable, TableName};

use super::*;

fn table(schema: &str, key: &[&str], columns: &[(&str, &str)]) -> SearchableTable {
    SearchableTable {
        database: None,
        name: TableName {
            schema: schema.to_owned(),
            name: "customers".to_owned(),
        },
        path: CatalogPath::root().child(NodeKind::Table, "customers"),
        relation: RelationKind::Table,
        key: key.iter().map(|name| (*name).to_owned()).collect(),
        row_estimate: None,
        columns: columns
            .iter()
            .map(|(name, data_type)| SearchColumn::new(*name, *data_type))
            .collect(),
    }
}

fn search(text: &str, matching: MatchKind, case: CaseMatch) -> DataSearch {
    match DataSearch::new(text, matching, case) {
        Ok(search) => search,
        Err(error) => panic!("{text:?}: {error}"),
    }
}

fn contains(text: &str) -> DataSearch {
    search(text, MatchKind::Contains, CaseMatch::Ignore)
}

fn sql(dialect: Dialect, table: &SearchableTable, search: &DataSearch) -> Option<String> {
    search_statement(dialect, table, search, RowsPerTable::DEFAULT).map(|statement| statement.sql)
}

/// A table of one column, to read a dialect's condition on it alone.
fn one_column(dialect_type: &str) -> SearchableTable {
    table("shop", &[], &[("note", dialect_type)])
}

#[test]
fn writes_the_statement_the_plan_shows() {
    let customers = table(
        "public",
        &["id"],
        &[
            ("id", "integer"),
            ("name", "text"),
            ("email", "character varying(200)"),
            ("photo", "bytea"),
        ],
    );
    let statement = search_statement(
        Dialect::Postgres,
        &customers,
        &contains("o'brien"),
        RowsPerTable::DEFAULT,
    );
    let expected = SearchStatement {
        sql: "SELECT \"id\", \"name\", \"email\"\n\
              FROM \"public\".\"customers\"\n\
              WHERE \"name\" ILIKE '%o''brien%' ESCAPE '!'\n   \
              OR \"email\" ILIKE '%o''brien%' ESCAPE '!'\n\
              LIMIT 101"
            .to_owned(),
        every_row: "SELECT \"id\", \"name\", \"email\"\n\
                    FROM \"public\".\"customers\"\n\
                    WHERE \"name\" ILIKE '%o''brien%' ESCAPE '!'\n   \
                    OR \"email\" ILIKE '%o''brien%' ESCAPE '!'"
            .to_owned(),
        selected: vec!["id".to_owned(), "name".to_owned(), "email".to_owned()],
        searched: vec!["name".to_owned(), "email".to_owned()],
    };
    assert_eq!(statement, Some(expected));
}

/// The hard value in every dialect: a quote, both wildcards, the escape character, a bracket
/// and a backslash, each matched as itself.
#[test]
fn escapes_the_hard_value_in_every_dialect() {
    let hard = contains(r"O'Brien 50%_off! [x] C:\y");
    let cases = [
        (
            Dialect::Postgres,
            "text",
            r#"SELECT "note"
FROM "shop"."customers"
WHERE "note" ILIKE '%O''Brien 50!%!_off!! [x] C:\y%' ESCAPE '!'
LIMIT 101"#,
        ),
        (
            Dialect::Sqlite,
            "TEXT",
            r#"SELECT "note"
FROM "shop"."customers"
WHERE "note" LIKE '%O''Brien 50!%!_off!! [x] C:\y%' ESCAPE '!'
LIMIT 101"#,
        ),
        (
            Dialect::DuckDb,
            "VARCHAR",
            r#"SELECT "note"
FROM "shop"."customers"
WHERE "note" ILIKE '%O''Brien 50!%!_off!! [x] C:\y%' ESCAPE '!'
LIMIT 101"#,
        ),
        (
            Dialect::Mysql,
            "varchar(80)",
            r"SELECT `note`
FROM `shop`.`customers`
WHERE `note` LIKE '%O''Brien 50!%!_off!! [x] C:\\y%' ESCAPE '!'
LIMIT 101",
        ),
        (
            Dialect::SqlServer,
            "nvarchar",
            r"SELECT TOP (101) [note]
FROM [shop].[customers]
WHERE [note] LIKE N'%O''Brien 50!%!_off!! ![x] C:\y%' ESCAPE '!'",
        ),
        (
            Dialect::ClickHouse,
            "String",
            r"SELECT `note`
FROM `shop`.`customers`
WHERE `note` ILIKE '%O''Brien 50\\%\\_off! [x] C:\\\\y%'
LIMIT 101",
        ),
        (
            Dialect::Trino,
            "varchar",
            r#"SELECT "note"
FROM "shop"."customers"
WHERE LOWER("note") LIKE '%o''brien 50!%!_off!! [x] c:\y%' ESCAPE '!'
LIMIT 101"#,
        ),
        (
            Dialect::BigQuery,
            "STRING",
            r"SELECT `note`
FROM `shop`.`customers`
WHERE LOWER(`note`) LIKE '%o\'brien 50\\%\\_off! [x] c:\\\\y%'
LIMIT 101",
        ),
        (
            Dialect::Snowflake,
            "VARCHAR(16777216)",
            r#"SELECT "note"
FROM "shop"."customers"
WHERE "note" ILIKE '%O''Brien 50!%!_off!! [x] C:\\y%' ESCAPE '!'
LIMIT 101"#,
        ),
        (
            Dialect::Databricks,
            "string",
            r"SELECT `note`
FROM `shop`.`customers`
WHERE `note` ILIKE '%O\'Brien 50!%!_off!! [x] C:\\y%' ESCAPE '!'
LIMIT 101",
        ),
    ];
    for (dialect, data_type, expected) in cases {
        assert_eq!(
            sql(dialect, &one_column(data_type), &hard).as_deref(),
            Some(expected),
            "{dialect:?}"
        );
    }
    assert_eq!(sql(Dialect::Cassandra, &one_column("text"), &hard), None);
}

#[test]
fn matches_case_as_each_dialect_can() {
    let exact = search("Kate", MatchKind::Contains, CaseMatch::Match);
    let whole = search("Kate", MatchKind::WholeValue, CaseMatch::Match);
    let condition = |dialect: Dialect, data_type: &str, search: &DataSearch| {
        sql(dialect, &one_column(data_type), search)
            .and_then(|sql| sql.lines().nth(2).map(str::to_owned))
    };
    let cases = [
        (
            Dialect::Postgres,
            "text",
            &exact,
            r#"WHERE "note" LIKE '%Kate%' ESCAPE '!'"#,
        ),
        (
            Dialect::Postgres,
            "text",
            &whole,
            r#"WHERE "note" = 'Kate'"#,
        ),
        (
            Dialect::Sqlite,
            "TEXT",
            &exact,
            r#"WHERE instr("note", 'Kate') > 0"#,
        ),
        (Dialect::Sqlite, "TEXT", &whole, r#"WHERE "note" = 'Kate'"#),
        (
            Dialect::Mysql,
            "varchar(80)",
            &exact,
            "WHERE `note` COLLATE utf8mb4_bin LIKE '%Kate%' ESCAPE '!'",
        ),
        (
            Dialect::SqlServer,
            "varchar",
            &whole,
            "WHERE [note] COLLATE Latin1_General_100_BIN2 = 'Kate'",
        ),
        (
            Dialect::BigQuery,
            "STRING",
            &exact,
            "WHERE `note` LIKE '%Kate%'",
        ),
    ];
    for (dialect, data_type, search, expected) in cases {
        assert_eq!(
            condition(dialect, data_type, search).as_deref(),
            Some(expected),
            "{dialect:?} {:?}",
            search.matching
        );
    }
}

#[test]
fn ignoring_case_on_a_column_that_tells_case_apart_lowers_both_sides() {
    let column = |collation: &str| SearchableTable {
        columns: vec![SearchColumn::new("note", "varchar(80)").with_collation(collation)],
        ..one_column("varchar(80)")
    };
    let latin = SearchableTable {
        columns: vec![SearchColumn::new("note", "varchar(80)").with_collation("latin1_swedish_ci")],
        ..one_column("varchar(80)")
    };
    let cases = [
        (
            Dialect::Mysql,
            column("utf8mb4_bin"),
            "WHERE LOWER(`note`) LIKE '%kate%' ESCAPE '!'",
        ),
        (
            Dialect::SqlServer,
            column("Latin1_General_CS_AS"),
            "WHERE LOWER([note]) LIKE '%kate%' ESCAPE '!'",
        ),
        (
            Dialect::Mysql,
            column("utf8mb4_0900_ai_ci"),
            "WHERE `note` LIKE '%Kate%' ESCAPE '!'",
        ),
    ];
    for (dialect, table, expected) in cases {
        let sql = sql(dialect, &table, &contains("Kate"));
        assert_eq!(
            sql.as_deref().and_then(|sql| sql.lines().nth(2)),
            Some(expected),
            "{dialect:?}"
        );
    }
    let exact = search("Kate", MatchKind::Contains, CaseMatch::Match);
    let latin = sql(Dialect::Mysql, &latin, &exact);
    assert_eq!(
        latin.as_deref().and_then(|sql| sql.lines().nth(2)),
        Some("WHERE `note` COLLATE latin1_bin LIKE '%Kate%' ESCAPE '!'"),
        "the binary collation of the column's own character set"
    );
}

#[test]
fn sql_server_writes_unicode_literals_only_for_unicode_columns() {
    let whole = search("Kate", MatchKind::WholeValue, CaseMatch::Ignore);
    let condition = |data_type: &str| {
        sql(Dialect::SqlServer, &one_column(data_type), &whole)
            .and_then(|sql| sql.lines().nth(2).map(str::to_owned))
    };
    assert_eq!(
        condition("varchar").as_deref(),
        Some("WHERE [note] = 'Kate'")
    );
    assert_eq!(
        condition("nvarchar").as_deref(),
        Some("WHERE [note] = N'Kate'")
    );
    assert_eq!(
        condition("sysname").as_deref(),
        Some("WHERE [note] = N'Kate'")
    );
}

#[test]
fn numbers_are_compared_only_where_they_fit() {
    let numbers = table(
        "shop",
        &[],
        &[
            ("tiny", "tinyint"),
            ("whole", "int"),
            ("price", "decimal(5,2)"),
            ("ratio", "float"),
        ],
    );
    let searched = |text: &str| {
        search_statement(
            Dialect::SqlServer,
            &numbers,
            &contains(text),
            RowsPerTable::DEFAULT,
        )
        .map(|statement| statement.searched)
    };
    let names = |names: &[&str]| Some(names.iter().map(|name| (*name).to_owned()).collect());
    assert_eq!(searched("42"), names(&["tiny", "whole", "price", "ratio"]));
    assert_eq!(searched("300"), names(&["whole", "price", "ratio"]));
    assert_eq!(searched("-1"), names(&["whole", "price", "ratio"]));
    assert_eq!(searched("3000000000"), names(&["ratio"]));
    assert_eq!(searched("42.50"), names(&["price", "ratio"]));
    assert_eq!(searched("42.505"), names(&["ratio"]));
    assert_eq!(
        searched("o'brien"),
        None,
        "a table of numbers searched for a name"
    );
}

#[test]
fn uuids_are_written_as_each_dialect_reads_them() {
    let uuid = contains("{3F2C8E1A-9B7D-4C3E-8A1F-0D2B4C6E8F10}");
    let cases = [
        (
            Dialect::Postgres,
            "uuid",
            "WHERE \"note\" = '3f2c8e1a-9b7d-4c3e-8a1f-0d2b4c6e8f10'",
        ),
        (
            Dialect::SqlServer,
            "uniqueidentifier",
            "WHERE [note] = N'3f2c8e1a-9b7d-4c3e-8a1f-0d2b4c6e8f10'",
        ),
        (
            Dialect::ClickHouse,
            "UUID",
            "WHERE `note` = toUUID('3f2c8e1a-9b7d-4c3e-8a1f-0d2b4c6e8f10')",
        ),
        (
            Dialect::Trino,
            "uuid",
            "WHERE \"note\" = UUID '3f2c8e1a-9b7d-4c3e-8a1f-0d2b4c6e8f10'",
        ),
    ];
    for (dialect, data_type, expected) in cases {
        let sql = sql(dialect, &one_column(data_type), &uuid);
        assert_eq!(
            sql.as_deref().and_then(|sql| sql.lines().nth(2)),
            Some(expected),
            "{dialect:?}"
        );
    }
}

#[test]
fn a_date_finds_its_day_in_date_and_timestamp_columns() {
    let day = contains("2026-12-31");
    let cases = [
        (
            Dialect::Postgres,
            "date",
            "WHERE \"note\" = DATE '2026-12-31'",
        ),
        (
            Dialect::Postgres,
            "timestamp without time zone",
            "WHERE (\"note\" >= TIMESTAMP '2026-12-31 00:00:00' \
             AND \"note\" < TIMESTAMP '2027-01-01 00:00:00')",
        ),
        (Dialect::SqlServer, "date", "WHERE [note] = '20261231'"),
        (
            Dialect::SqlServer,
            "datetime2",
            "WHERE ([note] >= '20261231' AND [note] < '20270101')",
        ),
        (
            Dialect::ClickHouse,
            "DateTime64(3)",
            "WHERE (`note` >= toDateTime('2026-12-31 00:00:00') \
             AND `note` < toDateTime('2027-01-01 00:00:00'))",
        ),
        (
            Dialect::BigQuery,
            "DATETIME",
            "WHERE (`note` >= DATETIME '2026-12-31 00:00:00' \
             AND `note` < DATETIME '2027-01-01 00:00:00')",
        ),
        (
            Dialect::BigQuery,
            "TIMESTAMP",
            "WHERE (`note` >= TIMESTAMP '2026-12-31 00:00:00' \
             AND `note` < TIMESTAMP '2027-01-01 00:00:00')",
        ),
    ];
    for (dialect, data_type, expected) in cases {
        let sql = sql(dialect, &one_column(data_type), &day);
        assert_eq!(
            sql.as_deref().and_then(|sql| sql.lines().nth(2)),
            Some(expected),
            "{dialect:?} {data_type}"
        );
    }
}

#[test]
fn documents_and_enums_are_read_as_text() {
    let cases = [
        (
            Dialect::Postgres,
            "jsonb",
            "WHERE CAST(\"note\" AS text) ILIKE '%paid%' ESCAPE '!'",
        ),
        (
            Dialect::Postgres,
            "enum",
            "WHERE CAST(\"note\" AS text) ILIKE '%paid%' ESCAPE '!'",
        ),
        (
            Dialect::Mysql,
            "json",
            "WHERE CAST(`note` AS CHAR) LIKE '%paid%' ESCAPE '!'",
        ),
        (
            Dialect::Mysql,
            "enum('paid','open')",
            "WHERE `note` LIKE '%paid%' ESCAPE '!'",
        ),
        (
            Dialect::SqlServer,
            "xml",
            "WHERE CAST([note] AS NVARCHAR(MAX)) LIKE N'%paid%' ESCAPE '!'",
        ),
        (
            Dialect::ClickHouse,
            "Enum8('paid' = 1)",
            "WHERE toString(`note`) ILIKE '%paid%'",
        ),
        (
            Dialect::Snowflake,
            "VARIANT",
            "WHERE TO_VARCHAR(\"note\") ILIKE '%paid%' ESCAPE '!'",
        ),
    ];
    for (dialect, data_type, expected) in cases {
        let sql = sql(dialect, &one_column(data_type), &contains("paid"));
        assert_eq!(
            sql.as_deref().and_then(|sql| sql.lines().nth(2)),
            Some(expected),
            "{dialect:?} {data_type}"
        );
    }
}

#[test]
fn a_whole_value_is_never_looked_for_in_a_document() {
    let whole = search("paid", MatchKind::WholeValue, CaseMatch::Ignore);
    assert_eq!(sql(Dialect::Postgres, &one_column("jsonb"), &whole), None);
    assert_eq!(
        sql(Dialect::Postgres, &one_column("enum"), &whole)
            .as_deref()
            .and_then(|sql| sql.lines().nth(2)),
        Some("WHERE CAST(\"note\" AS text) ILIKE 'paid' ESCAPE '!'")
    );
}

#[test]
fn the_key_comes_first_and_is_read_once() {
    let orders = table(
        "shop",
        &["ref", "line"],
        &[("line", "integer"), ("note", "text"), ("ref", "text")],
    );
    let statement = search_statement(
        Dialect::Postgres,
        &orders,
        &contains("7"),
        RowsPerTable::DEFAULT,
    );
    assert_eq!(
        statement.map(|statement| statement.selected),
        Some(vec!["ref".to_owned(), "line".to_owned(), "note".to_owned()])
    );
}

#[test]
fn says_which_engines_are_searched_and_which_ask_first() {
    assert_eq!(
        Dialect::Postgres.data_search(Some(ManagedService::Redshift)),
        SearchSupport::Unsupported
    );
    assert_eq!(
        Dialect::Postgres.data_search(Some(ManagedService::Neon)),
        SearchSupport::Searchable
    );
    for dialect in [
        Dialect::Trino,
        Dialect::BigQuery,
        Dialect::Snowflake,
        Dialect::Databricks,
    ] {
        assert_eq!(
            dialect.data_search(None),
            SearchSupport::AsksFirst,
            "{dialect:?}"
        );
    }
    assert_eq!(
        Dialect::SqlServer.data_search(Some(ManagedService::SynapseServerless)),
        SearchSupport::Unsupported
    );
}

#[test]
fn a_table_in_another_database_is_named_with_it() {
    let attached = SearchableTable {
        database: Some("lake".to_owned()),
        ..one_column("VARCHAR")
    };
    assert_eq!(
        sql(Dialect::DuckDb, &attached, &contains("x"))
            .as_deref()
            .and_then(|sql| sql.lines().nth(1)),
        Some("FROM \"lake\".\"shop\".\"customers\"")
    );
}

#[test]
fn a_table_without_a_key_reads_every_column_it_could_compare() {
    let events = table(
        "shop",
        &[],
        &[
            ("id", "integer"),
            ("note", "text"),
            ("payload", "bytea"),
            ("at", "date"),
        ],
    );
    let statement = search_statement(
        Dialect::Postgres,
        &events,
        &contains("refund"),
        RowsPerTable::DEFAULT,
    );
    assert_eq!(
        statement.map(|statement| (statement.selected, statement.searched)),
        Some((
            vec!["id".to_owned(), "note".to_owned(), "at".to_owned()],
            vec!["note".to_owned()]
        ))
    );
}
