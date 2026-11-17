//! Charts from results (W12), driven through the whole app against the PostgreSQL test server,
//! and a chart in a query's text (CH1) and its series and stacked bars (CH2) against the SQLite
//! shop of the example reports.

use dbrover_chart::{Drawing, Mark, Rect};

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

/// Net sales by month and category, with the first category picked out in a brand's red.
const SHOP_RED_FIRST: &str = "-- [chart]
-- kind = \"bars\"
-- x = \"month\"
-- y = \"net\"
-- series = \"category\"
-- colours = [\"#e34948\"]
SELECT strftime('%Y-%m', l.ordered_on) AS month, p.category, SUM(l.amount) AS net
FROM order_lines l JOIN products p ON p.id = l.product_id
GROUP BY month, p.category ORDER BY month, p.category;";

/// CH5e's exit: a named colour is that exact hue in the exported file, and the series the spec
/// doesn't name keep the palette's own.
#[test]
fn a_named_colour_holds_in_the_exported_file() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), SHOP);
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    console_with(&mut harness, "shop", SHOP_RED_FIRST);
    harness.get_by_label("Run").click();
    step_until(&mut harness, "the recoloured chart", |harness| {
        shown_chart(harness).is_some()
    });

    let pictures = dir.path().join("pictures");
    std::fs::create_dir_all(&pictures).unwrap();
    let answer = pictures.clone();
    harness.state_mut().pick_picture =
        Box::new(move |name, format| Some(answer.join(format!("{name}.{}", format.extension()))));
    harness.get_by_label("Export").click();
    harness.run_steps(2);
    harness.get_by_label("SVG…").click();
    let file = pictures.join("net by month.svg");
    step_until_visible(&mut harness, &format!("Exported to {}", file.display()));

    let svg = std::fs::read_to_string(&file).unwrap();
    let first = dbrover_chart::SERIES_LIGHT[0];
    assert!(
        svg.contains("fill=\"#e34948\""),
        "the named hue, as given: {svg}"
    );
    assert!(
        !svg.contains(&format!("fill=\"{first}\"")),
        "nothing is left in the palette's first hue, {first}"
    );
    let second = dbrover_chart::SERIES_LIGHT[1];
    assert!(
        svg.contains(&format!("fill=\"{second}\"")),
        "the second series keeps the palette's own {second}: {svg}"
    );
}

/// Every product's net sales against its price, on a log scale: the nets span decades, and the
/// bell, which is never ordered, has none, so a log axis has no place for it.
const SHOP_PRICES: &str = "-- [chart]
-- kind = \"scatter\"
-- x = \"price\"
-- y = \"net\"
-- y_scale = \"log\"
SELECT p.price, COALESCE(SUM(l.amount), 0) AS net
FROM products p LEFT JOIN order_lines l ON l.product_id = p.id
GROUP BY p.id, p.price ORDER BY p.price;";

/// CH5d's exit: a scatter on a log scale draws its decades, and the one zero in the column is a
/// gap the line under the chart names rather than a row that quietly vanishes.
#[test]
fn a_log_scale_draws_decades_and_names_the_zero_it_cannot_place() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), SHOP);
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    console_with(&mut harness, "shop", SHOP_PRICES);
    harness.get_by_label("Run").click();
    step_until(&mut harness, "the log chart", |harness| {
        shown_chart(harness).is_some()
    });

    let drawing = shown_chart(&harness).unwrap();
    let dots = drawing
        .marks
        .iter()
        .filter(|mark| matches!(mark, Mark::Circle { .. }))
        .count();
    assert_eq!(dots, 7, "eight products, and the bell's zero has no place");
    let (_, _, texts) = marks(&drawing);
    let decades: Vec<&String> = texts
        .iter()
        .filter(|text| {
            text.chars().all(|c| c.is_ascii_digit() || c == ',')
                && text.starts_with('1')
                && text
                    .trim_start_matches(['1', ','])
                    .chars()
                    .all(|c| c == '0')
        })
        .collect();
    assert!(
        decades.len() >= 2,
        "the axis is labelled by powers of ten: {texts:?}"
    );
    step_until_visible(
        &mut harness,
        "1 gap in net: zero or negative on a log scale",
    );
}

