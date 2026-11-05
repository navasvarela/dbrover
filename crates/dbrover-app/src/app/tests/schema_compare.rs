//! Schema compare (W13) through the whole app, on the PostgreSQL test server: **Compare
//! schema…** on a schema, the right side picked, the differences listed and the script opened
//! in a console. Running the script and comparing again is the engine's exit test
//! (`dbrover-engine/tests/ddl/schema_compare.rs`).

use super::*;

/// A database of the test's own, with two schemas that differ by a column and an index,
/// dropped when the test ends.
struct CompareDatabase<'a> {
    runtime: &'a tokio::runtime::Runtime,
    admin: Box<dyn dbrover_core::Connection>,
    name: String,
}

impl<'a> CompareDatabase<'a> {
    fn create(server: &PostgresTestServer, runtime: &'a tokio::runtime::Runtime) -> Self {
        let name = format!("dbrover_w13_{}", std::process::id());
        let mut admin = server.open(runtime, &server.database);
        first_value(
            runtime,
            &mut admin,
            &format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
        );
        first_value(runtime, &mut admin, &format!("CREATE DATABASE {name}"));
        let mut shop = server.open(runtime, &name);
        for schema in ["dev", "prod"] {
            first_value(runtime, &mut shop, &format!("CREATE SCHEMA {schema}"));
            first_value(
                runtime,
                &mut shop,
                &format!(
                    "CREATE TABLE {schema}.orders (id serial PRIMARY KEY, total numeric(12,2))"
                ),
            );
        }
        first_value(
            runtime,
            &mut shop,
            "ALTER TABLE prod.orders ADD COLUMN note text",
        );
        first_value(
            runtime,
            &mut shop,
            "CREATE INDEX orders_total_idx ON prod.orders (total)",
        );
        let _ = runtime.block_on(shop.close());
        Self {
            runtime,
            admin,
            name,
        }
    }
}

impl Drop for CompareDatabase<'_> {
    fn drop(&mut self) {
        let drop = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name);
        first_value(self.runtime, &mut self.admin, &drop);
    }
}

#[test]
fn compares_two_postgres_schemas_and_opens_the_script() {
    let Some(server) = PostgresTestServer::from_env() else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let database = CompareDatabase::create(&server, &runtime);
    let dir = tempfile::tempdir().unwrap();
    let source = server.data_source(&database.name, "pg");
    DataSourceFile::new(dir.path().join("data_sources.toml"))
        .save(std::slice::from_ref(&source))
        .unwrap();
    let (_layer, feed) = log_console::pipeline();
    let mut harness = crate::test_support::start_app(
        harness_at_default_size(),
        dir.path(),
        feed,
        server.secrets(&source),
    );
    for (expand, appears) in [
        ("Expand pg", format!("Expand {}", database.name)),
        (
            &*format!("Expand {}", database.name),
            "Expand dev".to_owned(),
        ),
    ] {
        step_until_visible(&mut harness, expand);
        harness.get_by_label(expand).click();
        step_until_visible(&mut harness, &appears);
    }

    node_menu(&mut harness, "dev", "Compare schema\u{2026}");
    step_until_visible(&mut harness, "Compare");
    harness
        .get_by(|node| {
            node.role() == Role::ComboBox && node.value().as_deref() == Some("Pick a data source")
        })
        .click();
    harness.run_steps(2);
    harness.get_all_by_label("pg").last().unwrap().click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, "Right")
        .focus();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, "Right")
        .type_text("prod");
    harness.run_steps(2);
    harness.get_by_label("Compare").click();
    step_until_visible(&mut harness, "2 differences");
    harness.get_by_label("~ orders");
    harness.get_by_label("+ orders_total_idx");

    harness.get_by_label("Open script in console").click();
    step_until(&mut harness, "a console with the script", |harness| {
        harness.state().consoles.open_consoles().any(|console| {
            console
                .text()
                .contains("ALTER TABLE dev.orders ADD COLUMN note text;")
        })
    });
}
