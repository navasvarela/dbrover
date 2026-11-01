//! Charts from results (W12), driven through the whole app against the PostgreSQL test server.

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

/// W12's exit: totals by month chart as bars and as lines. A month written as text opens as
/// bars; as a timestamp, the chart stays open for the next run and starts as a line over time.
#[test]
fn charts_totals_by_month_as_bars_and_as_lines() {
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
