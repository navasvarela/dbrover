//! A compare tab on its own, fed schemas as the engine describes them.

use dbrover_sql::{ColumnSpec, ObjectName, TableSpec};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;
use dbrover_schema::{Index, Table};

struct Open {
    compares: SchemaCompares,
    id: CompareId,
    candidates: Vec<Candidate>,
    actions: Vec<CompareAction>,
}

fn candidate(name: &str) -> Candidate {
    Candidate {
        id: DataSourceId::generate(),
        name: name.to_owned(),
        dialect: Dialect::Postgres,
    }
}

fn harness(candidates: Vec<Candidate>) -> Harness<'static, Open> {
    let mut compares = SchemaCompares::default();
    let id = compares.open(LiveSchema {
        data_source: candidates[0].id,
        schema: "staging".to_owned(),
    });
    let open = Open {
        compares,
        id,
        candidates,
        actions: Vec::new(),
    };
    let mut fonts_installed = false;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 700.0))
        .build_ui_state(
            move |ui, open: &mut Open| {
                if !fonts_installed {
                    crate::fonts::install(ui.ctx());
                    fonts_installed = true;
                    return;
                }
                let action = open.compares.show(ui, open.id, &open.candidates);
                open.actions.extend(action);
            },
            open,
        );
    harness.run_steps(3);
    harness
}

fn orders(columns: &[(&str, &str)]) -> Table {
    Table {
        spec: TableSpec {
            name: ObjectName::new(None, "orders"),
            columns: columns
                .iter()
                .map(|(name, data_type)| ColumnSpec {
                    name: (*name).to_owned(),
                    data_type: (*data_type).to_owned(),
                    nullable: *name != "id",
                    default: None,
                    identity: None,
                })
                .collect(),
            primary_key: vec!["id".to_owned()],
            unique_keys: vec![],
            foreign_keys: vec![],
            checks: vec![],
        },
        primary_key_name: Some("orders_pkey".to_owned()),
    }
}

fn staging() -> Schema {
    let mut schema = Schema::empty(Dialect::Postgres, "staging");
    schema.tables.insert(
        "orders".to_owned(),
        orders(&[("id", "integer"), ("total", "numeric(10,2)")]),
    );
    schema
}

fn public() -> Schema {
    let mut schema = Schema::empty(Dialect::Postgres, "public");
    schema.tables.insert(
        "orders".to_owned(),
        orders(&[
            ("id", "integer"),
            ("total", "numeric(10,2)"),
            ("note", "text"),
        ]),
    );
    schema.indexes.insert(
        "orders_total_idx".to_owned(),
        Index {
            table: "orders".to_owned(),
            unique: false,
            method: "btree".to_owned(),
            key: vec!["total".to_owned()],
            include: vec![],
            predicate: None,
        },
    );
    schema
}

/// Picks `data_source` and types `schema` as the right side.
fn pick_right(harness: &mut Harness<'_, Open>, data_source: &str, schema: &str) {
    harness
        .get_by(|node| {
            node.role() == Role::ComboBox && node.value().as_deref() == Some("Pick a data source")
        })
        .click();
    harness.run();
    harness
        .get_all_by_label(data_source)
        .last()
        .unwrap()
        .click();
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "Right")
        .click();
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "Right")
        .type_text(schema);
    harness.run();
}

/// Answers the reads the last **Compare** asked for, as the engine would.
fn answer(harness: &mut Harness<'_, Open>, left: Result<Schema, String>, right: Schema) {
    let Some(CompareAction::Read(sides)) = harness.state_mut().actions.pop() else {
        panic!("expected Compare to ask for both sides");
    };
    let [first, second] = sides.as_slice() else {
        panic!("expected two sides, got {sides:?}");
    };
    for (side, result) in [(first, left), (second, Ok(right))] {
        harness.state_mut().compares.apply(&Event::SchemaDescribed {
            data_source: side.data_source,
            schema: side.schema.clone(),
            result,
        });
    }
    harness.run();
}

