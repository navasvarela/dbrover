//! Charts from results (W12), driven through the whole app against the PostgreSQL test server,
//! and a chart in a query's text (CH1) against the SQLite shop of the example reports.

use dbrover_chart::{Drawing, Mark};

use super::plans::{console_with, start_on};
use super::*;

/// Orders over three months, in a schema of the test's own.
const ORDERS: &str = "
    CREATE TABLE {schema}.orders (id int PRIMARY KEY, day date NOT NULL, total numeric(10, 2));
    INSERT INTO {schema}.orders VALUES
        (1, '2026-01-05', 120.50), (2, '2026-01-20', 30.00),
        (3, '2026-02-11', 80.00),
        (4, '2026-03-02', 150.25), (5, '2026-03-30', 49.75)";

/// The test's own schema on the test server, dropped when the test ends.
struct Orders<'a> {
    runtime: &'a tokio::runtime::Runtime,
    admin: Box<dyn dbrover_core::Connection>,
    schema: String,
}

impl<'a> Orders<'a> {
    fn create(server: &PostgresTestServer, runtime: &'a tokio::runtime::Runtime) -> Self {
        let mut admin = server.open(runtime, &server.database);
        let schema = format!("dbrover_w12_{}", std::process::id());
        first_value(
            runtime,
            &mut admin,
            &format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
        );
        first_value(runtime, &mut admin, &format!("CREATE SCHEMA {schema}"));
        for statement in ORDERS.replace("{schema}", &schema).split(';') {
            first_value(runtime, &mut admin, statement);
        }
        Self {
            runtime,
            admin,
            schema,
        }
    }

    /// Totals by month, the month written as text or as the timestamp it starts at.
    fn by_month(&self, month: &str) -> String {
        format!(
            "SELECT {month} AS month, sum(total) AS total FROM {}.orders GROUP BY 1 ORDER BY 1",
            self.schema
        )
    }
}

impl Drop for Orders<'_> {
    fn drop(&mut self) {
        first_value(
            self.runtime,
            &mut self.admin,
            &format!("DROP SCHEMA IF EXISTS {} CASCADE", self.schema),
        );
    }
}

fn shown_chart(harness: &Harness<'_, DbRoverApp>) -> Option<Drawing> {
    harness
        .state()
        .open_consoles()
        .find_map(|console| console.shown_chart().cloned())
}

