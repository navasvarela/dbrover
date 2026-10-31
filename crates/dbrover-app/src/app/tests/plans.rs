//! The visual explain plan (W10), driven through the whole app against the PostgreSQL test
//! server.

use super::*;

/// Tables of customers and their orders in a schema of the test's own, analysed so the plan's
/// estimates are the same on every run: twenty orders a customer, so reading the orders is the
/// costliest step of their join.
const SHOP: &str = "
    CREATE TABLE {schema}.customers (id int PRIMARY KEY, name text NOT NULL, country text);
    CREATE TABLE {schema}.orders (
        id int PRIMARY KEY,
        customer_id int REFERENCES {schema}.customers (id),
        total numeric(10, 2)
    );
    INSERT INTO {schema}.customers
        SELECT g, 'customer ' || g, CASE WHEN g % 3 = 0 THEN 'ES' ELSE 'FR' END
        FROM generate_series(1, 1000) g;
    INSERT INTO {schema}.orders
        SELECT g, 1 + g % 1000, (g % 97) * 1.5 FROM generate_series(1, 20000) g;
    ANALYZE {schema}.customers;
    ANALYZE {schema}.orders";

/// The test's own schema on the test server, dropped when the test ends.
struct Shop<'a> {
    runtime: &'a tokio::runtime::Runtime,
    admin: Box<dyn dbrover_core::Connection>,
    schema: String,
}

impl<'a> Shop<'a> {
    /// `test` tells apart the schemas of tests running at once.
    fn create(
        server: &PostgresTestServer,
        runtime: &'a tokio::runtime::Runtime,
        test: &str,
    ) -> Self {
        let mut admin = server.open(runtime, &server.database);
        let schema = format!("dbrover_w10_{test}_{}", std::process::id());
        first_value(
            runtime,
            &mut admin,
            &format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
        );
        first_value(runtime, &mut admin, &format!("CREATE SCHEMA {schema}"));
        for statement in SHOP.replace("{schema}", &schema).split(';') {
            first_value(runtime, &mut admin, statement);
        }
        Self {
            runtime,
            admin,
            schema,
        }
    }

    fn join(&self) -> String {
        format!(
            "SELECT c.name, sum(o.total) FROM {schema}.orders o \
             JOIN {schema}.customers c ON c.id = o.customer_id GROUP BY c.name",
            schema = self.schema
        )
    }
}

impl Drop for Shop<'_> {
    fn drop(&mut self) {
        first_value(
            self.runtime,
            &mut self.admin,
            &format!("DROP SCHEMA IF EXISTS {} CASCADE", self.schema),
        );
    }
}

/// Opens a console on the data source named `name` and types `sql` into it.
fn console_with(harness: &mut Harness<'_, DbRoverApp>, name: &str, sql: &str) {
    step_until_visible(harness, &format!("Expand {name}"));
    harness.get_by_label(name).click_secondary();
    harness.run_steps(2);
    harness
        .get_all_by_label("New query console")
        .next()
        .unwrap()
        .click();
    step_until(harness, "the console's editor appearing", |harness| {
        harness.query_by_role(Role::MultilineTextInput).is_some()
    });
    let editor = harness.get_by_role(Role::MultilineTextInput);
    editor.focus();
    editor.type_text(sql);
    step_until(harness, "the query to be typed", |harness| {
        harness
            .get_by_role(Role::MultilineTextInput)
            .value()
            .is_some_and(|text| text == sql)
    });
}

fn shown_plan(harness: &Harness<'_, DbRoverApp>) -> Option<dbrover_plan::Plan> {
    harness
        .state()
        .open_consoles()
        .find_map(|console| console.shown_plan().cloned())
}

/// The app, with one data source named "pg" on the test server at `safety`.
fn start_on(
    server: &PostgresTestServer,
    dir: &Path,
    safety: dbrover_core::SafetyLevel,
) -> Harness<'static, DbRoverApp> {
    let source = dbrover_core::DataSource {
        safety,
        ..server.data_source(&server.database, "pg")
    };
    DataSourceFile::new(dir.join("data_sources.toml"))
        .save(std::slice::from_ref(&source))
        .unwrap();
    let (_layer, feed) = log_console::pipeline();
    crate::test_support::start_app(
        harness_at_default_size(),
        dir,
        feed,
        server.secrets(&source),
    )
}

