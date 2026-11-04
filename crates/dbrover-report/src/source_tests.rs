//! RP3's report file: lookups, declared columns, sources on every engine, and the rounds
//! sources load in.

use std::path::PathBuf;

use dbrover_core::{
    ConnectionConfig, CostGuard, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase,
    DynamoDbConfig, DynamoDbSignIn, MongoConfig, PostgresConfig, SafetyLevel, ServerConfig,
    TlsMode,
};
use dbrover_export::{ColumnType, DecimalSpec};
use dbrover_language::QueryLanguage;
use dbrover_sql::Dialect;

use super::*;

/// The valuation in `docs/reporting.md`'s RP3 section.
const VALUATION: &str = r##"
[report]
name = "Valuation"
params = { since = "timestamp", until = "timestamp" }

[[source]]
id = "tx"
data_source = "ledger-postgres"
query = """
SELECT id, wallet, asset_id, amount, executed_at
FROM valuation.transactions
WHERE executed_at >= :since AND executed_at < :until
"""
columns = [
  { name = "id",          type = "integer" },
  { name = "wallet",      type = "text" },
  { name = "asset_id",    type = "text" },
  { name = "amount",      type = "decimal(38,0)" },
  { name = "executed_at", type = "timestamp" },
]

[[source]]
id = "assets"
data_source = "assets-dynamodb"
lookup = { from = "tx", column = "asset_id", as = "asset_ids" }
query = "SELECT asset_id, symbol, unit, metadata FROM valuation_assets WHERE asset_id IN :asset_ids"
columns = [
  { name = "asset_id", type = "text" },
  { name = "symbol",   type = "text" },
  { name = "unit",     type = "decimal(38,18)" },
  { name = "chain",    from = "metadata.chain", type = "text", default = "unknown" },
]

[[source]]
id = "prices"
data_source = "prices-duckdb"
attach = true

[combine]
query = """
SELECT tx.executed_at, tx.wallet, assets.symbol, assets.chain,
       tx.amount * assets.unit AS quantity, p.price_usd,
       tx.amount * assets.unit * p.price_usd AS value_usd
FROM tx
JOIN assets USING (asset_id)
ASOF JOIN prices.hourly_prices p
  ON p.asset = assets.symbol AND p.hour <= tx.executed_at
ORDER BY tx.executed_at, tx.id
"""

[[output]]
format = "csv"
file = "valuation-{since:%Y-%m}.csv"
"##;

fn data_source(name: &str, config: ConnectionConfig) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: name.to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config,
    }
}

fn server(port: u16, database: &str) -> ServerConfig {
    ServerConfig {
        host: "localhost".to_owned(),
        port: port.into(),
        database: database.to_owned(),
        user: String::new(),
        tls: TlsMode::Disable,
        ssh: None,
        password_command: None,
        cloud_sign_in: None,
    }
}

fn dynamodb(name: &str) -> DataSource {
    data_source(
        name,
        ConnectionConfig::DynamoDb(DynamoDbConfig {
            sign_in: DynamoDbSignIn::LocalEmulator,
            profile: String::new(),
            region: String::new(),
            endpoint: "http://localhost:8000".to_owned(),
            cost_guard: CostGuard::default(),
        }),
    )
}

fn mongo(name: &str) -> DataSource {
    data_source(
        name,
        ConnectionConfig::Mongo(MongoConfig {
            server: server(27017, "crm"),
            auth_database: String::new(),
            connection_string: None,
        }),
    )
}

fn data_sources() -> Vec<DataSource> {
    vec![
        data_source(
            "ledger-postgres",
            ConnectionConfig::Postgres(PostgresConfig {
                server: server(5432, "ledger"),
            }),
        ),
        dynamodb("assets-dynamodb"),
        data_source(
            "prices-duckdb",
            ConnectionConfig::DuckDb(DuckDbConfig::new(DuckDbDatabase::File {
                path: PathBuf::from("/tmp/prices.duckdb"),
                read_only: true,
            })),
        ),
        mongo("crm"),
    ]
}

