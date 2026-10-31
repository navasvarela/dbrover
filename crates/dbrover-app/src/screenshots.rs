//! Renders the app headlessly into PNG screenshots, so the user manual's pictures are
//! regenerated from code instead of taken by hand.
//!
//! Ignored by default, because it needs a GPU adapter and rewrites files:
//!
//! ```text
//! cargo test -p dbrover-app regenerate_screenshots -- --ignored
//! ```
//!
//! Images go to `book/src/images`, or `DBROVER_SCREENSHOTS_DIR` when set. Scenes are rendered in
//! the light theme, or in each theme `DBROVER_SCREENSHOTS_THEMES` lists (`light,dark`), against a
//! SQLite fixture database. When `DBROVER_TEST_POSTGRES_URL` is set, a PostgreSQL data source
//! joins it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use dbrover_core::{
    ConnectionConfig, DataSource, DataSourceId, Password, PostgresConfig, ProjectId, ServerConfig,
    SourceColor, SqliteConfig, TlsMode,
};
use dbrover_store::{
    DataSourceFile, HistoryEntry, HistoryFile, HistoryOutcome, HistorySource, NamedCell, Project,
    ProjectFile, ProjectSlug, Projects, ReplacedCell, RowChange, RowChangeKind,
};
use egui::accesskit::Role;
use egui::{Theme, Vec2};
use egui_kittest::kittest::{By, NodeT, Queryable};
use egui_kittest::{Harness, HarnessBuilder};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::{Layer as _, reload};

use crate::app::DbRoverApp;
use crate::log_console::{self, LogConsoleLayer};
use crate::telemetry;
use crate::test_support::{MemorySecrets, start_app, step_until_visible};

mod site;

/// A typical laptop window; wide enough for the explorer, a console and its results.
const WINDOW_SIZE: Vec2 = Vec2::new(1180.0, 740.0);
/// Rendered at 2x so the images stay sharp on high-density screens.
const PIXELS_PER_POINT: f32 = 2.0;
/// Enough frames for egui's fade-in animations and the dock's layout to settle.
const SETTLE_STEPS: usize = 8;

const SHOP_SCHEMA: &str = "
    CREATE TABLE customers (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        email TEXT UNIQUE,
        country TEXT NOT NULL
    );
    CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT NOT NULL, price NUMERIC NOT NULL);
    CREATE TABLE orders (
        id INTEGER PRIMARY KEY,
        customer_id INTEGER NOT NULL REFERENCES customers (id),
        placed_at TEXT NOT NULL,
        status TEXT NOT NULL
    );
    CREATE TABLE order_items (
        order_id INTEGER NOT NULL REFERENCES orders (id),
        product_id INTEGER NOT NULL REFERENCES products (id),
        quantity INTEGER NOT NULL,
        PRIMARY KEY (order_id, product_id)
    );
    CREATE INDEX orders_by_customer ON orders (customer_id);
    CREATE VIEW order_totals AS
        SELECT o.id, o.customer_id, SUM(i.quantity * p.price) AS total
        FROM orders o
        JOIN order_items i ON i.order_id = o.id
        JOIN products p ON p.id = i.product_id
        GROUP BY o.id;

    INSERT INTO customers (name, email, country) VALUES
        ('Ada Lovelace', 'ada@example.com', 'GB'),
        ('Grace Hopper', 'grace@example.com', 'US'),
        ('Edsger Dijkstra', 'edsger@example.com', 'NL'),
        ('Barbara Liskov', 'barbara@example.com', 'US'),
        ('Donald Knuth', 'don@example.com', 'US'),
        ('Margaret Hamilton', 'margaret@example.com', 'US'),
        ('Niklaus Wirth', 'niklaus@example.com', 'CH'),
        ('Frances Allen', NULL, 'US');
    INSERT INTO products (name, price) VALUES
        ('Mechanical keyboard', 129.00), ('Desk lamp', 45.50), ('Monitor arm', 89.90),
        ('Notebook', 6.20), ('Fountain pen', 32.00), ('Headphones', 199.00);
    WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 60)
    INSERT INTO orders (customer_id, placed_at, status)
        SELECT (i * 5) % 8 + 1, date('2026-01-01', '+' || (i * 3) || ' days'),
               CASE WHEN i % 11 = 0 THEN 'cancelled' WHEN i % 4 = 0 THEN 'open' ELSE 'shipped' END
        FROM n;
    INSERT INTO order_items (order_id, product_id, quantity)
        SELECT id, id % 6 + 1, id % 3 + 1 FROM orders
        UNION ALL SELECT id, (id + 2) % 6 + 1, 1 FROM orders WHERE id % 2 = 0;

    CREATE TABLE events (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, happened_at TEXT NOT NULL);
    WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1200)
    INSERT INTO events (kind, happened_at)
        SELECT CASE i % 3 WHEN 0 THEN 'login' WHEN 1 THEN 'search' ELSE 'checkout' END,
               datetime('2026-03-01', '+' || (i * 7) || ' minutes')
        FROM n;
";

const BEST_CUSTOMERS: &str = "-- Best customers this year
SELECT c.name, c.country, COUNT(*) AS orders, ROUND(SUM(t.total), 2) AS spent
FROM customers c
JOIN order_totals t ON t.customer_id = c.id
JOIN orders o ON o.id = t.id
WHERE o.status <> 'cancelled'
GROUP BY c.id
ORDER BY spent DESC;
";

const MISSING_TABLE: &str = "SELECT id, carrier\nFROM shipments\nWHERE delivered_at IS NULL;\n";

/// More rows than one page (500), so the results show "more available".
const ALL_EVENTS: &str = "SELECT * FROM events;\n";

const SCRIPT: &str = "SELECT count(*) AS customers FROM customers;
SELECT status, count(*) AS orders FROM orders GROUP BY status;
UPDATE orders SET status = 'shipped' WHERE id = 4;
";

const DELETE_EVERYTHING: &str = "DELETE FROM order_items;\n";

struct Scene {
    name: &'static str,
    data_sources: FixtureSources,
    /// Drives a freshly started app into the state to capture.
    play: fn(&mut Harness<'static, DbRoverApp>, &Fixture),
    /// Leaves the pointer where the scene put it, for a tooltip; otherwise it is taken away,
    /// so no cursor or hover highlight ends up in the image.
    keeps_pointer: bool,
    /// The test server variables the scene needs; without them it is skipped and its image
    /// left as it is.
    needs: &'static [&'static str],
}

