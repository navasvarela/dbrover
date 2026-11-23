//! RS9's exit test (`docs/reports-at-scale.md`, "RS9's exit test"): a report built on another
//! report's result, run on the SQLite shop of `examples/reports/shop.sql`, which needs no test
//! server.
//!
//! The design wrote this as the valuation example and gains by wallet; `examples/reports/`
//! never gained a valuation — it needs PostgreSQL and DynamoDB — so the shop's
//! `sales-by-category` stands in and `net-by-category` is the report built on it.

// Helpers panic on failure like assertions do; clippy's test exemption covers only `#[test]`
// functions themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{MemorySecrets, sqlite_data_source};
use dbrover_engine::{PasswordCommands, ProcessRunner, SourceAccess, Tunnels};
use dbrover_report::{
    LimitDefaults, NamedReport, Plan, ResultCache, RunFolders, plan_text, read_report, run,
};
use dbrover_store::TrustedCommands;

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/reports")
}

/// The data source `shop`, a SQLite file in `folder` made by `shop.sql`.
fn shop(folder: &Path) -> dbrover_core::DataSource {
    let path = folder.join("shop.db");
    let script = std::fs::read_to_string(examples().join("shop.sql")).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    dbrover_core::DataSource {
        name: "shop".to_owned(),
        ..sqlite_data_source(path)
    }
}

/// Every example report, as the reports folder hands them to the planner.
fn folder_reports() -> Vec<NamedReport> {
    std::fs::read_dir(examples())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|name| name == "toml"))
        .filter_map(|path| {
            Some(NamedReport {
                name: path.file_stem()?.to_str()?.to_owned(),
                text: std::fs::read_to_string(&path).ok()?,
            })
        })
        .collect()
}

const OCTOBER: (&str, &str) = ("2026-10-01", "2026-11-01");