/// Category shares by month on the shop, as a stacked area: the same `GROUP BY` shape as the
/// stacked bars, drawn as filled bands.
const SHOP_SHARES: &str = "-- [chart]
-- kind = \"area\"
-- x = \"month\"
-- y = \"net\"
-- series = \"category\"
SELECT strftime('%Y-%m', l.ordered_on) AS month, p.category, SUM(l.amount) AS net
FROM order_lines l JOIN products p ON p.id = l.product_id
GROUP BY month, p.category ORDER BY month, p.category;";

/// CH5c's exit: category shares by month draw as a stacked area, a band a category piled on the
/// one below it, and the exported SVG writes each band as one polygon.
#[test]
fn category_shares_by_month_draw_as_a_stacked_area() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), SHOP);
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    console_with(&mut harness, "shop", SHOP_SHARES);
    harness.get_by_label("Run").click();
    step_until(&mut harness, "the area chart", |harness| {
        shown_chart(harness).is_some()
    });

    let drawing = shown_chart(&harness).unwrap();
    let rings: Vec<Vec<dbrover_chart::Point>> = drawing
        .marks
        .iter()
        .filter_map(|mark| match mark {
            Mark::Band { points, .. } => Some(points.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(rings.len(), 4, "a band per category: {rings:?}");
    // A band's ring is its own edge forwards then the edge below it back, so the lower edge of
    // each band is the upper edge of the one before it: the bands are piled, not overlaid.
    let edges: Vec<(Vec<f32>, Vec<f32>)> = rings
        .iter()
        .map(|ring| {
            let half = ring.len() / 2;
            let (edge, below) = ring.split_at(half);
            (
                edge.iter().map(|point| point.y).collect(),
                below.iter().rev().map(|point| point.y).collect(),
            )
        })
        .collect();
    assert_eq!(edges[0].0.len(), 3, "three months: {edges:?}");
    for pair in edges.windows(2) {
        for (above, below) in pair[0].0.iter().zip(&pair[1].1) {
            assert!(
                (above - below).abs() < 0.01,
                "each band sits on the one below: {above} and {below}"
            );
        }
    }
    let (_, _, texts) = marks(&drawing);
    for category in ["accessories", "bikes", "clothing", "parts"] {
        assert!(
            texts.contains(&category.to_owned()),
            "{category} in {texts:?}"
        );
    }
    // An area always stacks, so the box says so and there is nothing to turn off.
    harness.get_by_label("Stacked");

    let pictures = dir.path().join("pictures");
    std::fs::create_dir_all(&pictures).unwrap();
    let answer = pictures.clone();
    harness.state_mut().pick_picture =
        Box::new(move |name, format| Some(answer.join(format!("{name}.{}", format.extension()))));
    harness.get_by_label("Export").click();
    harness.run_steps(2);
    harness.get_by_label("SVG…").click();
    let file = pictures.join("net by month.svg");
    step_until_visible(&mut harness, &format!("Exported to {}", file.display()));
    let svg = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        svg.matches("<polygon ").count(),
        4,
        "one polygon a band: {svg}"
    );
    for category in ["accessories", "bikes", "clothing", "parts"] {
        assert!(
            svg.contains(&format!(">{category}</text>")),
            "{category} in the SVG"
        );
    }
}

/// Net sales by product and category on the shop: names longer than a slot along the bottom
/// holds, which is what horizontal bars are for.
const SHOP_BY_PRODUCT: &str = "-- [chart]
-- kind = \"hbars\"
-- x = \"product\"
-- y = \"net\"
-- sort = \"net desc\"
SELECT p.name || ' (' || p.category || ')' AS product, SUM(l.amount) AS net
FROM order_lines l JOIN products p ON p.id = l.product_id
GROUP BY product ORDER BY product;";

/// CH5b's exit: a chart of long names draws as horizontal bars whose names are readable, where
/// the same chart standing up cuts them to a slot; and the fourth kind in the toolbar is the
/// block's, so switching back to it leaves nothing to save.
#[test]
fn long_names_chart_as_horizontal_bars_whose_names_are_readable() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), SHOP);
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    console_with(&mut harness, "shop", SHOP_BY_PRODUCT);
    harness.get_by_label("Run").click();
    step_until(&mut harness, "the horizontal chart", |harness| {
        shown_chart(harness).is_some()
    });

    let drawing = shown_chart(&harness).unwrap();
    let (count, lines, texts) = marks(&drawing);
    assert_eq!((count, lines), (7, 0), "a bar per product: {texts:?}");
    assert!(
        texts.contains(&"Rain jacket (clothing)".to_owned()),
        "the longest name is whole in the gutter: {texts:?}"
    );
    let bars: Vec<Rect> = drawing
        .marks
        .iter()
        .filter_map(|mark| match mark {
            Mark::Rect { rect, .. } if rect.min.y >= drawing.plot.min.y => Some(*rect),
            _ => None,
        })
        .collect();
    for bar in &bars {
        assert!(
            (bar.min.x - drawing.plot.min.x).abs() < 0.01,
            "every bar grows from the zero line on the left: {bar:?}"
        );
    }
    assert!(
        bars[0].max.y <= bars[1].min.y,
        "the slots run down the plot: {bars:?}"
    );
    assert!(
        bars[0].width() > bars[bars.len() - 1].width(),
        "sorted largest first: {bars:?}"
    );

    harness.get_by_label("Bars").click();
    harness.run_steps(2);
    let upright = shown_chart(&harness).unwrap();
    let (_, _, upright_texts) = marks(&upright);
    assert!(
        upright_texts.contains(&"Rain jacket (clothi…".to_owned()),
        "standing up, a slot cuts the same name: {upright_texts:?}"
    );
    harness.get_by_label("Not in the query");

    harness.get_by_label("Hbars").click();
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Not in the query").is_none(),
        "the block's own kind leaves nothing to save"
    );
}