const POSTGRES_URL: &str = "DBROVER_TEST_POSTGRES_URL";
const SSH_URL: &str = "DBROVER_TEST_SSH_URL";
const SSH_POSTGRES_URL: &str = "DBROVER_TEST_SSH_POSTGRES_URL";

/// Which data sources a scene's app starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixtureSources {
    /// None at all, as on first launch.
    None,
    /// The `shop` SQLite database, plus `warehouse` on PostgreSQL when one is configured.
    Shop,
    /// The same, grouped into projects, with two more SQLite data sources over the shop
    /// database: `archive` beside `shop`, and `scratch` in no project.
    Projects,
}

const SCENES: &[Scene] = &[
    Scene {
        name: "first-launch",
        data_sources: FixtureSources::None,
        play: settle,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "welcome",
        data_sources: FixtureSources::Shop,
        play: settle,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "data-source-dialog",
        data_sources: FixtureSources::Shop,
        play: data_source_dialog,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "sqlite-data-source-dialog",
        data_sources: FixtureSources::Shop,
        play: sqlite_data_source_dialog,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "mysql-data-source-dialog",
        data_sources: FixtureSources::Shop,
        play: mysql_data_source_dialog,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "sqlserver-data-source-dialog",
        data_sources: FixtureSources::Shop,
        play: sqlserver_data_source_dialog,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "duckdb-data-source-dialog",
        data_sources: FixtureSources::Shop,
        play: duckdb_data_source_dialog,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "clickhouse-data-source-dialog",
        data_sources: FixtureSources::Shop,
        play: clickhouse_data_source_dialog,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "data-source-placeholders",
        data_sources: FixtureSources::Shop,
        play: data_source_placeholders,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "ssh-data-source-dialog",
        data_sources: FixtureSources::Shop,
        play: ssh_data_source_dialog,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "host-key-prompt",
        data_sources: FixtureSources::Shop,
        play: host_key_prompt,
        keeps_pointer: false,
        needs: &[SSH_URL, SSH_POSTGRES_URL],
    },
    Scene {
        name: "password-prompt",
        data_sources: FixtureSources::Shop,
        play: password_prompt,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "password-command-prompt",
        data_sources: FixtureSources::Shop,
        play: password_command_prompt,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "export-data-sources",
        data_sources: FixtureSources::Projects,
        play: export_data_sources,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "import-preview",
        data_sources: FixtureSources::Shop,
        play: import_preview,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "safety-level",
        data_sources: FixtureSources::Shop,
        play: safety_level,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "careful-confirmation",
        data_sources: FixtureSources::Shop,
        play: careful_confirmation,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "read-only-refusal",
        data_sources: FixtureSources::Shop,
        play: read_only_refusal,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "explorer-menu",
        data_sources: FixtureSources::Shop,
        play: explorer_menu,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "new-table",
        data_sources: FixtureSources::Shop,
        play: new_table,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "modify-table",
        data_sources: FixtureSources::Shop,
        play: modify_table,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "drop-table",
        data_sources: FixtureSources::Shop,
        play: drop_table,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "new-view",
        data_sources: FixtureSources::Shop,
        play: new_view,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "new-user",
        data_sources: FixtureSources::Shop,
        play: new_user,
        keeps_pointer: false,
        needs: &[POSTGRES_URL],
    },
    Scene {
        name: "explorer-projects",
        data_sources: FixtureSources::Projects,
        play: explorer_projects,
        // The Move to project submenu stays open only while the pointer is over it.
        keeps_pointer: true,
        needs: &[],
    },
    Scene {
        name: "project-menu",
        data_sources: FixtureSources::Projects,
        play: project_menu,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "project-settings",
        data_sources: FixtureSources::Projects,
        play: project_settings,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "settings",
        data_sources: FixtureSources::Shop,
        play: settings_editor,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "settings-mcp",
        data_sources: FixtureSources::Shop,
        play: settings_mcp,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "history-mcp",
        data_sources: FixtureSources::Shop,
        play: history_mcp,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "history-audit",
        data_sources: FixtureSources::Shop,
        play: history_audit,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "explorer-filter",
        data_sources: FixtureSources::Shop,
        play: explorer_filter,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "definition",
        data_sources: FixtureSources::Shop,
        play: definition,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "workspace",
        data_sources: FixtureSources::Shop,
        play: workspace,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "results-paging",
        data_sources: FixtureSources::Shop,
        play: results_paging,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "multiple-statements",
        data_sources: FixtureSources::Shop,
        play: multiple_statements,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "console-error",
        data_sources: FixtureSources::Shop,
        play: console_error,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "run-anyway",
        data_sources: FixtureSources::Shop,
        play: run_anyway,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "completion",
        data_sources: FixtureSources::Shop,
        play: completion,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "problems",
        data_sources: FixtureSources::Shop,
        play: problems,
        keeps_pointer: true,
        needs: &[],
    },
    Scene {
        name: "grid-changes",
        data_sources: FixtureSources::Shop,
        play: grid_changes,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "follow-key",
        data_sources: FixtureSources::Shop,
        play: follow_key,
        keeps_pointer: true,
        needs: &[],
    },
    Scene {
        name: "parameters",
        data_sources: FixtureSources::Shop,
        play: parameters,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "find-replace",
        data_sources: FixtureSources::Shop,
        play: find_replace,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "export-dialog",
        data_sources: FixtureSources::Shop,
        play: export_dialog,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "csv-import",
        data_sources: FixtureSources::Shop,
        play: csv_import,
        keeps_pointer: false,
        needs: &[],
    },
    Scene {
        name: "console-reconnect",
        data_sources: FixtureSources::Shop,
        play: console_reconnect,
        keeps_pointer: false,
        needs: &[POSTGRES_URL],
    },
];

#[test]
#[ignore = "needs a GPU adapter and rewrites the screenshots; run with --ignored"]
fn regenerate_screenshots() {
    let out_dir = output_dir();
    std::fs::create_dir_all(&out_dir).unwrap();
    let themes = themes();
    for scene in SCENES.iter().filter(|scene| has_servers_for(scene)) {
        for &theme in &themes {
            let file_name = match themes.as_slice() {
                [_] => format!("{}.png", scene.name),
                _ => format!("{}-{}.png", scene.name, theme_name(theme)),
            };
            let path = out_dir.join(file_name);
            render_scene(scene, theme, WINDOW_SIZE, whole_window)
                .save(&path)
                .unwrap();
            println!("wrote {}", path.display());
        }
    }
}

