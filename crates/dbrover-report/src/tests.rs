use std::path::PathBuf;
use std::time::Duration;

use dbrover_core::{
    BlobId, ConnectionConfig, DataSource, DataSourceId, DuckDbConfig, DuckDbDatabase,
    PostgresConfig, RedisConfig, SafetyLevel, ServerConfig, SqliteConfig, TlsMode,
};
use dbrover_export::{
    ColumnType, CustomFormat, CustomFormatId, CustomFormatKind, DecimalSpec, ExportFormat,
    FieldSeparator, ParquetOptions, Preset, SheetName,
};

use dbrover_language::QueryLanguage;

use super::*;

/// The trial balance in `docs/reporting.md`'s RP1 section.
const TRIAL_BALANCE: &str = r##"
[report]
name = "Trial balance"
description = "Debits, credits and the balance of each account in a period."
params = { since = "date", until = "date" }

[[source]]
id = "balances"
data_source = "ledger"
query = """
SELECT a.code AS account, a.name AS account_name,
       SUM(e.debit) AS debit, SUM(e.credit) AS credit,
       SUM(e.debit) - SUM(e.credit) AS balance
FROM entries e JOIN accounts a ON a.id = e.account_id
WHERE e.booked_on >= :since AND e.booked_on < :until
GROUP BY a.code, a.name
ORDER BY a.code
"""
max_rows = 100000

[[output]]
format = "xlsx"
file = "trial-balance-{since:%Y-%m}.xlsx"
sheet_name = "Trial balance"
columns = [
  { name = "Account", from = "account" },
  { name = "Name",    from = "account_name" },
  { name = "Debit",   from = "debit",   type = "decimal(18,2)", format = "#,##0.00" },
  { name = "Credit",  from = "credit",  type = "decimal(18,2)", format = "#,##0.00" },
  { name = "Balance", from = "balance", type = "decimal(18,2)", format = "#,##0.00;[Red]-#,##0.00" },
]

[[output]]
preset = "semicolon-csv"
file = "trial-balance-{since:%Y-%m}.csv"
columns = "same"
"##;