/// Runs the example `name` from `since` to `until` on `shop`, into `out`, keeping results in
/// `cache` when one is given.
async fn run_example(
    name: &str,
    data_source: &dbrover_core::DataSource,
    out: &Path,
    cache: Option<&Path>,
) {
    let text = std::fs::read_to_string(examples().join(format!("{name}.toml"))).unwrap();
    let plan = Plan::new(
        read_report(&text).unwrap(),
        std::slice::from_ref(data_source),
        &[],
        &folder_reports(),
        &LimitDefaults::default(),
    )
    .unwrap();
    let values = plan
        .values(
            &[("since", OCTOBER.0), ("until", OCTOBER.1)]
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect::<BTreeMap<String, String>>(),
        )
        .unwrap();
    let tunnels = Tunnels::new(out.join("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let secrets: Arc<dyn dbrover_store::SecretStore> = Arc::new(MemorySecrets::default());
    let access = SourceAccess {
        secrets: &secrets,
        tunnels: &tunnels,
        commands: &commands,
    };
    let staging = out.join(".run");
    let work = staging.with_extension("work");
    let folders = RunFolders {
        folder: out,
        staging: &staging,
        work: &work,
        cache: cache.map(|folder| ResultCache {
            folder,
            takes_kept: true,
        }),
    };
    run(&plan, &values, &access, folders, &mut |_| {})
        .await
        .unwrap_or_else(|failure| panic!("{name}: {failure}"));
}

/// A CSV's rows, each column by its header.
fn rows(file: &Path) -> Vec<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(file).unwrap();
    let mut lines = text.lines();
    let headers: Vec<String> = lines
        .next()
        .expect("a header line")
        .split(',')
        .map(str::to_owned)
        .collect();
    lines
        .map(|line| {
            headers
                .iter()
                .cloned()
                .zip(line.split(',').map(str::to_owned))
                .collect()
        })
        .collect()
}

/// RS9's exit: a report built on another report's result writes numbers that equal a sum over
/// that report's own CSV file.
#[tokio::test]
async fn a_report_built_on_another_equals_a_sum_over_its_csv() {
    let folder = tempfile::tempdir().unwrap();
    let out = folder.path();
    let shop = shop(out);

    // The report on its own, which writes the file the numbers are checked against.
    run_example("sales-by-category", &shop, out, None).await;
    // The report built on it, whose only source is that report's result.
    run_example("net-by-category", &shop, out, None).await;

    // Each category's subtotal, as the inner report's own file states it.
    let inner = rows(&out.join("sales-by-category-2026-10.csv"));
    let subtotals: BTreeMap<String, String> = inner
        .iter()
        .filter_map(|row| {
            let label = row.get("Category")?;
            let category = label.strip_prefix("Total ")?;
            Some((category.to_owned(), row.get("Net")?.clone()))
        })
        .collect();
    assert_eq!(
        subtotals.keys().collect::<Vec<_>>(),
        ["accessories", "bikes", "clothing", "parts"],
        "the inner report's own subtotals"
    );

    let composed = rows(&out.join("net-by-category-2026-10.csv"));
    let written: BTreeMap<String, String> = composed
        .iter()
        .filter_map(|row| Some((row.get("Category")?.clone(), row.get("Net")?.clone())))
        .collect();
    assert_eq!(
        written, subtotals,
        "each category's net, added up from the rows of the report's own result"
    );
    // Its total matches the inner report's grand total, to the cent.
    let total: f64 = written
        .values()
        .map(|net| net.parse::<f64>().expect("a number"))
        .sum();
    let grand: f64 = inner
        .iter()
        .find(|row| row.get("Category").is_some_and(|label| label == "Total"))
        .and_then(|row| row.get("Net")?.parse::<f64>().ok())
        .expect("the grand total");
    assert!(
        (total - grand).abs() < 0.005,
        "{total} isn't the inner report's {grand}"
    );
}

/// RS9's exit: two reports that read each other are a problem in both files, each naming both.
#[test]
fn a_cycle_of_two_reports_is_a_problem_in_both_files() {
    let folder = tempfile::tempdir().unwrap();
    let shop = sqlite_data_source(folder.path().join("shop.db"));
    let shop = dbrover_core::DataSource {
        name: "shop".to_owned(),
        ..shop
    };
    let one = |reads: &str| {
        format!(
            "[report]\nname = \"One\"\nparams = {{ since = \"date\" }}\n\n[[source]]\n\
             id = \"other\"\nreport = \"{reads}\"\nparams = {{ since = \":since\" }}\n\n\
             [combine]\nquery = \"SELECT * FROM other\"\n\n[[output]]\nformat = \"csv\"\n\
             file = \"one.csv\"\n"
        )
    };
    let first = one("second");
    let second = one("first");
    let reports = vec![
        NamedReport {
            name: "first".to_owned(),
            text: first.clone(),
        },
        NamedReport {
            name: "second".to_owned(),
            text: second.clone(),
        },
    ];
    for (name, text) in [("first", &first), ("second", &second)] {
        let problems = plan_text(
            text,
            std::slice::from_ref(&shop),
            &[],
            &reports,
            &LimitDefaults::default(),
        )
        .expect_err(name);
        let said: Vec<String> = problems
            .iter()
            .map(|problem| problem.message.clone())
            .collect();
        assert!(
            said.iter().any(|problem| problem.contains("in a circle")),
            "{name}: {said:?}"
        );
        assert!(
            said.iter()
                .any(|problem| problem.contains("first") && problem.contains("second")),
            "{name}: {said:?}"
        );
    }
}

/// RS9f, over the examples: a second run inside the age takes the result the first kept, and
/// writes the same file without reading the shop again.
#[tokio::test]
async fn a_second_run_inside_the_age_takes_the_kept_result() {
    let folder = tempfile::tempdir().unwrap();
    let cache = folder.path().join("report-cache");
    let shop = shop(folder.path());
    let reusing = std::fs::read_to_string(examples().join("net-by-category.toml"))
        .unwrap()
        .replace(
            "report = \"sales-by-category\"",
            "report = \"sales-by-category\"\nreuse = \"24h\"",
        );
    let reports = folder_reports();
    let plan = Plan::new(
        read_report(&reusing).unwrap(),
        std::slice::from_ref(&shop),
        &[],
        &reports,
        &LimitDefaults::default(),
    )
    .unwrap();
    let values = plan
        .values(
            &[("since", OCTOBER.0), ("until", OCTOBER.1)]
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect::<BTreeMap<String, String>>(),
        )
        .unwrap();
    let tunnels = Tunnels::new(folder.path().join("known_hosts"));
    let commands = PasswordCommands::new(TrustedCommands::default(), Arc::new(ProcessRunner));
    let secrets: Arc<dyn dbrover_store::SecretStore> = Arc::new(MemorySecrets::default());
    let access = SourceAccess {
        secrets: &secrets,
        tunnels: &tunnels,
        commands: &commands,
    };
    let mut written = Vec::new();
    for run_number in 0..2 {
        let out = folder.path().join(format!("run-{run_number}"));
        std::fs::create_dir_all(&out).unwrap();
        let staging = out.join(".run");
        let work = staging.with_extension("work");
        let folders = RunFolders {
            folder: &out,
            staging: &staging,
            work: &work,
            cache: Some(ResultCache {
                folder: &cache,
                takes_kept: true,
            }),
        };
        run(&plan, &values, &access, folders, &mut |_| {})
            .await
            .unwrap_or_else(|failure| panic!("run {run_number}: {failure}"));
        written.push(std::fs::read_to_string(out.join("net-by-category-2026-10.csv")).unwrap());
    }
    assert_eq!(
        written[0], written[1],
        "the kept result writes the same file"
    );
    let kept = std::fs::read_dir(&cache).unwrap().count();
    assert_eq!(
        kept, 1,
        "one result, keyed by the report and its parameters"
    );
}