/// Picks the part of the window a picture shows, in points; `None` keeps all of it.
type Region = fn(&Harness<'static, DbRoverApp>) -> Option<egui::Rect>;

/// Plays `scene` in a fresh app with a window of `size` and renders `region` of where it ends.
fn render_scene(scene: &Scene, theme: Theme, size: Vec2, region: Region) -> image::RgbaImage {
    let fixture = Fixture::new(scene.data_sources);
    let mut harness = fixture.start(theme, size);
    (scene.play)(&mut harness, &fixture);
    if !scene.keeps_pointer {
        harness.event(egui::Event::PointerGone);
    }
    harness.run_steps(SETTLE_STEPS);
    let image = harness.render().unwrap();
    match region(&harness) {
        Some(rect) => crop(&image, rect),
        None => image,
    }
}

fn whole_window(_: &Harness<'static, DbRoverApp>) -> Option<egui::Rect> {
    None
}

/// The pixels under `rect`, in points, kept inside the image.
fn crop(image: &image::RgbaImage, rect: egui::Rect) -> image::RgbaImage {
    let pixels = |points: f32, limit: u32| ((points * PIXELS_PER_POINT).max(0.0) as u32).min(limit);
    let left = pixels(rect.min.x, image.width());
    let top = pixels(rect.min.y, image.height());
    let right = pixels(rect.max.x, image.width());
    let bottom = pixels(rect.max.y, image.height());
    image::imageops::crop_imm(image, left, top, right - left, bottom - top).to_image()
}

/// Whether every test server variable `scene` needs is set, saying which isn't.
fn has_servers_for(scene: &Scene) -> bool {
    match scene
        .needs
        .iter()
        .find(|variable| std::env::var_os(variable).is_none())
    {
        Some(missing) => {
            println!("skipped {}: {missing} is not set", scene.name);
            false
        }
        None => true,
    }
}

/// The manual uses the light theme, which also prints well. Design reviews ask for both.
fn themes() -> Vec<Theme> {
    let Ok(names) = std::env::var("DBROVER_SCREENSHOTS_THEMES") else {
        return vec![Theme::Light];
    };
    names
        .split(',')
        .map(|name| match name.trim() {
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            other => panic!("unknown theme {other:?} in DBROVER_SCREENSHOTS_THEMES"),
        })
        .collect()
}

fn output_dir() -> PathBuf {
    std::env::var_os("DBROVER_SCREENSHOTS_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../book/src/images"),
        PathBuf::from,
    )
}

fn theme_name(theme: Theme) -> &'static str {
    match theme {
        Theme::Light => "light",
        Theme::Dark => "dark",
    }
}

/// A temporary directory with the fixture database and a data source file pointing at it.
struct Fixture {
    dir: tempfile::TempDir,
    postgres: Option<(DataSource, Password)>,
    sources: FixtureSources,
}

impl Fixture {
    fn new(sources: FixtureSources) -> Self {
        let dir = tempfile::tempdir().unwrap();
        if sources == FixtureSources::None {
            return Self {
                dir,
                postgres: None,
                sources,
            };
        }
        let shop = dir.path().join("shop.db");
        rusqlite::Connection::open(&shop)
            .unwrap()
            .execute_batch(SHOP_SCHEMA)
            .unwrap();
        let postgres = std::env::var(POSTGRES_URL)
            .ok()
            .map(|url| postgres_data_source(&url));
        let mut data_sources = vec![DataSource {
            id: DataSourceId::generate(),
            name: "shop".to_owned(),
            color: Some(SourceColor::Orange),
            safety: dbrover_core::SafetyLevel::Normal,
            project: None,
            mcp_login: None,
            config: ConnectionConfig::Sqlite(SqliteConfig {
                path: shop,
                read_only: false,
            }),
        }];
        data_sources.extend(postgres.iter().map(|(data_source, _)| data_source.clone()));
        if sources == FixtureSources::Projects {
            group_into_projects(dir.path(), &mut data_sources);
        }
        DataSourceFile::new(dir.path().join("data_sources.toml"))
            .save(&data_sources)
            .unwrap();
        Self {
            dir,
            postgres,
            sources,
        }
    }

    fn start(&self, theme: Theme, size: Vec2) -> Harness<'static, DbRoverApp> {
        let builder = HarnessBuilder::default()
            .with_size(size)
            .with_pixels_per_point(PIXELS_PER_POINT)
            .with_theme(theme)
            .wgpu();
        let secrets = match &self.postgres {
            Some((data_source, password)) => {
                MemorySecrets::with_password(data_source.id, password.clone())
            }
            None => MemorySecrets::default(),
        };
        let mut harness = start_app(builder, self.dir.path(), log_feed(), secrets);
        let ready = match self.sources {
            FixtureSources::None => "No data sources yet",
            FixtureSources::Shop | FixtureSources::Projects => "Expand shop",
        };
        step_until_visible(&mut harness, ready);
        harness
    }
}

/// Puts `shop` and a new `archive` in a "Web shop" project, `warehouse` (if there is one) in
/// "Reporting", and adds `scratch` in no project; writes `projects.toml`.
fn group_into_projects(dir: &Path, data_sources: &mut Vec<DataSource>) {
    let project = |name: &str| {
        Project::new(
            ProjectId::generate(),
            name.to_owned(),
            ProjectSlug::from_name(name),
        )
    };
    let web_shop = project("Web shop");
    let reporting = project("Reporting");
    let sqlite_copy = |shop: &DataSource, name: &str, project: Option<ProjectId>| DataSource {
        id: DataSourceId::generate(),
        name: name.to_owned(),
        color: None,
        project,
        ..shop.clone()
    };
    let shop = &mut data_sources[0];
    shop.project = Some(web_shop.id);
    let shop = shop.clone();
    for data_source in data_sources.iter_mut().skip(1) {
        data_source.project = Some(reporting.id);
    }
    data_sources.push(sqlite_copy(&shop, "archive", Some(web_shop.id)));
    data_sources.push(sqlite_copy(&shop, "scratch", None));
    ProjectFile::new(dir.join("projects.toml"))
        .save(&Projects {
            projects: vec![web_shop, reporting],
            ..Projects::default()
        })
        .unwrap();
}