fn plan(text: &str) -> Result<Plan, Vec<String>> {
    Plan::new(
        read_report(text)?,
        &data_sources(),
        &[],
        &LimitDefaults::default(),
    )
}

fn combined(plan: &Plan) -> &Combined {
    match &plan.logic {
        Logic::Combine(combined) => combined,
        Logic::Direct(_) => panic!("the plan has no combine step"),
    }
}

fn round_ids(plan: &Plan) -> Vec<Vec<&str>> {
    combined(plan)
        .rounds()
        .into_iter()
        .map(|round| round.into_iter().map(|source| source.id.as_str()).collect())
        .collect()
}

#[test]
fn reads_and_plans_the_valuation_with_a_lookup_into_two_rounds() {
    let report = read_report(VALUATION).unwrap();
    let SourceRead::Query {
        lookup: Some(lookup),
        columns: Some(columns),
        ..
    } = &report.sources[1].read
    else {
        panic!("the assets source has a lookup and columns");
    };
    assert_eq!(
        *lookup,
        Lookup {
            from: "tx".to_owned(),
            column: "asset_id".to_owned(),
            parameter: "asset_ids".to_owned(),
            batch_size: None,
        }
    );
    assert_eq!(
        columns[3],
        SourceColumn {
            name: "chain".to_owned(),
            from: "metadata.chain".to_owned(),
            column_type: ColumnType::Text,
            default: Some("unknown".to_owned()),
        }
    );
    assert_eq!(
        columns[2].column_type,
        ColumnType::Decimal(DecimalSpec::new(38, 18))
    );
    assert_eq!(columns[0].from, "asset_id", "from is the name by default");

    let plan = plan(VALUATION).unwrap();
    assert_eq!(round_ids(&plan), [vec!["tx"], vec!["assets"]]);
    let assets = combined(&plan).rounds()[1][0];
    assert_eq!(assets.language, QueryLanguage::PartiQl);
    assert_eq!(
        assets.lookup.as_ref().map(|lookup| lookup.batch_size.get()),
        Some(100),
        "a PartiQL statement takes 100 values"
    );
    let tx = combined(&plan).rounds()[0][0];
    assert_eq!(tx.language, QueryLanguage::Sql(Dialect::Postgres));
    assert!(tx.columns.is_some(), "a SQL source may declare its columns");
    let names: Vec<&str> = plan
        .params
        .iter()
        .map(|param| param.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["since", "until"],
        "a lookup's parameter isn't the report's"
    );
    let starts = plan.source_starts();
    assert_eq!(starts[1].waits_for.as_deref(), Some("tx"));
    assert_eq!(starts[0].waits_for, None);
}

#[test]
fn runs_sources_in_rounds_of_what_their_lookups_need() {
    let text = r#"
[report]
name = "Chain"

[[source]]
id = "a"
data_source = "ledger-postgres"
query = "SELECT 1 AS x"

[[source]]
id = "b"
data_source = "ledger-postgres"
lookup = { from = "a", column = "x", as = "xs", batch_size = 10 }
query = "SELECT x AS y FROM t WHERE x IN :xs"

[[source]]
id = "c"
data_source = "ledger-postgres"
query = "SELECT 2 AS z"

[[source]]
id = "d"
data_source = "crm"
lookup = { from = "b", column = "y", as = "ys" }
query = "db.counterparties.find({_id: {$in: :ys}})"
columns = [{ name = "id", from = "_id" }]

[[source]]
id = "e"
data_source = "ledger-postgres"
lookup = { from = "a", column = "x", as = "xs" }
query = "SELECT * FROM u WHERE x IN :xs"

[[source]]
id = "prices"
data_source = "prices-duckdb"
attach = true

[combine]
query = "SELECT * FROM d"

[[output]]
format = "csv"
file = "out.csv"
"#;
    let plan = plan(text).unwrap();
    assert_eq!(
        round_ids(&plan),
        [vec!["a", "c"], vec!["b", "e"], vec!["d"]]
    );
    let b = combined(&plan).rounds()[1][0];
    assert_eq!(
        b.lookup.as_ref().map(|lookup| lookup.batch_size.get()),
        Some(10)
    );
}