fn tab<'h>(harness: &'h Harness<'_, Open>) -> &'h CompareTab {
    let open = harness.state();
    &open.compares.tabs[&open.id]
}

#[test]
fn compares_two_schemas_lists_what_differs_and_opens_the_script() {
    let dev = candidate("shop-dev");
    let prod = candidate("shop");
    let mut harness = harness(vec![dev.clone(), prod.clone()]);
    assert!(
        harness
            .get_by_label("Compare")
            .accesskit_node()
            .is_disabled()
    );

    pick_right(&mut harness, "shop", "public");
    harness.get_by_label("Compare").click();
    // The sides' spinner repaints until the reads are answered.
    harness.run_steps(2);
    assert!(tab(&harness).sides.iter().all(Side::is_reading));
    answer(&mut harness, Ok(staging()), public());

    harness.get_by_label("2 differences");
    harness.get_by_label("~ orders");
    harness.get_by_label("+ orders_total_idx");
    assert!(
        harness
            .query_all_by_label_contains("1 column added")
            .next()
            .is_some()
    );
    assert_eq!(tab(&harness).title(), "staging \u{2192} public");

    harness.get_by_label("~ orders").click();
    harness.run();
    harness.get_by_label("Left: schema staging on shop-dev");
    harness.get_by_label("Right: schema public on shop");
    harness.get_by_label("    note text,");

    harness.get_by_label("Open script in console").click();
    harness.run();
    let Some(CompareAction::OpenInConsole { data_source, text }) =
        harness.state_mut().actions.pop()
    else {
        panic!("expected the script to open");
    };
    assert_eq!(data_source, dev.id);
    assert!(
        text.contains("ALTER TABLE staging.orders ADD COLUMN note text;"),
        "{text}"
    );
    assert!(
        text.contains("CREATE INDEX orders_total_idx ON staging.orders"),
        "{text}"
    );

    harness.get_by_label("Compare again").click();
    harness.run_steps(2);
    assert!(matches!(
        harness.state().actions.last(),
        Some(CompareAction::Read(sides)) if sides.len() == 2
    ));
}

#[test]
fn the_same_schemas_have_nothing_to_open() {
    let dev = candidate("shop-dev");
    let mut harness = harness(vec![dev]);
    pick_right(&mut harness, "shop-dev", "public");
    harness.get_by_label("Compare").click();
    // The sides' spinner repaints until the reads are answered.
    harness.run_steps(2);
    let mut same = staging();
    same.name = "public".to_owned();
    answer(&mut harness, Ok(staging()), same);

    assert!(
        harness
            .query_all_by_label("The schemas are the same.")
            .next()
            .is_some()
    );
    assert!(
        harness
            .get_by_label("Open script in console")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn a_side_that_fails_says_why() {
    let dev = candidate("shop-dev");
    let mut harness = harness(vec![dev]);
    pick_right(&mut harness, "shop-dev", "public");
    harness.get_by_label("Compare").click();
    // The sides' spinner repaints until the reads are answered.
    harness.run_steps(2);
    answer(
        &mut harness,
        Err("There is no schema \u{201c}staging\u{201d}.".to_owned()),
        public(),
    );

    harness.get_by_label("Left: There is no schema \u{201c}staging\u{201d}.");
    assert!(tab(&harness).outcome.is_none());
}

#[test]
fn swapping_turns_the_script_round() {
    let dev = candidate("shop-dev");
    let prod = candidate("shop");
    let mut harness = harness(vec![dev, prod.clone()]);
    pick_right(&mut harness, "shop", "public");

    harness.get_by_label("Swap sides").click();
    harness.run();

    let sides = &tab(&harness).sides;
    assert_eq!(sides[0].data_source, Some(prod.id));
    assert_eq!(sides[0].schema, "public");
    assert_eq!(sides[1].schema, "staging");
}