/// Parses `postgres://user:password@host:port/database`, the form the tests' URL takes.
fn postgres_data_source(url: &str) -> (DataSource, Password) {
    let rest = url.strip_prefix("postgres://").unwrap();
    let (credentials, rest) = rest.split_once('@').unwrap();
    let (user, password) = credentials.split_once(':').unwrap();
    let (address, database) = rest.split_once('/').unwrap();
    let (host, port) = address.split_once(':').unwrap();
    let data_source = DataSource {
        id: DataSourceId::generate(),
        name: "warehouse".to_owned(),
        color: Some(SourceColor::Red),
        safety: dbrover_core::SafetyLevel::Normal,
        project: None,
        mcp_login: None,
        config: ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                host: host.to_owned(),
                port: port.parse::<u16>().unwrap().into(),
                database: database.to_owned(),
                user: user.to_owned(),
                tls: TlsMode::default(),
                ssh: None,
                password_command: None,
            },
        }),
    };
    (data_source, Password::new(password))
}

/// A log feed that also receives what the engine logs on its own threads.
///
/// A thread-local subscriber would only see the UI thread, so one global subscriber is
/// installed for the whole run, and each app's log console layer is swapped into it.
fn log_feed() -> log_console::LogFeed {
    type Handle = reload::Handle<Option<LogConsoleLayer>, tracing_subscriber::Registry>;
    static HANDLE: OnceLock<Handle> = OnceLock::new();

    let (layer, feed) = log_console::pipeline();
    let handle = HANDLE.get_or_init(|| {
        let (reloadable, handle) = reload::Layer::new(None);
        let subscriber =
            tracing_subscriber::registry().with(reloadable.with_filter(telemetry::targets()));
        tracing::subscriber::set_global_default(subscriber).unwrap();
        handle
    });
    handle.reload(Some(layer)).unwrap();
    feed
}

fn expand(harness: &mut Harness<'static, DbRoverApp>, labels: &[&str]) {
    for label in labels {
        let expander = format!("Expand {label}");
        step_until_visible(harness, &expander);
        harness.get_by_label(&expander).click();
        harness.run_ok();
    }
}

fn open_console(harness: &mut Harness<'static, DbRoverApp>, data_source: &str, sql: &str) {
    harness.get_by_label(data_source).click_secondary();
    harness.run_ok();
    // The Welcome tab has a card of the same name; the context menu comes first.
    harness
        .get_all_by_label("New query console")
        .next()
        .unwrap()
        .click();
    harness.run_ok();
    let editor = harness.get_by_role(Role::MultilineTextInput);
    editor.focus();
    harness.run_ok();
    harness.get_by_role(Role::MultilineTextInput).type_text(sql);
    harness.run_ok();
}

fn run_all(harness: &mut Harness<'static, DbRoverApp>, finished: &str) {
    harness.get_by_label("Run all").click();
    step_until_visible(harness, finished);
}

fn settle(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    harness.run_ok();
}

fn data_source_dialog(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_new_data_source_dialog(harness, "PostgreSQL");
}

fn sqlite_data_source_dialog(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_new_data_source_dialog(harness, "SQLite");
}

fn mysql_data_source_dialog(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_new_data_source_dialog(harness, "MySQL / MariaDB");
}

fn sqlserver_data_source_dialog(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_new_data_source_dialog(harness, "SQL Server");
}

fn duckdb_data_source_dialog(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_new_data_source_dialog(harness, "DuckDB");
}

fn clickhouse_data_source_dialog(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_new_data_source_dialog(harness, "ClickHouse");
}

fn open_new_data_source_dialog(harness: &mut Harness<'static, DbRoverApp>, driver: &str) {
    harness
        .get_all_by_label("New data source")
        .next()
        .unwrap()
        .click();
    harness.run_ok();
    harness.get_by_label(driver).click();
    harness.run_ok();
}

/// Replaces the text of the field labelled `label` with `text`.
fn retype(harness: &mut Harness<'static, DbRoverApp>, label: &str, text: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .focus();
    harness.run_ok();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.key_press(egui::Key::Backspace);
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(text);
    harness.run_ok();
}

fn data_source_placeholders(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_new_data_source_dialog(harness, "PostgreSQL");
    retype(harness, "Name", "staging");
    retype(harness, "Host", "db-${DBROVER_ENV}.internal");
    retype(harness, "Port", "${PGPORT}");
    retype(harness, "User", "${USER}");
}

fn ssh_data_source_dialog(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_new_data_source_dialog(harness, "PostgreSQL");
    retype(harness, "Name", "orders (production)");
    retype(harness, "Host", "10.0.4.12");
    retype(harness, "Database", "orders");
    retype(harness, "User", "reporting");
    harness
        .get_by_label("Connect through an SSH server")
        .click();
    harness.run_ok();
    retype(harness, "SSH host", "bastion.example.com");
    retype(harness, "SSH user", "deploy");
    choose(harness, "SSH agent", "Key file");
    retype(harness, "Key file", "~/.ssh/id_ed25519");
}

/// Picks `choice` in the combo box showing `current`.
fn choose(harness: &mut Harness<'static, DbRoverApp>, current: &str, choice: &str) {
    harness
        .get_by(|node| node.role() == Role::ComboBox && node.value().as_deref() == Some(current))
        .click();
    harness.run_ok();
    harness.get_by_label(choice).click();
    harness.run_ok();
}

/// A PostgreSQL data source behind the `ssh` compose service, saved through the dialog, then
/// connected to for the first time, with a `known_hosts` that doesn't know the server yet.
fn host_key_prompt(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    let ssh = std::env::var(SSH_URL).unwrap();
    let (ssh_host, ssh_port) = ssh.rsplit_once('@').unwrap().1.split_once(':').unwrap();
    let postgres = std::env::var(SSH_POSTGRES_URL).unwrap();
    let (login, rest) = postgres
        .strip_prefix("postgres://")
        .unwrap()
        .split_once('@')
        .unwrap();
    let user = login.split_once(':').unwrap().0;
    let (address, database) = rest.split_once('/').unwrap();
    let (host, port) = address.split_once(':').unwrap();
    let key_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/ssh/client_ed25519");
    open_new_data_source_dialog(harness, "PostgreSQL");
    retype(harness, "Name", "orders (production)");
    retype(harness, "Host", host);
    retype(harness, "Port", port);
    retype(harness, "Database", database);
    retype(harness, "User", user);
    harness
        .get_by_label("Connect through an SSH server")
        .click();
    harness.run_ok();
    retype(harness, "SSH host", ssh_host);
    retype(harness, "SSH port", ssh_port);
    retype(harness, "SSH user", "dbrover");
    choose(harness, "SSH agent", "Key file");
    retype(harness, "Key file", &key_file.display().to_string());
    harness.get_by_label("Save").click();
    expand(harness, &["orders (production)"]);
    step_until_visible(harness, "Trust the SSH server");
}