fn sqlite(name: &str) -> DataSource {
    DataSource {
        id: DataSourceId::generate(),
        name: name.to_owned(),
        color: None,
        safety: SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Sqlite(SqliteConfig {
            path: PathBuf::from("/tmp/ledger.db"),
            read_only: false,
        }),
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

fn redis(name: &str) -> DataSource {
    DataSource {
        config: ConnectionConfig::Redis(RedisConfig {
            server: server(6379, "0"),
            separator: ":".to_owned(),
        }),
        ..sqlite(name)
    }
}

fn postgres(name: &str) -> DataSource {
    DataSource {
        config: ConnectionConfig::Postgres(PostgresConfig {
            server: server(5432, "ledger"),
        }),
        ..sqlite(name)
    }
}

fn duckdb(name: &str, database: DuckDbDatabase) -> DataSource {
    DataSource {
        config: ConnectionConfig::DuckDb(DuckDbConfig::new(database)),
        ..sqlite(name)
    }
}

fn prices_file() -> DuckDbDatabase {
    DuckDbDatabase::File {
        path: PathBuf::from("/tmp/prices.duckdb"),
        read_only: true,
    }
}

/// The valuation in `docs/reporting.md`'s RP2 section.
const VALUATION: &str = r##"
[report]
name = "Valuation"
description = "Each transaction valued at its asset's price in the hour it ran."
params = { since = "timestamp", until = "timestamp" }

[[source]]
id = "tx"
data_source = "ledger-postgres"
query = """
SELECT id, wallet, asset, quantity, executed_at
FROM valuation.transactions
WHERE executed_at >= :since AND executed_at < :until
"""

[[source]]
id = "prices"
data_source = "prices-duckdb"
attach = true

[combine]
query = """
SELECT tx.executed_at, tx.wallet, tx.asset, tx.quantity, p.price_usd,
       tx.quantity * p.price_usd AS value_usd
FROM tx
ASOF JOIN prices.hourly_prices p
  ON p.asset = tx.asset AND p.hour <= tx.executed_at
ORDER BY tx.executed_at, tx.id
"""
memory_limit = "1 GiB"

[[output]]
format = "xlsx"
file = "valuation-{since:%Y-%m}.xlsx"
sheet_name = "Valuation"
columns = [
  { name = "Executed (UTC)", from = "executed_at", type = "timestamp", format = "yyyy-mm-dd hh:mm" },
  { name = "Wallet",         from = "wallet" },
  { name = "Asset",          from = "asset" },
  { name = "Quantity",       from = "quantity",  type = "decimal(38,18)" },
  { name = "Price (USD)",    from = "price_usd", type = "decimal(38,8)", format = "#,##0.00######" },
  { name = "Value (USD)",    from = "value_usd", type = "decimal(38,2)", format = "#,##0.00" },
]

[[output]]
format = "csv"
file = "valuation-{since:%Y-%m}.csv"
columns = "same"
"##;

fn valuation_sources() -> [DataSource; 2] {
    [
        postgres("ledger-postgres"),
        duckdb("prices-duckdb", prices_file()),
    ]
}

fn preset(id: &str, format: ExportFormat, options: &str) -> CustomFormat {
    let options: toml::Table = toml::from_str(options).unwrap();
    CustomFormat {
        id: CustomFormatId::new(id).unwrap(),
        name: id.to_owned(),
        description: None,
        extension: format.extension().to_owned(),
        kind: CustomFormatKind::Preset(Preset::read(format, toml::Value::Table(options)).unwrap()),
    }
}

fn presets() -> Vec<CustomFormat> {
    vec![preset(
        "semicolon-csv",
        ExportFormat::Csv,
        "separator = \";\"",
    )]
}

/// A report reading `from` with `query`, writing one CSV file.
fn one_source(params: &str, query: &str, output: &str) -> String {
    format!(
        "[report]\nname = \"R\"\nparams = {{ {params} }}\n\
         [[source]]\nid = \"s\"\ndata_source = \"ledger\"\nquery = \"{query}\"\n\
         [[output]]\n{output}\n"
    )
}

fn problems(text: &str) -> Vec<String> {
    read_report(text).expect_err(text)
}

#[test]
fn reads_the_trial_balance() {
    let report = read_report(TRIAL_BALANCE).unwrap();
    assert_eq!(report.name, "Trial balance");
    assert_eq!(
        report.description.as_deref(),
        Some("Debits, credits and the balance of each account in a period.")
    );
    assert_eq!(report.timeout, None);
    assert_eq!(
        report
            .params
            .iter()
            .map(|param| (param.name.as_str(), param.param_type))
            .collect::<Vec<_>>(),
        [("since", ColumnType::Date), ("until", ColumnType::Date)]
    );
    let [source] = report.sources.as_slice() else {
        panic!("{:?}", report.sources);
    };
    assert_eq!(source.id, "balances");
    assert_eq!(source.data_source, "ledger");
    let SourceRead::Query { limits, .. } = &source.read else {
        panic!("{:?}", source.read);
    };
    assert_eq!(
        *limits,
        SourceLimitsSet {
            max_rows: Some(100_000),
            ..SourceLimitsSet::default()
        }
    );
    assert_eq!(report.combine, None);
    let [excel, csv] = report.outputs.as_slice() else {
        panic!("{:?}", report.outputs);
    };
    let OutputWriter::Format(Preset { format, options }) = &excel.writer else {
        panic!("{:?}", excel.writer);
    };
    assert_eq!(*format, ExportFormat::Xlsx);
    assert_eq!(
        options.xlsx.sheet_name,
        SheetName::try_from("Trial balance".to_owned()).unwrap()
    );
    let OutputColumns::Declared(columns) = &excel.columns else {
        panic!("{:?}", excel.columns);
    };
    assert_eq!(columns.len(), 5);
    assert_eq!(columns[2].name, "Debit");
    assert_eq!(columns[2].from, "debit");
    assert_eq!(
        columns[2].column_type,
        Some(ColumnType::Decimal(DecimalSpec::new(18, 2)))
    );
    assert_eq!(
        columns[4].excel_format.as_deref(),
        Some("#,##0.00;[Red]-#,##0.00")
    );
    assert_eq!(
        csv.writer,
        OutputWriter::Preset(CustomFormatId::new("semicolon-csv").unwrap())
    );
    assert_eq!(csv.columns, OutputColumns::Same);
    assert_eq!(csv.file.as_str(), "trial-balance-{since:%Y-%m}.csv");
}

#[test]
fn reads_limits_and_defaults() {
    let report = read_report(
        r#"
        [report]
        name = "Ledger"
        timeout = "1h"
        [[source]]
        id = "entries"
        data_source = "ledger"
        query = "SELECT * FROM entries"
        max_rows = 5000000
        max_bytes = "4 GiB"
        timeout = "45m"
        [[output]]
        format = "csv"
        file = "ledger.csv"
        max_size = "2 GB"
        [[output]]
        format = "xlsx"
        file = "ledger.xlsx"
        continue_on_new_sheet = true
        "#,
    )
    .unwrap();
    assert_eq!(report.timeout, Some(Duration::from_secs(3600)));
    let SourceRead::Query { limits, .. } = &report.sources[0].read else {
        panic!("{:?}", report.sources);
    };
    assert_eq!(
        *limits,
        SourceLimitsSet {
            max_rows: Some(5_000_000),
            max_bytes: Some(ByteSize(4 << 30)),
            timeout: Some(Duration::from_secs(45 * 60)),
        }
    );
    assert_eq!(
        report.outputs[0].limits.max_size,
        Some(ByteSize(2_000_000_000))
    );
    assert_eq!(report.outputs[0].columns, OutputColumns::All);
    assert_eq!(report.outputs[1].limits, OutputLimitsSet::default());
    assert!(report.outputs[1].continue_on_new_sheet);
}

/// Each problem a file can have, and the sentence that names it.
#[test]
fn names_every_problem_with_its_place() {
    let output = "format = \"csv\"\nfile = \"out.csv\"";
    let cases: Vec<(String, &str)> = vec![
        ("name = ".to_owned(), "line 1:"),
        ("".to_owned(), "the file has no [report] table"),
        (
            one_source("", "SELECT 1", output).replace("name = \"R\"\n", ""),
            "[report]: needs a name",
        ),
        (
            one_source("since = \"day\"", "SELECT 1", output),
            "[report]: params: since: \"day\" is not a type",
        ),
        (
            one_source("1st = \"date\"", "SELECT 1", output),
            "[report]: params: \"1st\" can't be a parameter's name",
        ),
        (
            one_source("", "SELECT 1", output).replace("[report]\n", "[report]\ncolour = 1\n"),
            "[report]: \"colour\" is not part of [report]",
        ),
        (
            format!(
                "{}\n[[source]]\nid = \"t\"\ndata_source = \"x\"\nquery = \"SELECT 2\"",
                one_source("", "SELECT 1", output)
            ),
            "the file has 2 [[source]] tables; several sources are combined by a [combine] \
             query",
        ),
        (
            one_source("", "", output).replace("query = \"\"\n", "attach = true\n"),
            "source \"s\": an attached source is read in place by a [combine] query",
        ),
        (
            one_source("", "SELECT 1", output)
                .replace("id = \"s\"\n", "id = \"s\"\nkeys = \"t\"\n"),
            "source \"s\": \"keys\" is not part of a source, which has id, data_source, query, \
             lookup, partition, max_rows_per_part, columns, attach, max_rows, max_bytes and \
             timeout",
        ),
        (
            one_source("", "SELECT 1", output)
                .replace("id = \"s\"\n", "id = \"s\"\nmax_rows = 0\n"),
            "source \"s\": max_rows must be a whole number greater than zero",
        ),
        (
            one_source("", "SELECT 1", output)
                .replace("id = \"s\"\n", "id = \"s\"\ntimeout = \"soon\"\n"),
            "source \"s\": timeout must be a duration",
        ),
        (one_source("", "", output), "source \"s\": needs a query"),
        (
            one_source("", "SELECT 1", "file = \"out.csv\""),
            "output 1: needs a format",
        ),
        (
            one_source("", "SELECT 1", "format = \"markdown\"\nfile = \"out.md\""),
            "output 1: a report writes CSV, Excel or Parquet files, not Markdown",
        ),
        (
            one_source(
                "",
                "SELECT 1",
                "format = \"csv\"\npreset = \"x\"\nfile = \"out.csv\"",
            ),
            "output 1: an output has a format or a preset from exports.toml, not both",
        ),
        (
            one_source(
                "",
                "SELECT 1",
                "format = \"csv\"\nfile = \"out.csv\"\nsheet_name = \"S\"",
            ),
            "output 1: \"sheet_name\" is not part of an output",
        ),
        (
            one_source(
                "",
                "SELECT 1",
                "format = \"csv\"\nfile = \"out.csv\"\nseparator = \"ab\"",
            ),
            "output 1:",
        ),
        (
            one_source(
                "",
                "SELECT 1",
                "preset = \"semicolon-csv\"\nfile = \"out.csv\"\nbom = true",
            ),
            "output 1: \"bom\" can't be set on an output with a preset",
        ),
        (
            one_source("", "SELECT 1", "format = \"csv\""),
            "output 1: needs a file name",
        ),
        (
            one_source("", "SELECT 1", "format = \"csv\"\nfile = \"{since}.csv\""),
            "output 1: file: {since} names no parameter of the report",
        ),
        (
            one_source(
                "",
                "SELECT 1",
                "format = \"csv\"\nfile = \"x.csv\"\ncolumns = \"same\"",
            ),
            "output 1: columns = \"same\" takes the first output's columns, and this is the first",
        ),
        (
            one_source(
                "",
                "SELECT 1",
                "format = \"csv\"\nfile = \"x.csv\"\ncolumns = [{ name = \"A\", type = \"money\" }]",
            ),
            "output 1: column \"A\": \"money\" is not a type",
        ),
        (
            one_source(
                "",
                "SELECT 1",
                "format = \"csv\"\nfile = \"x.csv\"\ncolumns = [{ name = \"A\", time_zone = \"UTC\" }]",
            ),
            "output 1: column \"A\": time_zone is for a timestamp column",
        ),
        (
            one_source(
                "",
                "SELECT 1",
                "format = \"csv\"\nfile = \"x.csv\"\ncolumns = [{ name = \"A\", type = \"timestamp\", time_zone = \"Mars/Olympus\" }]",
            ),
            "output 1: column \"A\": \"Mars/Olympus\" is not a time zone",
        ),
        (
            one_source(
                "",
                "SELECT 1",
                "format = \"csv\"\nfile = \"x.csv\"\ncolumns = [{ name = \"A\", if_invalid = \"skip\" }]",
            ),
            "output 1: column \"A\": \"skip\" is not what to do with a value",
        ),
        (
            one_source(
                "",
                "SELECT 1",
                "format = \"csv\"\nfile = \"x.csv\"\ncolumns = [{ name = \"A\", width = 3 }]",
            ),
            "output 1: column \"A\": \"width\" is not part of a column",
        ),
        (
            one_source(
                "",
                "SELECT 1",
                "format = \"csv\"\nfile = \"x.csv\"\ncontinue_on_new_sheet = true",
            ),
            "output 1: continue_on_new_sheet is for Excel outputs",
        ),
        (
            format!(
                "{}\n[[output]]\nformat = \"csv\"\nfile = \"out.csv\"",
                one_source("", "SELECT 1", output)
            ),
            "output 2: another output already writes \"out.csv\"",
        ),
        (
            one_source("", "SELECT 1", output)
                .replace("[[output]]\nformat = \"csv\"\nfile = \"out.csv\"\n", ""),
            "the file has no [[output]] table",
        ),
    ];
    for (text, expected) in cases {
        let found = problems(&text);
        assert!(
            found.iter().any(|problem| problem.starts_with(expected)),
            "expected {expected:?} among {found:#?} for\n{text}"
        );
    }
}

#[test]
fn lists_every_problem_rather_than_the_first() {
    let found = problems(
        r#"
        [report]
        params = { since = "day" }
        [[source]]
        id = "s"
        query = "SELECT 1"
        [[output]]
        format = "csv"
        "#,
    );
    assert_eq!(
        found,
        [
            "[report]: params: since: \"day\" is not a type; a column's type is text, integer, \
             decimal, decimal(p,s), boolean, date or timestamp",
            "[report]: needs a name, as name = \"Trial balance\"",
            "source \"s\": needs the data source it reads, by name: data_source = \"ledger\"",
            "output 1: needs a file name, as file = \"trial-balance.xlsx\"",
        ]
    );
}

#[test]
fn plans_the_trial_balance_with_its_preset_and_the_same_columns() {
    let ledger = sqlite("ledger");
    let plan = Plan::new(
        read_report(TRIAL_BALANCE).unwrap(),
        &[sqlite("other"), ledger.clone()],
        &presets(),
        &LimitDefaults::default(),
    )
    .unwrap();
    let Logic::Direct(source) = &plan.logic else {
        panic!("{:?}", plan.logic);
    };
    assert_eq!(source.data_source, ledger);
    assert_eq!(
        source.language,
        QueryLanguage::Sql(dbrover_sql::Dialect::Sqlite)
    );
    let [excel, csv] = plan.outputs.as_slice() else {
        panic!("{:?}", plan.outputs);
    };
    assert!(matches!(excel.format, OutputFormat::Xlsx(_)));
    let OutputFormat::Csv(options) = &csv.format else {
        panic!("{:?}", csv.format);
    };
    assert_eq!(options.separator, FieldSeparator::SEMICOLON);
    assert_eq!(csv.columns, excel.columns);
    assert_eq!(csv.columns.as_ref().map(Vec::len), Some(5));
}

#[test]
fn finds_the_data_source_by_name_or_by_id() {
    let report = |data_source: &str| {
        read_report(&one_source(
            "",
            "SELECT 1",
            "format = \"csv\"\nfile = \"x.csv\"",
        ))
        .map(|mut report| {
            report.sources[0].data_source = data_source.to_owned();
            report
        })
        .unwrap()
    };
    let twins = [sqlite("ledger"), sqlite("ledger")];
    let by_id = Plan::new(
        report(&twins[1].id.to_string()),
        &twins,
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    let Logic::Direct(source) = &by_id.logic else {
        panic!("{:?}", by_id.logic);
    };
    assert_eq!(source.data_source.id, twins[1].id);

    let ambiguous =
        Plan::new(report("ledger"), &twins, &[], &LimitDefaults::default()).unwrap_err();
    assert!(
        ambiguous[0].starts_with(
            "source \"s\": 2 data sources are named \"ledger\"; name one by its id instead: "
        ),
        "{ambiguous:?}"
    );
    let missing = Plan::new(
        report("books"),
        &[sqlite("ledger"), sqlite("audit")],
        &[],
        &LimitDefaults::default(),
    )
    .unwrap_err();
    assert_eq!(
        missing,
        ["source \"s\": there is no data source \"books\"; there are audit, ledger"]
    );
    let documents = Plan::new(
        report("cache"),
        &[redis("cache")],
        &[],
        &LimitDefaults::default(),
    )
    .unwrap_err();
    assert!(
        documents[0].starts_with("source \"s\": \"cache\" is a Redis"),
        "{documents:?}"
    );
}

#[test]
fn checks_presets_against_exports_toml() {
    let report = |output: &str| read_report(&one_source("", "SELECT 1", output)).unwrap();
    let ledger = [sqlite("ledger")];
    let missing = Plan::new(
        report("preset = \"tabs\"\nfile = \"x.csv\""),
        &ledger,
        &presets(),
        &LimitDefaults::default(),
    )
    .unwrap_err();
    assert!(missing[0].starts_with("output 1: exports.toml has no export with the id \"tabs\""));
    let markdown = [preset("notes", ExportFormat::Markdown, "")];
    let not_csv = Plan::new(
        report("preset = \"notes\"\nfile = \"x.md\""),
        &ledger,
        &markdown,
        &LimitDefaults::default(),
    )
    .unwrap_err();
    assert_eq!(
        not_csv,
        [
            "output 1: the preset \"notes\": a report writes CSV, Excel or Parquet files, not Markdown"
        ]
    );
    let not_excel = Plan::new(
        report("preset = \"semicolon-csv\"\nfile = \"x.csv\"\ncontinue_on_new_sheet = true"),
        &ledger,
        &presets(),
        &LimitDefaults::default(),
    )
    .unwrap_err();
    assert_eq!(
        not_excel,
        ["output 1: continue_on_new_sheet is for Excel outputs"]
    );
}

#[test]
fn orders_parameters_as_the_query_uses_them_and_refuses_undeclared_ones() {
    let report = read_report(&one_source(
        "account = \"text\", until = \"date\", since = \"date\"",
        "SELECT * FROM e WHERE d >= :since AND d < :until",
        "format = \"csv\"\nfile = \"{account}.csv\"",
    ))
    .unwrap();
    let plan = Plan::new(report, &[sqlite("ledger")], &[], &LimitDefaults::default()).unwrap();
    assert_eq!(
        plan.params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>(),
        ["since", "until", "account"]
    );

    let undeclared = read_report(&one_source(
        "since = \"date\"",
        "SELECT * FROM e WHERE d >= :since AND d < :until",
        "format = \"csv\"\nfile = \"x.csv\"",
    ))
    .unwrap();
    assert_eq!(
        Plan::new(
            undeclared,
            &[sqlite("ledger")],
            &[],
            &LimitDefaults::default()
        )
        .unwrap_err(),
        [
            "source \"s\": the query uses :until, which [report] params doesn't declare; add \
             until = \"text\" (or its type) to params"
        ]
    );
}

#[test]
fn checks_values_and_names_the_files() {
    let plan = Plan::new(
        read_report(TRIAL_BALANCE).unwrap(),
        &[sqlite("ledger")],
        &presets(),
        &LimitDefaults::default(),
    )
    .unwrap();
    let typed = |since: &str, until: &str| -> TypedValues {
        [("since", since), ("until", until)]
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect()
    };
    let values = plan.values(&typed("2026-10-01", " 2026-11-01 ")).unwrap();
    assert_eq!(
        values.parameters.get("until"),
        Some(&Some("2026-11-01".to_owned()))
    );
    assert_eq!(
        values.files,
        ["trial-balance-2026-10.xlsx", "trial-balance-2026-10.csv"]
    );
    assert_eq!(
        plan.values(&typed("", "2026-13-01")).unwrap_err(),
        [
            "since needs a value",
            "until: \"2026-13-01\" isn't a date, as 2026-10-01",
        ]
    );
}

#[test]
fn refuses_values_that_make_two_outputs_one_file() {
    let report = read_report(&one_source(
        "a = \"text\", b = \"text\"",
        "SELECT 1",
        "format = \"csv\"\nfile = \"{a}.csv\"\n[[output]]\nformat = \"csv\"\nfile = \"{b}.csv\"",
    ))
    .unwrap();
    let plan = Plan::new(report, &[sqlite("ledger")], &[], &LimitDefaults::default()).unwrap();
    let same: TypedValues = [("a", "x"), ("b", "x")]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    assert_eq!(
        plan.values(&same).unwrap_err(),
        ["output 2: another output already writes \"x.csv\" with these values"]
    );
}

#[test]
fn reads_and_plans_the_valuation() {
    let report = read_report(VALUATION).unwrap();
    let ids: Vec<(&str, bool)> = report
        .sources
        .iter()
        .map(|source| (source.id.as_str(), source.read == SourceRead::Attach))
        .collect();
    assert_eq!(ids, [("tx", false), ("prices", true)]);
    let combine = report.combine.clone().unwrap();
    assert!(report.quiet_hints.is_empty());
    let quiet = read_report(&VALUATION.replace(
        "[report]\n",
        "[report]\nquiet_hints = [\"source-reads-more\", \"near-limit\", \"source-reads-more\"]\n",
    ))
    .unwrap();
    assert_eq!(
        quiet.quiet_hints,
        [HintId::SourceReadsMore, HintId::NearLimit]
    );
    assert!(combine.query.sql().contains("ASOF JOIN"));
    assert_eq!(combine.query.compiled(), None);
    assert_eq!(
        combine.limits,
        CombineLimitsSet {
            memory_limit: Some(ByteSize(1 << 30)),
            ..CombineLimitsSet::default()
        }
    );

    let plan = Plan::new(report, &valuation_sources(), &[], &LimitDefaults::default()).unwrap();
    let Logic::Combine(combined) = &plan.logic else {
        panic!("{:?}", plan.logic);
    };
    let [PlannedSource::Query(tx), PlannedSource::Attached(prices)] = combined.sources.as_slice()
    else {
        panic!("{:?}", combined.sources);
    };
    assert_eq!(
        tx.language,
        QueryLanguage::Sql(dbrover_sql::Dialect::Postgres)
    );
    assert_eq!(prices.path, PathBuf::from("/tmp/prices.duckdb"));
    assert_eq!(
        plan.params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>(),
        ["since", "until"]
    );
    assert_eq!(
        plan.source_starts(),
        [
            SourceStart {
                id: "tx".to_owned(),
                data_source: "ledger-postgres".to_owned(),
                attached: false,
                waits_for: None,
                engine: Some("PostgreSQL".to_owned()),
                limits: Some(SourceCaps::from(&SourceLimits::default())),
                partition: None,
            },
            SourceStart {
                id: "prices".to_owned(),
                data_source: "prices-duckdb".to_owned(),
                attached: true,
                waits_for: None,
                engine: Some("DuckDB".to_owned()),
                limits: None,
                partition: None,
            },
        ]
    );
    let machine = Machine {
        cores: 8,
        dbrover: "0.31.0".to_owned(),
    };
    let RunEvent::Started {
        combine,
        combine_limits,
        timeout_ms,
        machine: started_on,
        ..
    } = plan.started(BlobId::of(b""), machine.clone())
    else {
        panic!("not a start");
    };
    assert!(combine);
    assert_eq!(
        combine_limits,
        Some(CombineCaps {
            memory_limit: 1 << 30,
            max_temp_size: DEFAULT_MAX_TEMP_SIZE.0,
            timeout_ms: u64::try_from(DEFAULT_COMBINE_TIMEOUT.as_millis()).unwrap(),
        })
    );
    assert_eq!(
        timeout_ms,
        Some(u64::try_from(DEFAULT_RUN_TIMEOUT.as_millis()).unwrap())
    );
    assert_eq!(started_on, Some(machine));
}

#[test]
fn reads_the_combine_steps_limits() {
    let text = VALUATION.replace(
        "memory_limit = \"1 GiB\"",
        "memory_limit = \"512 MiB\"\nthreads = 3\ntimeout = \"2m\"\nmax_temp_size = \"20 GB\"",
    );
    let limits = read_report(&text).unwrap().combine.unwrap().limits;
    assert_eq!(
        limits,
        CombineLimitsSet {
            timeout: Some(Duration::from_secs(120)),
            memory_limit: Some(ByteSize(512 << 20)),
            threads: std::num::NonZeroUsize::new(3),
            max_temp_size: Some(ByteSize(20_000_000_000)),
        }
    );
    assert_eq!(limits.or(CombineLimits::default()).threads(), 3);
    assert!(CombineLimits::default().threads() >= 1);
}

/// What a report's file sets wins, key by key; what it leaves out comes from the defaults,
/// which Settings › Reports may have changed.
#[test]
fn takes_the_limits_a_file_leaves_out_from_the_defaults() {
    let defaults = LimitDefaults {
        source: SourceLimits {
            max_rows: 10,
            max_bytes: ByteSize(1 << 20),
            timeout: Duration::from_secs(60),
        },
        combine: CombineLimits {
            timeout: Duration::from_secs(30),
            memory_limit: ByteSize(256 << 20),
            threads: std::num::NonZeroUsize::new(2),
            max_temp_size: ByteSize(1 << 30),
        },
        output: OutputLimits {
            max_size: ByteSize(5 << 20),
        },
        run_timeout: Duration::from_secs(90),
        max_disk: ByteSize(1 << 30),
        stall_timeout: Duration::from_secs(120),
    };
    let report = read_report(VALUATION).unwrap();
    let plan = Plan::new(report, &valuation_sources(), &[], &defaults).unwrap();
    assert_eq!(plan.timeout, Duration::from_secs(90));
    let Logic::Combine(combined) = &plan.logic else {
        panic!("{:?}", plan.logic);
    };
    let PlannedSource::Query(tx) = &combined.sources[0] else {
        panic!("{:?}", combined.sources);
    };
    assert_eq!(tx.limits, defaults.source);
    assert_eq!(
        combined.combine.limits,
        CombineLimits {
            memory_limit: ByteSize(1 << 30),
            ..defaults.combine
        },
        "the file's memory_limit wins"
    );
    assert!(
        plan.outputs
            .iter()
            .all(|output| output.limits == defaults.output)
    );
}

/// Each problem RP2's keys can have, and the sentence that names it.
#[test]
fn names_every_problem_with_several_sources() {
    let cases: Vec<(String, &str)> = vec![
        (
            VALUATION.replace("id = \"tx\"", "id = \"my tx\""),
            "source \"my tx\": an id names the source's table in [combine], so it is letters, \
             digits and _, starting with a letter",
        ),
        (
            VALUATION.replace("id = \"prices\"", "id = \"Memory\""),
            "source \"Memory\": \"Memory\" is a name DuckDB keeps for itself",
        ),
        (
            VALUATION.replace("id = \"prices\"", "id = \"TX\""),
            "source \"TX\": another source already has this id",
        ),
        (
            VALUATION.replace("attach = true", "attach = true\nquery = \"SELECT 1\""),
            "source \"prices\": an attached source has no query",
        ),
        (
            VALUATION.replace("attach = true", "attach = true\nmax_rows = 10"),
            "source \"prices\": max_rows bounds what a source's query copies",
        ),
        (
            VALUATION.replace("attach = true", "attach = \"yes\""),
            "source \"prices\": attach must be true or false",
        ),
        (
            VALUATION.replace("attach = true", "attach = false"),
            "source \"prices\": needs a query, or attach = true to read a DuckDB file in place",
        ),
        (
            VALUATION.replace(
                "[combine]\nquery = \"\"\"",
                "[combine]\nprql = \"from tx\"\nquery = \"\"\"",
            ),
            "[combine]: has both a query and prql",
        ),
        (
            VALUATION.replace(
                "[combine]\nquery = \"\"\"",
                "[combine]\nprql = \"from tx | derive x = \"\nquery_was = \"\"\"",
            ),
            "[combine]: prql, line 1, column ",
        ),
        (
            VALUATION.replace("[report]\n", "[report]\nquiet_hints = [\"spills\"]\n"),
            "[report]: quiet_hints: \"spills\" isn't a hint's id; the ids are source-reads-more, \
             combine-spilled, slow-first-row, load-bound, many-batches, round-wait, writer-bound, \
             near-limit, unused-columns, filter-one-source, aggregate-one-source and \
             scanned-more",
        ),
        (
            VALUATION.replace(
                "[report]\n",
                "[report]\nquiet_hints = \"combine-spilled\"\n",
            ),
            "[report]: quiet_hints is a list of hints' ids",
        ),
        (
            VALUATION.replace("[report]\n", "[report]\nprql = \"from tx\"\n"),
            "[report]: PRQL goes in [combine]",
        ),
        (
            VALUATION.replace("memory_limit = \"1 GiB\"", "threads = 0"),
            "[combine]: threads must be a whole number greater than zero",
        ),
        (
            VALUATION.replace("memory_limit = \"1 GiB\"", "memory = \"1 GiB\""),
            "[combine]: \"memory\" is not part of [combine], which has query or prql, \
             timeout, memory_limit, threads and max_temp_size",
        ),
        (
            VALUATION.replace("[combine]\nquery = \"\"\"", "[combine]\nsql = \"\"\""),
            "[combine]: needs a query, the DuckDB SQL that combines the sources' tables, or \
             prql",
        ),
    ];
    for (text, expected) in cases {
        let found = problems(&text);
        assert!(
            found.iter().any(|problem| problem.starts_with(expected)),
            "expected {expected:?} among {found:#?}"
        );
    }
}

#[test]
fn plans_attached_sources_only_on_duckdb_files_and_a_combine_that_only_reads() {
    let report = || read_report(VALUATION).unwrap();
    let [ledger, _] = valuation_sources();
    let on_postgres = Plan::new(
        report(),
        &[ledger.clone(), postgres("prices-duckdb")],
        &[],
        &LimitDefaults::default(),
    )
    .unwrap_err();
    assert_eq!(
        on_postgres,
        [
            "source \"prices\": attach = true reads a DuckDB file in place, and \
             \"prices-duckdb\" is a PostgreSQL data source; give the source a query instead"
        ]
    );
    let in_memory = Plan::new(
        report(),
        &[
            ledger.clone(),
            duckdb("prices-duckdb", DuckDbDatabase::InMemory),
        ],
        &[],
        &LimitDefaults::default(),
    )
    .unwrap_err();
    assert!(
        in_memory[0]
            .starts_with("source \"prices\": \"prices-duckdb\" is a DuckDB database in memory"),
        "{in_memory:?}"
    );

    let writes = VALUATION
        .replace(
            "SELECT tx.executed_at",
            "INSERT INTO prices.hourly_prices SELECT tx.executed_at",
        )
        .replace("ASOF JOIN", "JOIN");
    let refused = Plan::new(
        read_report(&writes).unwrap(),
        &valuation_sources(),
        &[],
        &LimitDefaults::default(),
    )
    .unwrap_err();
    assert_eq!(
        refused,
        ["[combine]: the query changes data; the combine step only reads the sources' tables"]
    );
    let two = VALUATION.replace("ORDER BY tx.executed_at, tx.id", "ORDER BY 1; SELECT 2");
    let refused = Plan::new(
        read_report(&two).unwrap(),
        &valuation_sources(),
        &[],
        &LimitDefaults::default(),
    )
    .unwrap_err();
    assert_eq!(
        refused,
        ["[combine]: the query holds 2 statements; the combine step is one query that only reads"]
    );
    let unparsed = VALUATION.replace("SELECT tx.executed_at", "COPY tx.executed_at");
    let refused = Plan::new(
        read_report(&unparsed).unwrap(),
        &valuation_sources(),
        &[],
        &LimitDefaults::default(),
    )
    .unwrap_err();
    assert!(
        refused[0].ends_with("; the combine step is one query that only reads"),
        "{refused:?}"
    );
}

#[test]
fn orders_parameters_across_the_sources_and_the_combine_step() {
    let text = VALUATION
        .replace(
            "params = { since = \"timestamp\", until = \"timestamp\" }",
            "params = { wallet = \"text\", until = \"timestamp\", since = \"timestamp\", \
             note = \"text\" }",
        )
        .replace(
            "ORDER BY tx.executed_at, tx.id",
            "WHERE tx.wallet = :wallet OR :wallet = ''\nORDER BY tx.executed_at, tx.id",
        );
    let plan = Plan::new(
        read_report(&text).unwrap(),
        &valuation_sources(),
        &[],
        &LimitDefaults::default(),
    )
    .unwrap();
    assert_eq!(
        plan.params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>(),
        ["since", "until", "wallet", "note"]
    );

    let undeclared = VALUATION.replace(
        "ORDER BY tx.executed_at, tx.id",
        "WHERE tx.asset = :asset\nORDER BY tx.executed_at, tx.id",
    );
    let problems = Plan::new(
        read_report(&undeclared).unwrap(),
        &valuation_sources(),
        &[],
        &LimitDefaults::default(),
    )
    .unwrap_err();
    assert_eq!(
        problems,
        [
            "[combine]: the query uses :asset, which [report] params doesn't declare; add asset = \
          \"text\" (or its type) to params"
        ]
    );
}

#[test]
fn says_whether_the_file_or_settings_set_a_limit() {
    let plan = Plan::new(
        read_report(TRIAL_BALANCE).unwrap(),
        &[sqlite("ledger")],
        &presets(),
        &LimitDefaults::default(),
    )
    .unwrap();
    let source = Step::Source {
        id: "balances".to_owned(),
    };
    let output = Step::Output {
        number: 1,
        file: "trial-balance-2026-10.xlsx".to_owned(),
    };
    let cases = [
        (&source, "max_rows", Some(LimitSetIn::File)),
        (&source, "max_bytes", Some(LimitSetIn::Settings)),
        (&source, "timeout", Some(LimitSetIn::Settings)),
        (&output, "max_size", Some(LimitSetIn::Settings)),
        (&output, "continue_on_new_sheet", None),
        (&Step::Worker, "timeout", Some(LimitSetIn::Settings)),
    ];
    for (step, key, expected) in cases {
        assert_eq!(plan.limit_set_in(step, key), expected, "{step} {key}");
    }

    let hit = |set_in| LimitHit {
        key: "max_rows".to_owned(),
        value: "100,000 rows".to_owned(),
        raise_in: "the [[source]] \"balances\"".to_owned(),
        set_in,
    };
    let mut failure = RunFailure::at_limit(source.clone(), "it stopped", hit(None));
    failure.place_limit(&plan);
    assert_eq!(failure.limit.as_deref(), Some(&hit(Some(LimitSetIn::File))));
    assert_eq!(
        hit(Some(LimitSetIn::File)).advice(),
        "It stopped at max_rows = 100,000 rows, set in the [[source]] \"balances\" of the \
         report's file. To allow more, raise it there."
    );
    assert!(
        hit(Some(LimitSetIn::Settings)).advice().starts_with(
            "It stopped at max_rows = 100,000 rows, the default in Settings › Reports"
        )
    );
}

/// What each problem of `text` underlines, by its message's start, after planning it against
/// the ledger and the presets.
fn underlined(text: &str) -> Vec<(String, String)> {
    let problems = plan_text(
        text,
        &[sqlite("ledger")],
        &presets(),
        &LimitDefaults::default(),
    )
    .expect_err(text);
    let locator = Locator::new(text);
    problems
        .into_iter()
        .map(|problem| {
            let range = locator.locate(&problem.place);
            (problem.message, text[range].to_owned())
        })
        .collect()
}

#[test]
fn places_each_problem_where_it_is() {
    let base = |replace: &str, with: &str| TRIAL_BALANCE.replacen(replace, with, 1);
    let cases = [
        (
            base("sheet_name = \"Trial balance\"", "sheet = 2"),
            vec![("output 1: \"sheet\" is not part of an output", "sheet")],
        ),
        (
            base("max_rows = 100000", "max_rows = \"many\""),
            vec![("source \"balances\": max_rows must be", "max_rows")],
        ),
        (
            base("data_source = \"ledger\"", "data_source = \"ledgr\""),
            vec![(
                "source \"balances\": there is no data source",
                "data_source",
            )],
        ),
        (
            base("[[output]]\npreset", "[[output]]\nsheet = 1\npreset"),
            vec![(
                "output 2: \"sheet\" can't be set on an output with a preset",
                "sheet",
            )],
        ),
        (
            base(
                "params = { since = \"date\", until = \"date\" }",
                "params = { since = \"date\" }",
            ),
            vec![("source \"balances\": the query uses :until", "query")],
        ),
        (
            base("description = ", "descripton = "),
            vec![(
                "[report]: \"descripton\" is not part of [report]",
                "descripton",
            )],
        ),
        (
            base(
                "[[output]]\nformat = \"xlsx\"",
                "[[output]]\nformat = \"xlsx\"\nformat2 = 1",
            ),
            vec![("output 1: \"format2\" is not part of an output", "format2")],
        ),
        (
            format!("colour = \"red\"\n{TRIAL_BALANCE}"),
            vec![("\"colour\" is not part of a report file", "colour")],
        ),
    ];
    for (text, expected) in cases {
        let found = underlined(&text);
        assert_eq!(found.len(), expected.len(), "{found:#?}");
        for ((message, place), (start, wanted)) in found.iter().zip(&expected) {
            assert!(message.starts_with(start), "{message:?} for {start:?}");
            assert_eq!(place, wanted, "{message}");
        }
    }
}

#[test]
fn names_a_missing_data_source_beside_the_problems_that_stop_the_read() {
    let text = TRIAL_BALANCE
        .replacen("data_source = \"ledger\"", "data_source = \"ledgr\"", 1)
        .replacen("sheet_name = \"Trial balance\"", "sheet = 2", 1);
    let found = underlined(&text);
    let places: Vec<&str> = found.iter().map(|(_, place)| place.as_str()).collect();
    assert_eq!(places, ["sheet", "data_source"], "{found:#?}");
    assert!(found[1].0.contains("there is no data source \"ledgr\""));
}

#[test]
fn places_every_syntax_error_and_keeps_the_messages_callers_print() {
    let text = format!("{TRIAL_BALANCE}\noops = \nmore = ]\n");
    let found = underlined(&text);
    assert_eq!(found.len(), 2, "{found:#?}");
    assert!(
        found
            .iter()
            .all(|(message, _)| message.starts_with("line "))
    );
    assert_eq!(
        read_report(&text).expect_err("broken"),
        found
            .into_iter()
            .map(|(message, _)| message)
            .collect::<Vec<_>>()
    );
}

/// RS3d: DuckDB writes a Parquet output from the combine step, with the columns it gives them:
/// a report without one, or a Parquet output naming its columns, is refused.
#[test]
fn plans_parquet_only_from_a_combine_step_and_with_its_columns() {
    let planned = |text: &str| {
        Plan::new(
            read_report(text).unwrap(),
            &[sqlite("ledger")],
            &[],
            &LimitDefaults::default(),
        )
    };
    let alone = one_source(
        "",
        "SELECT 1",
        "format = \"parquet\"\nfile = \"out.parquet\"",
    );
    let problems = planned(&alone).unwrap_err();
    assert!(
        problems[0].starts_with(
            "output 1: a Parquet file is written by the combine step's DuckDB; add a [combine]"
        ),
        "{problems:?}"
    );
    let combined = |output: &str| {
        one_source("", "SELECT 1 AS n", output).replace(
            "[[output]]",
            "[combine]\nquery = \"SELECT * FROM s\"\n[[output]]",
        )
    };
    let plan = planned(&combined(
        "format = \"parquet\"\ncompression = \"snappy\"\nfile = \"out.parquet\"",
    ))
    .unwrap();
    assert_eq!(
        plan.outputs[0].format.written_by_duckdb(),
        Some(ParquetOptions {
            compression: dbrover_export::ParquetCompression::Snappy
        })
    );
    let named = planned(&combined(
        "format = \"parquet\"\nfile = \"out.parquet\"\ncolumns = [{ name = \"N\", from = \"n\" }]",
    ))
    .unwrap_err();
    assert_eq!(
        named,
        [
            "output 1: a Parquet output writes the combine step's columns as DuckDB types them; \
          name and cast them in the combine query instead"
        ]
    );
}
