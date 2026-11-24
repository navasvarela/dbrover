//! What a `[[source]]`'s `handoff` may say (RS7f): the keys it refuses, the engines that have
//! one, and the bucket its files can go in. Nothing here reaches a bucket or an engine: a
//! handoff is read and planned before a run starts.

use std::path::PathBuf;

use dbrover_core::{
    AthenaConfig, BigQueryConfig, ConnectionConfig, DataSource, DataSourceId, DatabricksConfig,
    DuckDbConfig, DuckDbDatabase, GcsBucket, ObjectStorage, S3Bucket, S3SignIn, SafetyLevel,
    SqliteConfig,
};

use super::*;

/// A report whose Athena source hands its result over, with `handoff` as the case writes it.
const ORDERS: &str = r##"
[report]
name = "Orders by item"

[[source]]
id = "orders"
data_source = "warehouse"
query = "SELECT item, amount FROM orders"
{handoff}
{keys}

[combine]
query = "SELECT item, sum(amount) AS total FROM orders GROUP BY item"

[[output]]
format = "csv"
file = "orders.csv"
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

fn athena() -> DataSource {
    data_source(
        "warehouse",
        ConnectionConfig::Athena(AthenaConfig {
            region: "eu-west-1".to_owned(),
            ..AthenaConfig::default()
        }),
    )
}

fn databricks() -> DataSource {
    data_source(
        "warehouse",
        ConnectionConfig::Databricks(DatabricksConfig::default()),
    )
}

fn bigquery() -> DataSource {
    data_source(
        "warehouse",
        ConnectionConfig::BigQuery(BigQueryConfig::default()),
    )
}

/// A DuckDB data source whose bucket is in `provider`'s own storage.
fn bucket(name: &str, storage: ObjectStorage) -> DataSource {
    data_source(
        name,
        ConnectionConfig::DuckDb(DuckDbConfig {
            storage: Some(storage),
            ..DuckDbConfig::new(DuckDbDatabase::InMemory)
        }),
    )
}

fn s3_bucket(name: &str) -> DataSource {
    bucket(
        name,
        ObjectStorage::S3(S3Bucket {
            bucket: "dbrover".to_owned(),
            sign_in: S3SignIn::Anonymous,
            ..S3Bucket::default()
        }),
    )
}

fn gcs_bucket(name: &str) -> DataSource {
    bucket(
        name,
        ObjectStorage::Gcs(GcsBucket {
            bucket: "dbrover".to_owned(),
            ..GcsBucket::default()
        }),
    )
}

/// The plan of `ORDERS` with `handoff` and `keys`, or the problems it was refused with.
fn plan_of(handoff: &str, keys: &str, sources: &[DataSource]) -> Result<Plan, Vec<String>> {
    let text = ORDERS.replace("{handoff}", handoff).replace("{keys}", keys);
    let report = read_report(&text)?;
    Plan::new(report, sources, &[], &[], &LimitDefaults::default())
}

/// The handoff of the planned source, which is what a run reads the files with.
fn planned(handoff: &str, sources: &[DataSource]) -> PlannedHandoff {
    let plan = plan_of(handoff, "", sources).unwrap_or_else(|problems| panic!("{problems:#?}"));
    let Logic::Combine(combined) = &plan.logic else {
        panic!("the report has a combine step");
    };
    combined
        .sources
        .iter()
        .find_map(|source| match source {
            PlannedSource::Query(source) => source.handoff.as_deref().cloned(),
            PlannedSource::Attached(_) | PlannedSource::Lake(_) | PlannedSource::Report(_) => None,
        })
        .expect("the source has a handoff")
}

#[test]
fn a_handoff_names_a_bucket_data_source_and_a_folder_under_it() {
    let sources = [athena(), s3_bucket("lake")];

    let whole = planned(
        "handoff = { data_source = \"lake\", prefix = \"dbrover-tmp\" }",
        &sources,
    );
    assert_eq!(whole.data_source.name, "lake");
    assert_eq!(whole.storage.url(), "s3://dbrover");
    // Every key a run writes is under the prefix, so a prefix is a folder however it is
    // written in the file.
    assert_eq!(whole.prefix.as_deref(), Some("dbrover-tmp/"));

    let own = planned("handoff = { data_source = \"lake\" }", &sources);
    assert_eq!(own.prefix, None, "Athena reads its own result file");
}