#[test]
fn names_every_problem_with_lookups_and_declared_columns() {
    let lookup = "lookup = { from = \"tx\", column = \"asset_id\", as = \"asset_ids\" }";
    let cases: Vec<(String, &str)> = vec![
        (
            VALUATION.replace(lookup, "lookup = { from = \"prices\", column = \"asset_id\", as = \"asset_ids\" }"),
            "source \"assets\": lookup: \"prices\" is below this source",
        ),
        (
            VALUATION.replace(lookup, "lookup = { from = \"nowhere\", column = \"asset_id\", as = \"asset_ids\" }"),
            "source \"assets\": lookup: there is no source \"nowhere\" to read from",
        ),
        (
            VALUATION.replace(lookup, "lookup = { from = \"assets\", column = \"asset_id\", as = \"asset_ids\" }"),
            "source \"assets\": lookup: a source can't look up its own values",
        ),
        (
            VALUATION.replace(lookup, "lookup = { from = \"tx\", column = \"asset_id\", as = \"since\" }"),
            "source \"assets\": lookup: :since is one of [report] params",
        ),
        (
            VALUATION.replace(lookup, "lookup = { from = \"tx\", as = \"asset_ids\" }"),
            "source \"assets\": lookup needs column",
        ),
        (
            VALUATION.replace(lookup, "lookup = { from = \"tx\", column = \"asset_id\", as = \"asset ids\" }"),
            "source \"assets\": lookup: \"asset ids\" can't be a parameter's name",
        ),
        (
            VALUATION.replace(lookup, "lookup = { from = \"tx\", column = \"asset_id\", as = \"asset_ids\", batch = 5 }"),
            "source \"assets\": lookup: \"batch\" is not part of a lookup, which has from, column, \
             as and batch_size",
        ),
        (
            VALUATION.replace(lookup, "lookup = { from = \"tx\", column = \"asset_id\", as = \"asset_ids\", batch_size = 0 }"),
            "source \"assets\": lookup: batch_size must be a whole number greater than zero",
        ),
        (
            VALUATION.replace(lookup, "lookup = \"tx\""),
            "source \"assets\": lookup is a table",
        ),
        (
            VALUATION.replace("attach = true", "attach = true\nlookup = { from = \"tx\", column = \"a\", as = \"b\" }"),
            "source \"prices\": lookup is for a source's query, and an attached source has none",
        ),
        (
            VALUATION.replace("attach = true", "attach = true\ncolumns = [{ name = \"a\" }]"),
            "source \"prices\": columns is for a source's query, and an attached source has none",
        ),
        (
            VALUATION.replace(
                "{ name = \"symbol\",   type = \"text\" },",
                "{ name = \"Asset_ID\", type = \"text\" },",
            ),
            "source \"assets\": column \"Asset_ID\": another column already has this name",
        ),
        (
            VALUATION.replace("type = \"decimal(38,18)\"", "type = \"decimal(40,18)\""),
            "source \"assets\": column \"unit\": a source's decimal has at most 38 digits",
        ),
        (
            VALUATION.replace("type = \"decimal(38,18)\"", "type = \"money\""),
            "source \"assets\": column \"unit\": \"money\" is not a type",
        ),
        (
            VALUATION.replace("default = \"unknown\"", "default = \"unknown\", colour = 1"),
            "source \"assets\": column \"chain\": \"colour\" is not part of a source's column, \
             which has name, from, type and default",
        ),
        (
            VALUATION.replace(
                "{ name = \"wallet\",      type = \"text\" },",
                "{ name = \"wallet\",      type = \"date\", default = \"soon\" },",
            ),
            "source \"tx\": column \"wallet\": default: \"soon\" isn't a date",
        ),
        (
            VALUATION.replace("{ name = \"id\",          type = \"integer\" },", "{ type = \"integer\" },"),
            "source \"tx\": column 1: needs a name",
        ),
        (
            VALUATION.replace("{ name = \"id\",          type = \"integer\" },", "\"id\","),
            "source \"tx\": column 1: each column is a table",
        ),
        (
            VALUATION.replace(
                "columns = [\n  { name = \"id\"",
                "columns = []\nold_columns = [\n  { name = \"id\"",
            ),
            "source \"tx\": columns lists at least one column",
        ),
    ];
    for (text, expected) in cases {
        let found = read_report(&text).expect_err(&text);
        assert!(
            found.iter().any(|problem| problem.starts_with(expected)),
            "expected {expected:?} among {found:#?}"
        );
    }
}