/// A console whose session ended under it, here by its own hand so no other session on the
/// test server is touched, then asked to run something.
fn console_reconnect(harness: &mut Harness<'static, DbRoverApp>, fixture: &Fixture) {
    let Some((postgres, _)) = &fixture.postgres else {
        return;
    };
    open_console(
        harness,
        &postgres.name,
        "SELECT pg_terminate_backend(pg_backend_pid());",
    );
    run_all(harness, "session settings are gone");
    harness.get_by_role(Role::MultilineTextInput).focus();
    harness.run_ok();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role(Role::MultilineTextInput)
        .type_text("SELECT status, count(*) FROM orders GROUP BY status;\n");
    harness.run_ok();
    run_all(harness, "reconnect to go on");
}

fn password_prompt(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_new_data_source_dialog(harness, "PostgreSQL");
    retype(harness, "Name", "staging");
    retype(harness, "Host", "staging.internal");
    harness.get_by_label("Save").click();
    expand(harness, &["staging"]);
    step_until_visible(harness, "Connect to staging");
}

/// An RDS data source whose password is an IAM token, saved through the dialog, then connected
/// to for the first time. Nothing runs: the picture is the question before the first run.
fn password_command_prompt(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_new_data_source_dialog(harness, "PostgreSQL");
    retype(harness, "Name", "orders (production)");
    retype(
        harness,
        "Host",
        "orders.cluster-c9x2.eu-west-1.rds.amazonaws.com",
    );
    retype(harness, "User", "app_reader");
    harness.get_by_label("Password from a command").click();
    harness.run_ok();
    let command = [
        "aws",
        "rds",
        "generate-db-auth-token",
        "--hostname",
        "{host}",
        "--port",
        "{port}",
        "--username",
        "{user}",
        "--region",
        "eu-west-1",
    ]
    .join("\n");
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Password command")
        .focus();
    harness.run_ok();
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Password command")
        .type_text(&command);
    harness.run_ok();
    harness.get_by_label("Save").click();
    expand(harness, &["orders (production)"]);
    step_until_visible(harness, "Run this command for a password?");
}

fn open_file_menu_item(harness: &mut Harness<'static, DbRoverApp>, item: &str) {
    harness.get_by_label("File").click();
    harness.run_ok();
    harness.get_by_label(item).click();
    harness.run_ok();
}

fn export_data_sources(harness: &mut Harness<'static, DbRoverApp>, fixture: &Fixture) {
    harness
        .state_mut()
        .answer_file_dialogs_with(&fixture.dir.path().join("dbrover.toml"));
    open_file_menu_item(harness, "Export data sources…");
    harness
        .get_by_role_and_label(Role::CheckBox, "Web shop")
        .click();
    harness.run_ok();
}

/// A file from a teammate: the shop marked differently, a new project with a PostgreSQL data
/// source whose host is a placeholder, and a SQLite file this machine doesn't have.
fn import_preview(harness: &mut Harness<'static, DbRoverApp>, fixture: &Fixture) {
    let local = DataSourceFile::new(fixture.dir.path().join("data_sources.toml"))
        .load()
        .unwrap();
    let shop = local[0].clone();
    let reporting = Project::new(
        ProjectId::generate(),
        "Reporting".to_owned(),
        ProjectSlug::from_name("Reporting"),
    );
    let teams_shop = DataSource {
        color: Some(SourceColor::Green),
        ..shop.clone()
    };
    let analytics = DataSource {
        id: DataSourceId::generate(),
        name: "analytics".to_owned(),
        color: Some(SourceColor::Red),
        safety: dbrover_core::SafetyLevel::Normal,
        project: Some(reporting.id),
        mcp_login: None,
        config: ConnectionConfig::Postgres(PostgresConfig {
            server: ServerConfig {
                host: "${ANALYTICS_HOST}".to_owned(),
                port: 5432.into(),
                database: "analytics".to_owned(),
                user: "reporting".to_owned(),
                tls: TlsMode::Require,
                ssh: None,
                password_command: None,
            },
        }),
    };
    let archive = DataSource {
        id: DataSourceId::generate(),
        name: "archive".to_owned(),
        color: None,
        safety: dbrover_core::SafetyLevel::Normal,
        project: Some(reporting.id),
        mcp_login: None,
        config: ConnectionConfig::Sqlite(SqliteConfig {
            path: fixture.dir.path().join("archive/2025.db"),
            read_only: true,
        }),
    };
    let file = fixture.dir.path().join("team.toml");
    dbrover_store::write_export(
        &file,
        &[teams_shop, analytics, archive],
        std::slice::from_ref(&reporting),
        None,
    )
    .unwrap();
    harness.state_mut().answer_file_dialogs_with(&file);
    open_file_menu_item(harness, "Import data sources…");
    step_until_visible(harness, "Imports 1 project and 3 data sources.");
}

fn edit_shop(harness: &mut Harness<'static, DbRoverApp>) {
    harness.get_by_label("shop").click_secondary();
    harness.run_ok();
    harness.get_by_label("Edit…").click();
    harness.run_ok();
}

/// Sets `shop`'s safety level through its dialog, as a user would.
fn set_shop_safety(harness: &mut Harness<'static, DbRoverApp>, level: &str) {
    edit_shop(harness);
    harness
        .get_by_role_and_label(Role::RadioButton, level)
        .click();
    harness.run_ok();
    harness.get_by_label("Save").click();
    harness.run_ok();
    step_until_visible(harness, "Expand shop");
}

fn safety_level(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    edit_shop(harness);
    harness.get_by_label("Red").click();
    harness.run_ok();
}

fn careful_confirmation(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    set_shop_safety(harness, "Careful");
    open_console(
        harness,
        "shop",
        "UPDATE orders SET status = 'shipped' WHERE id = 4;\n",
    );
    run_all(harness, "Run on \u{201c}shop\u{201d}?");
}

fn read_only_refusal(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    set_shop_safety(harness, "Read-only");
    open_console(
        harness,
        "shop",
        "DELETE FROM order_items WHERE order_id = 4;\n",
    );
    run_all(harness, "Refused:");
}

fn explorer_menu(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    expand(harness, &["shop", "main", "tables", "orders", "columns"]);
    step_until_visible(harness, "customer_id");
    harness.get_by_label("orders").click_secondary();
    harness.run_ok();
}

/// Opens a menu button and ticks or picks `item` in it, then closes the menu.
fn pick(harness: &mut Harness<'static, DbRoverApp>, menu: &str, item: &str) {
    harness.get_by_label(menu).click();
    harness.run_ok();
    harness.get_by_label(item).click();
    harness.run_ok();
    harness.key_press(egui::Key::Escape);
    harness.run_ok();
}