#[test]
fn names_every_problem_with_a_handoff() {
    let sqlite = data_source(
        "warehouse",
        ConnectionConfig::Sqlite(SqliteConfig {
            path: PathBuf::from("/tmp/ledger.db"),
            read_only: true,
        }),
    );
    let plain_duckdb = data_source(
        "plain-duckdb",
        ConnectionConfig::DuckDb(DuckDbConfig::new(DuckDbDatabase::File {
            path: PathBuf::from("/tmp/prices.duckdb"),
            read_only: true,
        })),
    );
    let athena_sources = [athena(), s3_bucket("lake"), gcs_bucket("gcs"), plain_duckdb];
    let databricks_sources = [databricks(), s3_bucket("lake")];
    let cases: [(&[DataSource], &str, &str, &str); 12] = [
        (
            &databricks_sources,
            "handoff = { data_source = \"lake\" }",
            "",
            "source \"orders\": \"warehouse\" is a Databricks data source, which hands its \
             result over by itself: the source reads it as Arrow files through Databricks's \
             external links, with no bucket of the report's. Drop handoff",
        ),
        (
            &athena_sources,
            "handoff = { data_source = \"nowhere\" }",
            "",
            "source \"orders\": there is no data source \"nowhere\"; there are gcs, lake, \
             plain-duckdb, warehouse",
        ),
        (
            &athena_sources,
            "handoff = { data_source = \"plain-duckdb\" }",
            "",
            "source \"orders\": \"plain-duckdb\" is a DuckDB data source without a bucket, \
             and a handoff's files go in one; add object storage to the data source's settings",
        ),
        (
            &athena_sources,
            "handoff = { data_source = \"gcs\" }",
            "",
            "source \"orders\": Amazon Athena writes a handoff's files to Amazon S3, and \
             \"gcs\" is Google Cloud Storage; name a data source whose bucket is in Amazon S3",
        ),
        (
            &[sqlite.clone(), s3_bucket("lake")],
            "handoff = { data_source = \"lake\" }",
            "",
            "source \"orders\": \"warehouse\" is a SQLite data source, which has no handoff: \
             it answers with rows, so the source reads them. Drop handoff, or reduce the \
             result in its own query",
        ),
        (
            &[bigquery(), gcs_bucket("lake")],
            "handoff = { data_source = \"lake\" }",
            "",
            "source \"orders\": BigQuery writes a handoff's files where the report says, so \
             the handoff needs a folder of its own: prefix = \"dbrover-tmp/\"",
        ),
        (
            &athena_sources,
            "handoff = { data_source = \"lake\", prefix = \"/dbrover-tmp\" }",
            "",
            "source \"orders\": handoff: \"/dbrover-tmp\" isn't a folder in the bucket; a \
             prefix is a path under it, as prefix = \"dbrover-tmp/\"",
        ),
        (
            &athena_sources,
            "handoff = { data_source = \"lake\", prefix = \"../etc/\" }",
            "",
            "source \"orders\": handoff: \"../etc/\" isn't a folder in the bucket; a prefix \
             is a path under it, as prefix = \"dbrover-tmp/\"",
        ),
        (
            &athena_sources,
            "handoff = { prefix = \"dbrover-tmp/\" }",
            "",
            "source \"orders\": handoff needs the data source whose bucket it writes in, by \
             name: data_source = \"lake\"",
        ),
        (
            &athena_sources,
            "handoff = \"s3://dbrover/dbrover-tmp/\"",
            "",
            "source \"orders\": handoff is a table, as handoff = { data_source = \"lake\", \
             prefix = \"dbrover-tmp/\" }",
        ),
        (
            &athena_sources,
            "handoff = { data_source = \"lake\", bucket = \"other\" }",
            "",
            "source \"orders\": handoff: \"bucket\" is not part of a handoff, which has \
             data_source and prefix",
        ),
        (
            &athena_sources,
            "handoff = { data_source = \"lake\" }",
            "max_rows = 1000",
            "source \"orders\": max_rows bounds the rows a source receives, and a handoff \
             receives files: the data source's cost guard bounds what its statement scans, \
             which is what the engine bills, and timeout still bounds the statement",
        ),
    ];
    for (sources, handoff, keys, expected) in cases {
        let problems = plan_of(handoff, keys, sources)
            .err()
            .unwrap_or_else(|| panic!("{handoff} {keys} was planned"));
        let messages: Vec<&str> = problems.iter().map(String::as_str).collect();
        assert!(
            messages.contains(&expected),
            "{handoff} {keys}: {messages:#?}"
        );
    }
}

/// The keys that count or shape rows a handoff never receives, each refused with what bounds
/// or shapes it instead.
#[test]
fn a_handoff_refuses_the_keys_that_bound_or_shape_paged_rows() {
    let sources = [athena(), s3_bucket("lake")];
    let cases = [
        ("max_bytes = \"1 GB\"", "max_bytes"),
        ("columnar = true", "columnar"),
        (
            "columns = [{ name = \"item\", type = \"text\" }]",
            "columns",
        ),
        (
            "lookup = { from = \"items\", column = \"item\", as = \"items\" }",
            "lookup",
        ),
        (
            "partition = { from = \"2026-01-01\", to = \"2026-02-01\", every = \"1 month\" }",
            "partition",
        ),
    ];
    for (key, named) in cases {
        let problems = plan_of("handoff = { data_source = \"lake\" }", key, &sources)
            .err()
            .unwrap_or_else(|| panic!("{key} was planned"));
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains(named) && problem.contains("handoff")),
            "{key}: {problems:#?}"
        );
    }
}

/// A report with one source and no combine step has no DuckDB to read a handoff's files with.
#[test]
fn a_handoff_needs_a_combine_step() {
    let text = r##"
[report]
name = "Orders"

[[source]]
id = "orders"
data_source = "warehouse"
query = "SELECT item, amount FROM orders"
handoff = { data_source = "lake" }

[[output]]
format = "csv"
file = "orders.csv"
"##;

    let problems = read_report(text)
        .err()
        .unwrap_or_else(|| panic!("a handoff without a combine step was read"));
    let messages: Vec<&str> = problems.iter().map(String::as_str).collect();
    assert!(
        messages.contains(
            &"source \"orders\": a handoff's files are read by a [combine] query, which this \
              report has none of; add one, or drop the handoff and let the report page its rows"
        ),
        "{messages:#?}"
    );
}