#[test]
fn refuses_a_lookup_from_a_later_source() {
    let prices = "[[source]]\nid = \"prices\"\ndata_source = \"prices-duckdb\"\nattach = true\n\n";
    let attached_above = VALUATION.replace(prices, "").replace(
        "[[source]]\nid = \"assets\"",
        &format!("{prices}[[source]]\nid = \"assets\""),
    );
    let attached_above =
        attached_above.replace("lookup = { from = \"tx\"", "lookup = { from = \"prices\"");
    assert_eq!(
        read_report(&attached_above).unwrap_err(),
        [
            "source \"assets\": lookup: \"prices\" is attached, and a lookup reads a loaded \
             source's table; join the attached tables in [combine] instead"
        ]
    );

    let text = r#"
[report]
name = "R"

[[source]]
id = "first"
data_source = "ledger-postgres"
lookup = { from = "second", column = "x", as = "xs" }
query = "SELECT * FROM t WHERE x IN :xs"

[[source]]
id = "second"
data_source = "ledger-postgres"
query = "SELECT 1 AS x"

[combine]
query = "SELECT * FROM first"

[[output]]
format = "csv"
file = "out.csv"
"#;
    assert_eq!(
        read_report(text).unwrap_err(),
        [
            "source \"first\": lookup: \"second\" is below this source; a lookup reads a source \
             above it in the file, which runs first"
        ]
    );
}

#[test]
fn plans_lookups_and_sources_without_sql_with_their_problems() {
    let unused = VALUATION.replace("WHERE asset_id IN :asset_ids", "WHERE asset_id = 'btc'");
    assert_eq!(
        plan(&unused).unwrap_err(),
        [
            "source \"assets\": lookup: the query doesn't use :asset_ids, which the lookup's \
             values are bound to; read by them, as WHERE asset_id IN :asset_ids"
        ]
    );
    let too_many = VALUATION.replace(
        "as = \"asset_ids\" }",
        "as = \"asset_ids\", batch_size = 500 }",
    );
    assert_eq!(
        plan(&too_many).unwrap_err(),
        [
            "source \"assets\": lookup: batch_size is 500, more than the 100 values a PartiQL \
             statement takes"
        ]
    );
    let undeclared = VALUATION.replace(
        "columns = [\n  { name = \"asset_id\", type = \"text\" },\n  { name = \"symbol\",   type = \"text\" },\n  { name = \"unit\",     type = \"decimal(38,18)\" },\n  { name = \"chain\",    from = \"metadata.chain\", type = \"text\", default = \"unknown\" },\n]\n",
        "",
    );
    assert_ne!(undeclared, VALUATION);
    let found = plan(&undeclared).unwrap_err();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with(
            "source \"assets\": \"assets-dynamodb\" is a DynamoDB data source, whose results have \
             no fixed columns; declare the columns the source makes"
        ),
        "{found:?}"
    );

    let undeclared_param = VALUATION.replace(
        "WHERE asset_id IN :asset_ids",
        "WHERE asset_id IN :asset_ids AND kind = :kind",
    );
    assert_eq!(
        plan(&undeclared_param).unwrap_err(),
        [
            "source \"assets\": the query uses :kind, which [report] params doesn't declare; add \
             kind = \"text\" (or its type) to params"
        ]
    );
}