/// New table with a few columns and a foreign key to `orders`, and the SQL it will run.
fn new_table(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    expand(harness, &["shop", "main", "tables"]);
    step_until_visible(harness, "Expand orders");
    harness.get_by_label("tables").click_secondary();
    harness.run_ok();
    harness.get_by_label("New").hover();
    harness.run_ok();
    harness.get_by_label("Table…").click();
    harness.run_ok();
    retype(harness, "Name", "shipments");
    for (row, name, data_type) in [
        (2, "order_id", "integer"),
        (3, "carrier", "text"),
        (4, "shipped_at", "text"),
    ] {
        harness.get_by_label("Add column").click();
        harness.run_ok();
        retype(harness, &format!("Column {row} name"), name);
        retype(harness, &format!("Column {row} type"), data_type);
    }
    for row in [2, 3] {
        harness
            .get_by_label(&format!("Column {row} nullable"))
            .click();
        harness.run_ok();
    }
    harness.get_by_label("Foreign keys").click();
    harness.run_ok();
    harness.get_by_label("Add foreign key").click();
    harness.run_ok();
    pick(harness, "Foreign key 1 columns", "order_id");
    harness.get_by_label("Foreign key 1 references").click();
    harness.run_ok();
    harness.get_by_label("main.orders").click();
    step_until_visible(harness, "Foreign key 1 referenced columns");
    pick(harness, "Foreign key 1 referenced columns", "id");
    // The foreign key's own "Columns" row label is there too; the tab is the one to click.
    harness
        .get_all_by_label("Columns")
        .find(|node| node.accesskit_node().role() != Role::Label)
        .unwrap()
        .click();
    harness.run_ok();
}

/// Modify table on `orders`: a column renamed and one added, and the `ALTER TABLE`
/// statements that make the change.
fn modify_table(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    expand(harness, &["shop", "main", "tables"]);
    step_until_visible(harness, "Expand orders");
    harness.get_by_label("orders").click_secondary();
    harness.run_ok();
    harness.get_by_label("Modify table…").click();
    step_until_visible(harness, "Column 4 name");
    retype(harness, "Column 4 name", "state");
    harness.get_by_label("Add column").click();
    harness.run_ok();
    retype(harness, "Column 5 name", "shipped_at");
    retype(harness, "Column 5 type", "text");
}

/// Drop… on a data source coloured red, which wants the table's name typed.
fn drop_table(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    edit_shop(harness);
    harness.get_by_label("Red").click();
    harness.run_ok();
    harness
        .get_by_role_and_label(Role::RadioButton, "Normal")
        .click();
    harness.run_ok();
    harness.get_by_label("Save").click();
    harness.run_ok();
    expand(harness, &["shop", "main", "tables"]);
    step_until_visible(harness, "Expand events");
    harness.get_by_label("events").click_secondary();
    harness.run_ok();
    harness.get_by_label("Drop…").click();
    harness.run_ok();
}

/// New view, its query in the highlighted editor, and the SQL it will run.
fn new_view(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    expand(harness, &["shop", "main"]);
    step_until_visible(harness, "Expand views");
    harness.get_by_label("views").click_secondary();
    harness.run_ok();
    harness.get_by_label("New").hover();
    harness.run_ok();
    harness.get_by_label("View…").click();
    harness.run_ok();
    retype(harness, "Name", "shipped_orders");
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Query")
        .focus();
    harness.run_ok();
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Query")
        .type_text(
            "SELECT o.id, c.name, o.placed_at\nFROM orders o\nJOIN customers c ON c.id = \
             o.customer_id\nWHERE o.status = 'shipped'",
        );
    harness.run_ok();
}

/// New user on the PostgreSQL data source, filled in but not run: the review panel shows the
/// password redacted.
fn new_user(harness: &mut Harness<'static, DbRoverApp>, fixture: &Fixture) {
    let Some((postgres, _)) = &fixture.postgres else {
        return;
    };
    expand(harness, &[postgres.name.as_str(), "roles"]);
    harness.get_by_label("roles").click_secondary();
    harness.run_ok();
    harness.get_by_label("New").hover();
    harness.run_ok();
    harness.get_by_label("User…").click();
    harness.run_ok();
    retype(harness, "Name", "report_reader");
    for label in ["Password", "Confirm password"] {
        harness
            .get_by_role_and_label(Role::PasswordInput, label)
            .focus();
        harness.run_ok();
        harness
            .get_by_role_and_label(Role::PasswordInput, label)
            .type_text("correct horse battery");
        harness.run_ok();
    }
    harness.get_by_label("Limit to").click();
    harness.run_ok();
    harness.get_by_label("Roles it belongs to").click();
    harness.run_ok();
    // PostgreSQL's own roles are hidden in the tree but offered here, once the folder loads.
    step_until_visible(harness, "pg_read_all_data");
    harness
        .get_by_role_and_label(Role::CheckBox, "pg_read_all_data")
        .click();
    harness.run_ok();
    harness.key_press(egui::Key::Escape);
    harness.run_ok();
}

fn explorer_projects(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    harness.get_by_label("scratch").click_secondary();
    harness.run_ok();
    harness.get_by_label("Move to project").hover();
    harness.run_ok();
}

fn project_menu(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    harness.get_by_label("Web shop").click_secondary();
    harness.run_ok();
}

/// The project with `warehouse` when there is one, so the missing MCP login is warned about;
/// otherwise the SQLite one.
fn project_settings(harness: &mut Harness<'static, DbRoverApp>, fixture: &Fixture) {
    let project = if fixture.postgres.is_some() {
        "Reporting"
    } else {
        "Web shop"
    };
    harness.get_by_label(project).click_secondary();
    harness.run_ok();
    harness.get_by_label("Project settings…").click();
    harness.run_ok();
    harness
        .get_by_label("Allow MCP clients to use this project")
        .click();
    harness.run_ok();
    harness.get_by_label("Write").click();
    harness.run_ok();
}

/// View › Settings…, on the Editor page.
fn settings_editor(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_settings_page(harness, "Editor");
}

fn settings_mcp(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_settings_page(harness, "MCP");
}

/// Opens the Settings tab on its page `category`.
fn open_settings_page(harness: &mut Harness<'static, DbRoverApp>, category: &str) {
    harness.get_by_label("View").click();
    harness.run_ok();
    harness.get_by_label_contains("Settings…").click();
    harness.run_ok();
    harness.get_by_label(category).click();
    harness.run_ok();
}

