//! `dbrover schema` as a CI job runs it: the real binary. Reading a PostgreSQL schema and
//! writing its snapshot are tested in dbrover-schema and dbrover-engine (`read_schema`, which
//! this command calls); this checks the command around them: its arguments, what it prints
//! and its exit codes.
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