/// Net sales by month and category on the shop: the usual `GROUP BY` shape, not pivoted.
const SHOP_BY_CATEGORY: &str = "-- [chart]
-- kind = \"bars\"
-- x = \"month\"
-- y = \"net\"
-- series = \"category\"
-- stack = true
SELECT strftime('%Y-%m', l.ordered_on) AS month, p.category, SUM(l.amount) AS net
FROM order_lines l JOIN products p ON p.id = l.product_id
GROUP BY month, p.category ORDER BY month, p.category;";

/// CH2's exit: `GROUP BY month, category` draws as stacked bars per category, with no pivot in
/// the query, on screen and exported as SVG.
#[test]
fn group_by_month_and_category_draws_stacked_bars_and_exports_them() {
    let dir = tempfile::tempdir().unwrap();
    save_shop(dir.path(), SHOP);
    let (_layer, feed) = log_console::pipeline();
    let mut harness = start_app(dir.path(), feed);
    console_with(&mut harness, "shop", SHOP_BY_CATEGORY);
    harness.get_by_label("Run").click();
    step_until(&mut harness, "the stacked chart", |harness| {
        shown_chart(harness).is_some()
    });
    let drawing = shown_chart(&harness).unwrap();
    let pieces: Vec<dbrover_chart::Rect> = drawing
        .marks
        .iter()
        .filter_map(|mark| match mark {
            Mark::Rect {
                rect,
                rounded: false,
                ..
            } => Some(*rect),
            _ => None,
        })
        .collect();
    let lefts: std::collections::BTreeSet<i64> = pieces
        .iter()
        .map(|rect| (rect.min.x * 100.0).round() as i64)
        .collect();
    assert_eq!(lefts.len(), 3, "a bar for each of three months: {pieces:?}");
    assert!(pieces.len() > lefts.len(), "each bar in pieces: {pieces:?}");
    let (_, _, texts) = marks(&drawing);
    for category in ["accessories", "bikes", "clothing", "parts"] {
        assert!(
            texts.contains(&category.to_owned()),
            "{category} in {texts:?}"
        );
    }
    harness.get_by_value("Series: category");

    let pictures = dir.path().join("pictures");
    std::fs::create_dir_all(&pictures).unwrap();
    let answer = pictures.clone();
    harness.state_mut().pick_picture =
        Box::new(move |name, format| Some(answer.join(format!("{name}.{}", format.extension()))));
    harness.get_by_label("Export").click();
    harness.run_steps(2);
    harness.get_by_label("SVG…").click();
    let file = pictures.join("net by month.svg");
    step_until_visible(&mut harness, &format!("Exported to {}", file.display()));
    let svg = std::fs::read_to_string(&file).unwrap();
    let square = svg
        .lines()
        .filter(|line| {
            line.starts_with("<rect x=") && !line.contains(" rx=") && !line.contains("100%")
        })
        .count();
    assert_eq!(square, pieces.len(), "{svg}");
    for category in ["accessories", "bikes", "clothing", "parts"] {
        assert!(
            svg.contains(&format!(">{category}</text>")),
            "{category} in the SVG"
        );
    }
}