/// A few statements an assistant ran, recorded as `dbrover mcp` records them, beside the one
/// the console runs.
fn history_mcp(harness: &mut Harness<'static, DbRoverApp>, fixture: &Fixture) {
    let data_sources = DataSourceFile::new(fixture.dir.path().join("data_sources.toml"))
        .load()
        .unwrap();
    let shop = data_sources
        .iter()
        .find(|data_source| data_source.name == "shop")
        .unwrap();
    let history = HistoryFile::new(fixture.dir.path().join("history.sqlite"))
        .open()
        .unwrap();
    let now = jiff::Timestamp::now();
    for (minutes_ago, sql, rows) in [
        (12, "SELECT status, count(*) FROM orders GROUP BY status", 3),
        (9, "SELECT * FROM customers WHERE country = 'ES'", 2),
        (
            4,
            "INSERT INTO customers (name, country) VALUES ('Lucía', 'ES')",
            1,
        ),
    ] {
        history
            .record(&HistoryEntry {
                data_source: shop.id,
                executed_at: now - jiff::SignedDuration::from_mins(minutes_ago),
                sql: sql.to_owned(),
                duration: Some(std::time::Duration::from_millis(3)),
                rows: Some(rows),
                outcome: HistoryOutcome::Ok,
                source: HistorySource::Mcp {
                    client: Some("claude-ai".to_owned()),
                },
            })
            .unwrap();
    }
    open_console(harness, "shop", BEST_CUSTOMERS);
    run_all(harness, "8 rows");
    harness.get_by_label("History").click();
    step_until_visible(harness, "MCP");
}

/// Statements from several places, the grid edit's details open: the value it replaced, and
/// the statement that puts it back.
fn history_audit(harness: &mut Harness<'static, DbRoverApp>, fixture: &Fixture) {
    let data_sources = DataSourceFile::new(fixture.dir.path().join("data_sources.toml"))
        .load()
        .unwrap();
    let shop = data_sources
        .iter()
        .find(|data_source| data_source.name == "shop")
        .unwrap();
    let history = HistoryFile::new(fixture.dir.path().join("history.sqlite"))
        .open()
        .unwrap();
    let edit = r#"UPDATE "main"."customers" SET "country" = 'PT' WHERE "id" = '3'"#;
    let change = RowChange {
        table: "main.customers".to_owned(),
        kind: RowChangeKind::Updated {
            key: vec![NamedCell {
                column: "id".to_owned(),
                value: Some("3".to_owned()),
            }],
            cells: vec![ReplacedCell {
                column: "country".to_owned(),
                old: Some("ES".to_owned()),
                new: Some("PT".to_owned()),
            }],
        },
        revert: Some(
            r#"UPDATE "main"."customers" SET "country" = 'ES' WHERE "id" = '3'"#.to_owned(),
        ),
    };
    let now = jiff::Timestamp::now();
    for (minutes_ago, sql, source) in [
        (
            30,
            "CREATE INDEX orders_by_status ON orders (status)",
            HistorySource::ObjectHelper,
        ),
        (
            21,
            "SELECT * FROM customers WHERE country = 'ES'",
            HistorySource::Mcp {
                client: Some("claude-ai".to_owned()),
            },
        ),
        (8, edit, HistorySource::GridEdit(Some(change))),
    ] {
        history
            .record(&HistoryEntry {
                data_source: shop.id,
                executed_at: now - jiff::SignedDuration::from_mins(minutes_ago),
                sql: sql.to_owned(),
                duration: Some(std::time::Duration::from_millis(2)),
                rows: Some(1),
                outcome: HistoryOutcome::Ok,
                source,
            })
            .unwrap();
    }
    open_console(harness, "shop", BEST_CUSTOMERS);
    run_all(harness, "8 rows");
    harness.get_by_label("History").click();
    step_until_visible(harness, "MCP");
    harness.get_by_label(edit).click();
    harness.run_ok();
    step_until_visible(harness, "To put it back:");
}

fn explorer_filter(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    expand(harness, &["shop", "main", "tables", "views"]);
    let filter = By::new().predicate(|node| node.placeholder() == Some("Filter loaded nodes"));
    harness.get(filter).focus();
    harness.run_ok();
    let filter = By::new().predicate(|node| node.placeholder() == Some("Filter loaded nodes"));
    harness.get(filter).type_text("order");
    harness.run_ok();
}

fn definition(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    expand(harness, &["shop", "main", "views"]);
    step_until_visible(harness, "order_totals");
    harness.get_by_label("order_totals").click_secondary();
    harness.run_ok();
    harness.get_by_label("Show definition").click();
    // The definition is the value of a read-only text field, not a label.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let loaded = || {
        By::new().predicate(|node| {
            node.value()
                .is_some_and(|text| text.contains("CREATE VIEW"))
        })
    };
    while harness.query_all(loaded()).next().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the definition never loaded"
        );
        harness.step();
    }
}

fn workspace(harness: &mut Harness<'static, DbRoverApp>, fixture: &Fixture) {
    expand(harness, &["shop", "main", "tables", "views"]);
    if let Some((postgres, _)) = &fixture.postgres {
        expand(harness, &[postgres.name.as_str()]);
    }
    open_console(harness, "shop", BEST_CUSTOMERS);
    run_all(harness, "8 rows");
}

/// The label showing `value` in the grid's `column`, among other labels with the same text
/// (row numbers, other columns).
fn grid_cell<'a>(
    harness: &'a Harness<'static, DbRoverApp>,
    column: &str,
    value: &'a str,
) -> egui_kittest::Node<'a> {
    let header = harness.get_by_label(column).rect();
    harness
        .get_all_by_label(value)
        .find(|node| {
            let x = node.rect().center().x;
            x > header.min.x && x < header.min.x + 140.0
        })
        .unwrap_or_else(|| panic!("no {value:?} under {column:?}"))
}

/// Opens the cell editor on the selected cell, once the table is described and F2 edits.
fn edit_selected_cell(harness: &mut Harness<'static, DbRoverApp>, text: &str) {
    let editing = |harness: &Harness<'static, DbRoverApp>| {
        harness
            .query_by(|node| node.role() == Role::TextInput && node.is_focused())
            .is_some()
    };
    while !editing(harness) {
        harness.key_press(egui::Key::F2);
        harness.step();
    }
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by(|node| node.role() == Role::TextInput && node.is_focused())
        .type_text(text);
    harness.key_press(egui::Key::Enter);
    harness.run_ok();
}