/// The bars and lines drawn, and the labels written.
fn marks(drawing: &Drawing) -> (usize, usize, Vec<String>) {
    let bars = drawing
        .marks
        .iter()
        .filter(|mark| matches!(mark, Mark::Rect { rect, .. } if rect.min.y >= drawing.plot.min.y))
        .count();
    let lines = drawing
        .marks
        .iter()
        .filter(|mark| matches!(mark, Mark::Polyline { .. }))
        .count();
    let texts = drawing
        .marks
        .iter()
        .filter_map(|mark| match mark {
            Mark::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    (bars, lines, texts)
}

/// W12's exit: totals by month chart as bars, exported as SVG (and PNG), and as lines. A month
/// written as text opens as bars; as a timestamp, the chart stays open for the next run and
/// starts as a line over time.
#[test]
fn charts_totals_by_month_as_bars_and_lines_and_exports_an_svg() {
    let Some(server) = PostgresTestServer::from_env() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let orders = Orders::create(&server, &runtime);
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_on(&server, dir.path(), dbrover_core::SafetyLevel::Normal);
    console_with(
        &mut harness,
        "pg",
        &orders.by_month("to_char(day, 'YYYY-MM')"),
    );
    harness.get_by_label("Run").click();
    step_until_visible(&mut harness, "2026-02");

    harness.get_by_label("Chart").click();
    step_until(&mut harness, "the chart to be drawn", |harness| {
        shown_chart(harness).is_some()
    });
    harness.get_by_value("X: month");
    harness.get_by_label("Y: total");
    let (bars, lines, texts) = marks(&shown_chart(&harness).unwrap());
    assert_eq!((bars, lines), (3, 0));
    for label in ["2026-01", "2026-02", "2026-03", "month", "total"] {
        assert!(texts.contains(&label.to_owned()), "{label} in {texts:?}");
    }

    let pictures = dir.path().join("pictures");
    std::fs::create_dir_all(&pictures).unwrap();
    let answer = pictures.clone();
    harness.state_mut().pick_picture =
        Box::new(move |name, format| Some(answer.join(format!("{name}.{}", format.extension()))));
    for (item, file) in [
        ("SVG…", "total by month.svg"),
        ("PNG at 2×…", "total by month.png"),
    ] {
        harness.get_by_label("Export").click();
        harness.run_steps(2);
        harness.get_by_label(item).click();
        step_until_visible(
            &mut harness,
            &format!("Exported to {}", pictures.join(file).display()),
        );
    }
    let svg = std::fs::read_to_string(pictures.join("total by month.svg")).unwrap();
    let bars = svg
        .lines()
        .filter(|line| line.starts_with("<rect x=") && line.contains(" rx="))
        .count();
    assert_eq!(bars, 3, "{svg}");
    for label in ["2026-01", "2026-02", "2026-03", "month", "total"] {
        assert!(
            svg.contains(&format!(">{label}</text>")),
            "{label} in the SVG"
        );
    }
    let png = std::fs::read(pictures.join("total by month.png")).unwrap();
    assert!(png.starts_with(b"\x89PNG"));

    harness.get_by_label("Lines").click();
    harness.run_steps(2);
    let (bars, lines, _) = marks(&shown_chart(&harness).unwrap());
    assert_eq!((bars, lines), (0, 1));

    let by_timestamp = orders.by_month("date_trunc('month', day)");
    harness.get_by_role(Role::MultilineTextInput).focus();
    harness.run();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.key_press(egui::Key::Backspace);
    harness
        .get_by_role(Role::MultilineTextInput)
        .type_text(&by_timestamp);
    step_until(&mut harness, "the second query to be typed", |harness| {
        harness
            .get_by_role(Role::MultilineTextInput)
            .value()
            .is_some_and(|text| text == by_timestamp)
    });
    harness.get_by_label("Run").click();
    step_until(&mut harness, "the second result's chart", |harness| {
        shown_chart(harness).is_some_and(|drawing| {
            marks(&drawing)
                .2
                .iter()
                .any(|text| text.contains("Jan") && text != "2026-01")
        })
    });
    let (bars, lines, texts) = marks(&shown_chart(&harness).unwrap());
    assert_eq!((bars, lines), (0, 1), "a line over time: {texts:?}");
    // Dates along the bottom, at whatever step fits the width: `1 Jan`, or `Jan 2026`.
    assert!(
        !texts.iter().any(|text| text.starts_with("2026-")),
        "{texts:?}"
    );
}

/// Net sales by month on the example reports' shop, as bars, with a comment inside the block.
const SHOP_BY_MONTH: &str = "-- Net sales by month.
-- [chart]
-- # Bars read best for three months.
-- kind = \"bars\"
-- x = \"month\"
-- y = \"net\"
SELECT strftime('%Y-%m', ordered_on) AS month, SUM(amount) AS net
FROM order_lines GROUP BY month ORDER BY month;";

/// The shop's tables and rows, from the file the example reports are made from.
const SHOP: &str = include_str!("../../../../../examples/reports/shop.sql");

fn replace_text(harness: &mut Harness<'_, DbRoverApp>, text: &str) {
    harness.get_by_role(Role::MultilineTextInput).focus();
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.key_press(egui::Key::Backspace);
    harness
        .get_by_role(Role::MultilineTextInput)
        .type_text(text);
    step_until(harness, "the text to be typed", |harness| {
        editor_text(harness) == text
    });
}

/// CH1's exit: a statement with a block of bars by month opens as that chart; switching to
/// lines and Save to query rewrites only `kind`, keeping the comment inside the block; undo
/// restores the block; and a misspelt column shows the problem with the result's columns.
#[test]
fn a_chart_block_opens_its_chart_and_save_to_query_writes_it_back() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), SHOP);
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    console_with(&mut harness, "shop", SHOP_BY_MONTH);
    harness.get_by_label("Run").click();
    step_until(&mut harness, "the block's chart", |harness| {
        shown_chart(harness).is_some()
    });
    let (bars, lines, texts) = marks(&shown_chart(&harness).unwrap());
    assert_eq!((bars, lines), (3, 0), "{texts:?}");
    for label in ["2026-09", "2026-10", "2026-11", "month", "net"] {
        assert!(texts.contains(&label.to_owned()), "{label} in {texts:?}");
    }

    harness.get_by_label("Lines").click();
    harness.run_steps(2);
    harness.get_by_label("Not in the query");
    harness.get_by_label("Save to query").click();
    let saved = SHOP_BY_MONTH.replace("kind = \"bars\"", "kind = \"lines\"");
    step_until(&mut harness, "the block to be rewritten", |harness| {
        editor_text(harness) == saved
    });
    assert!(harness.query_by_label("Not in the query").is_none());

    harness.get_by_role(Role::MultilineTextInput).focus();
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    step_until(&mut harness, "undo to restore the block", |harness| {
        editor_text(harness) == SHOP_BY_MONTH
    });

    replace_text(
        &mut harness,
        &SHOP_BY_MONTH.replace("y = \"net\"", "y = \"nett\""),
    );
    harness.get_by_label("Run").click();
    step_until_visible(
        &mut harness,
        "chart: `y = \"nett\"` isn't a column of this result: month, net",
    );
}
