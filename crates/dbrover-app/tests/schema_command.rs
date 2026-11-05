//! `dbrover schema` as a CI job runs it: the real binary. Reading a PostgreSQL schema and
//! writing its snapshot are tested in dbrover-schema and dbrover-engine (`read_schema`, which
//! these commands call); this checks the commands around them: their arguments, what they
//! print and their exit codes, comparing snapshot files, which need no server.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

const BINARY: &str = env!("CARGO_BIN_EXE_dbrover");

#[test]
fn refuses_a_command_line_it_does_not_understand() {
    let cases: [(&[&str], &str); 3] = [
        (&["schema"], "dbrover schema needs a command: snapshot"),
        (
            &["schema", "snapshot", "shop"],
            "needs a data source and a schema",
        ),
        (
            &["schema", "snapshot", "shop", "public", "--outptu", "x"],
            "unknown argument \"--outptu\"",
        ),
    ];
    for (args, problem) in cases {
        let output = Command::new(BINARY).args(args).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(problem), "{args:?}: {stderr}");
        assert!(stderr.contains("Exit codes:"), "{args:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}

/// `orders` as a snapshot holds it, `extra` lines added to its table.
fn snapshot(extra: &str) -> String {
    format!(
        "[snapshot]\nformat = 1\nengine = \"postgresql\"\nschema = \"public\"\n\n\
         [tables.orders]\ncolumns = [\n  {{ name = \"id\", type = \"integer\", nullable = false }},\n{extra}]\n\
         primary_key = {{ name = \"orders_pkey\", columns = [\"id\"] }}\n"
    )
}

fn compare(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(BINARY)
        .args(["schema", "compare"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

/// `dbrover schema compare` exits as `diff` does: 1 with the differences, 0 when there are
/// none, 2 when a side can't be read.
#[test]
fn compares_two_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("before.schema.toml"), snapshot("")).unwrap();
    std::fs::write(
        dir.path().join("after.schema.toml"),
        snapshot("  { name = \"note\", type = \"text\" },\n"),
    )
    .unwrap();

    let differ = compare(dir.path(), &["before.schema.toml", "after.schema.toml"]);
    assert_eq!(differ.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&differ.stdout),
        "Tables\n  ~ orders\n      1 column added\n      + column note\n"
    );

    let script = compare(
        dir.path(),
        &["before.schema.toml", "after.schema.toml", "--script"],
    );
    assert_eq!(script.status.code(), Some(1));
    let script = String::from_utf8_lossy(&script.stdout);
    assert!(
        script.contains("-- Changes snapshot before.schema.toml (left) to match snapshot after.schema.toml (right)."),
        "{script}"
    );
    assert!(
        script.contains("ALTER TABLE \"public\".orders ADD COLUMN note text;"),
        "{script}"
    );

    let same = compare(dir.path(), &["before.schema.toml", "before.schema.toml"]);
    assert_eq!(same.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&same.stdout),
        "The schemas are the same.\n"
    );

    let missing = compare(dir.path(), &["before.schema.toml", "gone.schema.toml"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("could not read gone.schema.toml"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    assert!(missing.stdout.is_empty());
}

#[test]
fn prints_its_usage_when_asked() {
    let output = Command::new(BINARY)
        .args(["schema", "--help"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: dbrover schema snapshot <data source> <schema>"),
        "{stdout}"
    );
}

/// Unix only, as the report command's tests: the home folder is moved by setting `HOME`, which
/// Windows ignores, and the tests must not use the real one's.
#[cfg(unix)]
mod settings {
    use std::path::PathBuf;
    use std::process::Output;

    use super::*;

    /// A home folder whose settings folder has an SQLite data source, `shop`.
    struct Settings {
        home: tempfile::TempDir,
    }

    impl Settings {
        fn new() -> Self {
            let settings = Self {
                home: tempfile::tempdir().unwrap(),
            };
            std::fs::create_dir_all(settings.config()).unwrap();
            let database = settings.home.path().join("shop.db");
            rusqlite::Connection::open(&database)
                .unwrap()
                .execute_batch("CREATE TABLE orders (id integer PRIMARY KEY)")
                .unwrap();
            let data_sources = format!(
                "[[data_source]]\nid = \"6f1c1a3e-6a43-4b8e-9d55-0d6c2b7e1a01\"\nname = \"shop\"\n\
                 driver = \"sqlite\"\npath = {:?}\n",
                database.display().to_string()
            );
            std::fs::write(settings.config().join("data_sources.toml"), data_sources).unwrap();
            settings
        }

        fn config(&self) -> PathBuf {
            self.home.path().join("settings")
        }

        fn snapshot(&self, args: &[&str]) -> Output {
            Command::new(BINARY)
                .args(["schema", "snapshot"])
                .args(args)
                .arg("--config-dir")
                .arg(self.config())
                .current_dir(self.home.path())
                .env("HOME", self.home.path())
                .env_remove("XDG_CONFIG_HOME")
                .env_remove("XDG_DATA_HOME")
                .env_remove("DBROVER_CONFIG_DIR")
                .output()
                .unwrap()
        }
    }

    #[test]
    fn says_why_it_wrote_nothing() {
        let settings = Settings::new();
        let cases: [(&[&str], &str); 2] = [
            (
                &["warehouse", "public"],
                "There is no data source named \u{201c}warehouse\u{201d}.",
            ),
            (
                &["shop", "main", "-o", "main.schema.toml"],
                "This database's schemas can't be compared yet.",
            ),
        ];
        for (args, problem) in cases {
            let output = settings.snapshot(args);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
            assert!(stderr.contains(problem), "{args:?}: {stderr}");
            assert!(output.stdout.is_empty(), "{args:?}");
        }
        assert!(!settings.home.path().join("main.schema.toml").exists());
        assert!(!settings.home.path().join("main.schema.toml.part").exists());
    }
}