fn explain_analyse(harness: &mut Harness<'_, DbRoverApp>) {
    harness.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::ALT | egui::Modifiers::SHIFT,
        egui::Key::E,
    );
}

/// W10's exit: ⌥⌘E on a join of orders and customers shows the plan as a tree, the costliest
/// step highlighted, and as text one click away, both copied; Explain analyse adds the actual
/// rows.
#[test]
fn explains_a_join_as_a_tree_with_its_costliest_step_highlighted() {
    let Some(server) = PostgresTestServer::from_env() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let shop = Shop::create(&server, &runtime, "exit");
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_on(&server, dir.path(), dbrover_core::SafetyLevel::Normal);
    console_with(&mut harness, "pg", &shop.join());

    harness.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::ALT,
        egui::Key::E,
    );
    step_until_visible(&mut harness, "Costliest");
    // The table sizes its columns on its first frame, and its rows move on the next: a click
    // aimed at the first frame's rows lands a row off.
    harness.run_steps(2);
    let plan = shown_plan(&harness).unwrap();
    let outline = plan.outline();
    let costliest: Vec<String> = outline
        .iter()
        .filter(|line| line.weight.heat == dbrover_plan::Heat::Costliest)
        .map(|line| line.step.title())
        .collect();
    assert_eq!(costliest, ["Seq Scan on orders o"]);
    harness.get_by_label("Hash Join");
    harness.get_by_label("on orders o");

    harness.get_by_label("Collapse Hash Join").click();
    step_until(&mut harness, "the join's steps to be hidden", |harness| {
        harness.query_by_label("on customers c").is_none()
    });
    harness.get_by_label("Expand Hash Join").click();
    step_until_visible(&mut harness, "on customers c");

    harness.get_by_label("Copy JSON").click();
    step_until(&mut harness, "the plan being copied", |harness| {
        copied(harness).is_some()
    });
    assert!(
        copied(&harness)
            .unwrap()
            .contains("\"Node Type\": \"Hash Join\"")
    );

    harness.get_by_label("Text").click();
    harness.run_steps(2);
    harness.get_by_label("Copy text").click();
    step_until(&mut harness, "the text plan being copied", |harness| {
        copied(harness).is_some_and(|text| text.contains("->  Hash Join"))
    });
    let text = copied(&harness).unwrap();
    assert!(text.contains("->  Seq Scan on orders o  (cost="), "{text}");

    harness.get_by_label("Tree").click();
    harness.run_steps(2);
    harness.get_by_label("Explain analyse").click();
    step_until_visible(&mut harness, "Actual rows");
    let plan = shown_plan(&harness).unwrap();
    assert!(plan.is_analysed());
    let outline = plan.outline();
    let orders = outline
        .iter()
        .find(|line| line.step.title() == "Seq Scan on orders o")
        .and_then(|line| line.step.actual)
        .unwrap();
    assert_eq!((orders.rows, orders.loops), (Some(20000.0), 1.0));
    harness.get_by_label_contains("Analysed plan · ran in");
}

/// On a careful data source Explain analyse asks first, since it runs the statement, even a
/// `SELECT` that running wouldn't ask about; Explain doesn't.
#[test]
fn explain_analyse_asks_first_on_a_careful_data_source() {
    let Some(server) = PostgresTestServer::from_env() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let shop = Shop::create(&server, &runtime, "careful");
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_on(&server, dir.path(), dbrover_core::SafetyLevel::Careful);
    console_with(&mut harness, "pg", &shop.join());

    explain_analyse(&mut harness);
    step_until_visible(&mut harness, "Explain analyse on \u{201c}pg\u{201d}?");
    harness.get_by_label_contains("Explain analyse runs this statement");
    // The modal is measured on its first frame and placed on the next.
    harness.run_steps(2);
    harness.get_by_label("Run anyway").click();
    step_until_visible(&mut harness, "Actual rows");
    assert!(shown_plan(&harness).unwrap().is_analysed());
}