/// A changed cell, a row marked for deletion and a new row, waiting for Submit.
fn grid_changes(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_console(harness, "shop", "SELECT * FROM customers WHERE id <= 4;\n");
    run_all(harness, "4 rows");
    step_until_visible(harness, "Add row");
    harness.get_by_label("grace@example.com").click();
    harness.run_ok();
    edit_selected_cell(harness, "grace.hopper@example.com");
    harness.get_by_label("Edsger Dijkstra").click_secondary();
    harness.run_ok();
    harness.get_by_label("Delete row").click();
    harness.run_ok();
    harness.get_by_label("Add row").click();
    harness.run_ok();
    new_row_cell(harness, 1).click();
    harness.run_ok();
    edit_selected_cell(harness, "Alan Turing");
    step_until_visible(harness, "1 changed cell · 1 new row · 1 deleted row");
}

/// The cell of the added row (the grid's last) in result column `column`, for a table
/// without defaults: until typed into, each of its cells shows `NULL`.
fn new_row_cell<'a>(
    harness: &'a Harness<'static, DbRoverApp>,
    column: usize,
) -> egui_kittest::Node<'a> {
    let bottom = harness
        .get_all_by_label("NULL")
        .map(|node| node.rect().center().y)
        .fold(f32::MIN, f32::max);
    let mut row: Vec<_> = harness
        .get_all_by_label("NULL")
        .filter(|node| (node.rect().center().y - bottom).abs() < 1.0)
        .collect();
    row.sort_by(|a, b| a.rect().min.x.total_cmp(&b.rect().min.x));
    row.into_iter().nth(column).expect("the new row's cell")
}

/// An order's customer, a link, with its menu open on Referencing rows.
fn follow_key(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_console(harness, "shop", "SELECT * FROM orders;\n");
    run_all(harness, "60 rows");
    step_until_visible(harness, "Add row");
    grid_cell(harness, "customer_id", "6").click_secondary();
    harness.run_ok();
    harness.get_by_label_contains("Referencing rows").click();
    harness.run_ok();
    step_until_visible(harness, "order_items.order_id");
    harness.get_by_label("order_items.order_id").hover();
    harness.run_ok();
}

/// Import CSV… on customers: the columns matched by name, the first rows, and a warning.
fn export_dialog(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_console(harness, "shop", "SELECT * FROM customers;");
    run_all(harness, "Add row");
    harness.get_by_label("Export…").click();
    harness.run_ok();
    harness.get_by_label("CSV for Excel").click();
    harness.run_ok();
}

fn csv_import(harness: &mut Harness<'static, DbRoverApp>, fixture: &Fixture) {
    let file = fixture.dir.path().join("new-customers.csv");
    std::fs::write(
        &file,
        "id,name,email,country\n\
         9,Alan Turing,alan@example.com,GB\n\
         10,Katherine Johnson,,US\n\
         A-11,Linus Torvalds,linus@example.com,FI\n\
         12,Ada Yonath,ada.yonath@example.com,IL\n",
    )
    .unwrap();
    harness.state_mut().pick_csv_file = Box::new(move || Some(file.clone()));
    expand(harness, &["shop", "main", "tables"]);
    step_until_visible(harness, "Expand customers");
    harness.get_by_label("customers").click_secondary();
    harness.run_ok();
    harness.get_by_label("Import CSV…").click();
    step_until_visible(harness, "Source of name");
    harness.run_ok();
}

fn parameters(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_console(
        harness,
        "shop",
        "SELECT * FROM orders\nWHERE placed_at >= :since\n  AND status = :status;",
    );
    harness.get_by_label("Run").click();
    step_until_visible(harness, "Parameter values");
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, ":since")
        .type_text("2026-01-01");
    harness.run_ok();
    harness
        .get_by_role_and_label(Role::TextInput, ":status")
        .focus();
    harness.run_ok();
    harness
        .get_by_role_and_label(Role::TextInput, ":status")
        .type_text("shipped");
    harness.run_ok();
}

fn find_replace(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_console(
        harness,
        "shop",
        "SELECT o.id, c.name\nFROM orders o\nJOIN customers c ON c.id = o.customer_id\n\
         WHERE o.status = 'shipped';\n\nSELECT count(*) FROM orders;",
    );
    harness.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::ALT,
        egui::Key::F,
    );
    harness.run_ok();
    harness
        .get_by_role_and_label(Role::TextInput, "Find")
        .type_text("orders");
    harness.run_ok();
    harness.key_press(egui::Key::Enter);
    harness.run_ok();
    harness
        .get_by_role_and_label(Role::TextInput, "Replace")
        .focus();
    harness.run_ok();
    harness
        .get_by_role_and_label(Role::TextInput, "Replace")
        .type_text("orders_2026");
    harness.run_ok();
}

fn results_paging(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_console(harness, "shop", ALL_EVENTS);
    run_all(harness, "500 rows, more available");
}

fn multiple_statements(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_console(harness, "shop", SCRIPT);
    run_all(harness, "Done in");
}

fn console_error(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_console(harness, "shop", MISSING_TABLE);
    run_all(harness, "no such table");
}

fn run_anyway(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_console(harness, "shop", DELETE_EVERYTHING);
    run_all(harness, "Run anyway");
}

fn completion(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_console(
        harness,
        "shop",
        "SELECT o.id, c.\nFROM orders o\nJOIN customers c ON c.id = o.customer_id",
    );
    // Back to just after `c.`, where the list opens on request.
    let after_dot = "SELECT o.id, c.".chars().count();
    let total = "SELECT o.id, c.\nFROM orders o\nJOIN customers c ON c.id = o.customer_id"
        .chars()
        .count();
    for _ in after_dot..total {
        harness.key_press(egui::Key::ArrowLeft);
    }
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Space);
    // The columns load into the explorer's cache in the background.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while harness.query_by_label("country").is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the columns were never suggested"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        harness.step();
    }
    // The list sizes itself from the frame before; let it grow to its full height.
    harness.run_steps(4);
}

fn problems(harness: &mut Harness<'static, DbRoverApp>, _: &Fixture) {
    open_console(
        harness,
        "shop",
        "SELECT id, status\nFROM orders\nWHERE placed_at = NULL;\n\nSELECT name FROM customers WHERE",
    );
    step_until_visible(harness, "1 error, 1 warning");
    let squiggle = harness
        .state()
        .open_consoles()
        .flat_map(|console| console.squiggle_rects().collect::<Vec<_>>())
        .next()
        .unwrap();
    harness.hover_at(squiggle.center());
    step_until_visible(harness, "never true");
}
