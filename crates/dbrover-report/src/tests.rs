use std::path::PathBuf;
use std::time::Duration;

use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, RedisConfig, SafetyLevel, ServerConfig,
    SqliteConfig, TlsMode,
};
use dbrover_export::{
    ColumnType, CustomFormat, CustomFormatId, CustomFormatKind, DecimalSpec, ExportFormat,
    FieldSeparator, Preset, SheetName,
};

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

fn redis(name: &str) -> DataSource {
    DataSource {
        config: ConnectionConfig::Redis(RedisConfig {
            server: ServerConfig {
                host: "localhost".to_owned(),
                port: 6379.into(),
                database: "0".to_owned(),
                user: String::new(),
                tls: TlsMode::Disable,
                ssh: None,
                password_command: None,
                cloud_sign_in: None,
            },
            separator: ":".to_owned(),
        }),
        ..sqlite(name)
    }
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
    assert_eq!(report.timeout, DEFAULT_RUN_TIMEOUT);
    assert_eq!(
        report
            .params
            .iter()
            .map(|param| (param.name.as_str(), param.param_type))
            .collect::<Vec<_>>(),
        [("since", ColumnType::Date), ("until", ColumnType::Date)]
    );
    assert_eq!(report.source.id, "balances");
    assert_eq!(report.source.data_source, "ledger");
    assert_eq!(
        report.source.limits,
        SourceLimits {
            max_rows: 100_000,
            ..SourceLimits::default()
        }
    );
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
    assert_eq!(report.timeout, Duration::from_secs(3600));
    assert_eq!(
        report.source.limits,
        SourceLimits {
            max_rows: 5_000_000,
            max_bytes: ByteSize(4 << 30),
            timeout: Duration::from_secs(45 * 60),
        }
    );
    assert_eq!(report.outputs[0].limits.max_size, ByteSize(2_000_000_000));
    assert_eq!(report.outputs[0].columns, OutputColumns::All);
    assert_eq!(report.outputs[1].limits, OutputLimits::default());
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
                "{}\n[combine]\nquery = \"SELECT 1\"",
                one_source("", "SELECT 1", output)
            ),
            "[combine] combines several sources in DuckDB, which comes with RP2",
        ),
        (
            format!(
                "{}\n[[source]]\nid = \"t\"\ndata_source = \"x\"\nquery = \"SELECT 2\"",
                one_source("", "SELECT 1", output)
            ),
            "the file has 2 [[source]] tables; a report reads one until RP2",
        ),
        (
            one_source("", "SELECT 1", output)
                .replace("id = \"s\"\n", "id = \"s\"\nattach = true\n"),
            "source \"s\": attaching a DuckDB data source to the combine step comes with RP2",
        ),
        (
            one_source("", "SELECT 1", output)
                .replace("id = \"s\"\n", "id = \"s\"\nlookup = { from = \"t\" }\n"),
            "source \"s\": a lookup reads by the keys an earlier source found, which comes with RP3",
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
            one_source(
                "",
                "SELECT 1",
                "format = \"parquet\"\nfile = \"out.parquet\"",
            ),
            "output 1: a report writes CSV or Excel files in RP1, not Parquet",
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
    )
    .unwrap();
    assert_eq!(plan.source.data_source, ledger);
    assert_eq!(plan.source.dialect, dbrover_sql::Dialect::Sqlite);
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
            report.source.data_source = data_source.to_owned();
            report
        })
        .unwrap()
    };
    let twins = [sqlite("ledger"), sqlite("ledger")];
    let by_id = Plan::new(report(&twins[1].id.to_string()), &twins, &[]).unwrap();
    assert_eq!(by_id.source.data_source.id, twins[1].id);

    let ambiguous = Plan::new(report("ledger"), &twins, &[]).unwrap_err();
    assert!(
        ambiguous[0].starts_with(
            "source \"s\": 2 data sources are named \"ledger\"; name one by its id instead: "
        ),
        "{ambiguous:?}"
    );
    let missing =
        Plan::new(report("books"), &[sqlite("ledger"), sqlite("audit")], &[]).unwrap_err();
    assert_eq!(
        missing,
        ["source \"s\": there is no data source \"books\"; there are audit, ledger"]
    );
    let documents = Plan::new(report("cache"), &[redis("cache")], &[]).unwrap_err();
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
    )
    .unwrap_err();
    assert!(missing[0].starts_with("output 1: exports.toml has no export with the id \"tabs\""));
    let markdown = [preset("notes", ExportFormat::Markdown, "")];
    let not_csv = Plan::new(
        report("preset = \"notes\"\nfile = \"x.md\""),
        &ledger,
        &markdown,
    )
    .unwrap_err();
    assert_eq!(
        not_csv,
        ["output 1: the preset \"notes\": a report writes CSV or Excel files in RP1, not Markdown"]
    );
    let not_excel = Plan::new(
        report("preset = \"semicolon-csv\"\nfile = \"x.csv\"\ncontinue_on_new_sheet = true"),
        &ledger,
        &presets(),
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
    let plan = Plan::new(report, &[sqlite("ledger")], &[]).unwrap();
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
        Plan::new(undeclared, &[sqlite("ledger")], &[]).unwrap_err(),
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
    let plan = Plan::new(report, &[sqlite("ledger")], &[]).unwrap();
    let same: TypedValues = [("a", "x"), ("b", "x")]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    assert_eq!(
        plan.values(&same).unwrap_err(),
        ["output 2: another output already writes \"x.csv\" with these values"]
    );
}
