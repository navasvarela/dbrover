//! `dbrover report run` as cron or a CI job runs it: the real binary, on a settings folder with
//! the examples' SQLite shop as the data source `shop`. The run itself is tested in
//! dbrover-report, dbrover-engine and the worker's tests; this checks the command around it:
//! its arguments, what it prints, the files it leaves and the exit code for each outcome.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

const BINARY: &str = env!("CARGO_BIN_EXE_dbrover");

/// The command line's own mistakes: nothing is read, so this runs on every platform, Windows'
/// console included.
#[test]
fn refuses_a_command_line_it_does_not_understand() {
    let cases: [(&[&str], &str); 4] = [
        (&["report"], "dbrover report needs a command: run"),
        (&["report", "run"], "dbrover report run needs a report"),
        (
            &["report", "run", "sales", "--parm", "since=2026-10-01"],
            "unknown argument \"--parm\"",
        ),
        (
            &["report", "run", "sales", "-p", "since"],
            "needs the form <name>=<value>",
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
        .args(["report", "run", "--help"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: dbrover report run <report>"),
        "{stdout}"
    );
}

/// Unix only, as the worker's tests: the home folder is moved by setting `HOME`, which Windows
/// ignores when it looks up the user's folders, and the tests must not use the real one's.
#[cfg(unix)]
mod runs {
    use std::path::{Path, PathBuf};
    use std::process::Output;

    use dbrover_store::{HistoryFile, ReportName, ReportRun, RunOutcome, StepMetrics};

    use super::*;

    const OCTOBER: [&str; 4] = ["-p", "since=2026-10-01", "-p", "until=2026-11-01"];

    /// A home folder, its settings folder with the examples' shop as the data source `shop`,
    /// and `sales-by-category.toml` in its reports.
    struct Settings {
        home: tempfile::TempDir,
    }

    impl Settings {
        /// `report`'s runs in the settings folder's history, newest first.
        fn runs(&self, report: &str) -> Vec<ReportRun> {
            let name = ReportName::new(report).unwrap();
            HistoryFile::new(self.config().join("history.sqlite"))
                .open()
                .unwrap()
                .runs(&name, 10)
                .unwrap()
        }

        fn new() -> Self {
            let settings = Self {
                home: tempfile::tempdir().unwrap(),
            };
            let database = settings.home.path().join("shop.db");
            let script = std::fs::read_to_string(examples().join("shop.sql")).unwrap();
            rusqlite::Connection::open(&database)
                .unwrap()
                .execute_batch(&script)
                .unwrap();
            std::fs::create_dir_all(settings.config()).unwrap();
            let data_sources = format!(
                "[[data_source]]\nid = \"6f1c1a3e-6a43-4b8e-9d55-0d6c2b7e1a01\"\nname = \"shop\"\n\
                 driver = \"sqlite\"\npath = {:?}\n",
                database.display().to_string()
            );
            std::fs::write(settings.config().join("data_sources.toml"), data_sources).unwrap();
            let sales = std::fs::read_to_string(examples().join("sales-by-category.toml")).unwrap();
            settings.write_report("sales-by-category", &sales);
            settings
        }

        fn config(&self) -> PathBuf {
            self.home.path().join("settings")
        }

        fn out(&self) -> PathBuf {
            self.home.path().join("out")
        }

        fn write_report(&self, name: &str, text: &str) {
            let path = self.config().join("reports").join(format!("{name}.toml"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }

        /// `dbrover report run` on this settings folder, from the home folder.
        fn command(&self, args: &[&str]) -> Command {
            self.subcommand("run", args)
        }

        /// `dbrover report <subcommand>` on this settings folder, from the home folder.
        fn subcommand(&self, subcommand: &str, args: &[&str]) -> Command {
            let mut command = Command::new(BINARY);
            command
                .arg("report")
                .arg(subcommand)
                .args(args)
                .arg("--config-dir")
                .arg(self.config())
                .current_dir(self.home.path())
                .env("HOME", self.home.path())
                .env_remove("XDG_CONFIG_HOME")
                .env_remove("XDG_DATA_HOME")
                .env_remove("DBROVER_CONFIG_DIR");
            command
        }

        /// Runs the command with its files going to `out`.
        fn run(&self, args: &[&str]) -> Output {
            let out = self.out().display().to_string();
            let args: Vec<&str> = args.iter().copied().chain(["-o", &out]).collect();
            self.command(&args).output().unwrap()
        }
    }

    fn examples() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/reports")
    }

    fn files_in(folder: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(folder) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn describe(output: &Output) -> String {
        format!(
            "exit {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    /// Whether `file` in `out` is the one checked in under the examples' `expected/`.
    fn assert_matches_expected(out: &Path, file: &str) {
        let written = std::fs::read_to_string(out.join(file)).unwrap();
        let expected = std::fs::read_to_string(examples().join("expected").join(file)).unwrap();
        // Git may have checked the file out with the platform's line endings; the writer's
        // are CSV's own, CRLF.
        let expected = expected.replace("\r\n", "\n").replace('\n', "\r\n");
        assert!(
            written == expected,
            "{file} differs from the checked-in one"
        );
    }

    #[test]
    fn runs_a_report_by_name_and_lists_its_files() {
        let settings = Settings::new();
        let output = settings.run(&[&["sales-by-category"][..], &OCTOBER].concat());
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert_eq!(
            files_in(&settings.out()),
            [
                "sales-by-category-2026-10.csv",
                "sales-by-category-2026-10.xlsx"
            ]
        );
        assert_matches_expected(&settings.out(), "sales-by-category-2026-10.csv");
        let listed: Vec<PathBuf> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            listed,
            [
                settings.out().join("sales-by-category-2026-10.xlsx"),
                settings.out().join("sales-by-category-2026-10.csv"),
            ]
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        let lines: Vec<&str> = stderr.lines().collect();
        assert_eq!(
            lines.first(),
            Some(&"Sales by category (since = 2026-10-01, until = 2026-11-01)"),
            "{stderr}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("Source lines: 246 rows"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("Combine: 13 rows"))
        );
        assert!(
            lines.last().unwrap().starts_with("Finished in "),
            "{stderr}"
        );
    }

    /// A report file outside the settings folder, given by its path, into the current folder.
    #[test]
    fn runs_a_report_file_by_its_path_into_the_current_folder() {
        let settings = Settings::new();
        std::fs::create_dir_all(settings.out()).unwrap();
        let report = examples().join("sales-by-category.toml");
        let report = report.to_str().unwrap();
        let output = settings
            .command(&[&[report][..], &OCTOBER].concat())
            .current_dir(settings.out())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert_matches_expected(&settings.out(), "sales-by-category-2026-10.csv");
    }

    /// The PRQL example matches its checked-in file, and says the SQL it compiled to in its
    /// events.
    #[test]
    fn runs_the_prql_example() {
        let settings = Settings::new();
        let report = examples().join("sales-by-week.toml");
        let report = report.to_str().unwrap();
        let output = settings.run(&[&[report, "--json"][..], &OCTOBER].concat());
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert_matches_expected(&settings.out(), "sales-by-week-2026-10.csv");
        let started: serde_json::Value = serde_json::from_str(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        let sql = started["combine_sql"].as_str().unwrap();
        assert!(sql.contains("date_trunc('week', ordered_on)"), "{sql}");
        assert!(sql.contains(":since"), "{sql}");
    }

    /// One row of products, for the reports below.
    fn one_source(report: &str, source: &str) -> String {
        format!(
            "[report]\nname = \"Products\"\n{report}\n\n[[source]]\nid = \"products\"\n\
             data_source = \"shop\"\n{source}\n\n[[output]]\nformat = \"csv\"\n\
             file = \"products.csv\"\n"
        )
    }

    /// A source that counts for a long time before returning its one row.
    const SLOW_QUERY: &str = "query = \"\"\"\nWITH RECURSIVE n(x) AS \
        (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 5000000000) SELECT max(x) FROM n\n\"\"\"";

    /// Each way a run can end, its exit code and what it says, and that nothing is left in
    /// the output folder but a finished run's files.
    #[test]
    fn exits_with_a_code_for_each_outcome() {
        struct Case {
            what: &'static str,
            report: String,
            args: Vec<&'static str>,
            code: i32,
            says: &'static str,
        }
        let sales = |params: &[&'static str]| {
            let mut args = vec!["sales-by-category"];
            args.extend_from_slice(params);
            args
        };
        let cases = [
            Case {
                what: "a parameter missing",
                report: String::new(),
                args: sales(&["-p", "since=2026-10-01"]),
                code: 3,
                says: "until needs a value",
            },
            Case {
                what: "a parameter the report doesn't take",
                report: String::new(),
                args: sales(&[&OCTOBER[..], &["-p", "sinse=2026-10-01"]].concat()),
                code: 3,
                says: "the report takes no parameter \"sinse\"; it takes since, until",
            },
            Case {
                what: "a value of the wrong type",
                report: String::new(),
                args: sales(&["-p", "since=October", "-p", "until=2026-11-01"]),
                code: 3,
                says: "since: \"October\"",
            },
            Case {
                what: "no such report",
                report: String::new(),
                args: vec!["sales"],
                code: 3,
                says: "sales.toml",
            },
            Case {
                what: "a data source that doesn't exist",
                report: one_source("", "query = \"SELECT 1\"")
                    .replace("data_source = \"shop\"", "data_source = \"nowhere\""),
                args: vec!["products"],
                code: 3,
                says: "nowhere",
            },
            Case {
                what: "a source's query failing",
                report: one_source("", "query = \"SELECT * FROM no_such_table\""),
                args: vec!["products"],
                code: 4,
                says: "no_such_table",
            },
            Case {
                what: "a source returning more than its max_rows",
                report: one_source("", "query = \"SELECT * FROM products\"\nmax_rows = 2"),
                args: vec!["products"],
                code: 5,
                says: "It stopped at max_rows = 2 rows, set in the [[source]] \"products\" of the \
                       report's file.",
            },
            Case {
                what: "the run passing its time limit",
                report: one_source("timeout = \"2s\"", SLOW_QUERY),
                args: vec!["products"],
                code: 6,
                says: "time limit of 2s",
            },
        ];
        for case in cases {
            let settings = Settings::new();
            if !case.report.is_empty() {
                settings.write_report("products", &case.report);
            }
            let output = settings.run(&case.args);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(case.code),
                "{}: {}",
                case.what,
                describe(&output)
            );
            assert!(stderr.contains(case.says), "{}: {stderr}", case.what);
            assert!(output.stdout.is_empty(), "{}", case.what);
            assert_eq!(
                files_in(&settings.out()),
                Vec::<String>::new(),
                "{}",
                case.what
            );
        }
    }

    /// On Linux, a command killed outright takes its worker with it: the worker asked the
    /// kernel for SIGKILL when its parent dies.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_command_killed_outright_takes_its_worker() {
        use std::io::{BufRead, BufReader};
        use std::process::Stdio;
        use std::time::{Duration, Instant};

        /// The pids of `parent`'s children, from each process's `stat` in `/proc`.
        fn children(parent: u32) -> Vec<u32> {
            let parent = parent.to_string();
            std::fs::read_dir("/proc")
                .unwrap()
                .filter_map(|entry| {
                    let pid: u32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
                    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                    // The command's name, in brackets, can hold spaces: fields follow its `)`.
                    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
                    (fields.get(1) == Some(&parent.as_str())).then_some(pid)
                })
                .collect()
        }

        /// Whether `pid` is still a process, not one that has died and awaits its reaping.
        fn alive(pid: u32) -> bool {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    let state = stat
                        .rsplit_once(')')?
                        .1
                        .split_whitespace()
                        .next()?
                        .to_owned();
                    Some(state != "Z" && state != "X")
                })
                .unwrap_or(false)
        }

        let settings = Settings::new();
        settings.write_report("products", &one_source("", SLOW_QUERY));
        let out = settings.out().display().to_string();
        let mut child = settings
            .command(&["products", "-o", &out])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let mut started = String::new();
        stderr.read_line(&mut started).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let worker = loop {
            if let [worker] = children(child.id()).as_slice() {
                break *worker;
            }
            assert!(Instant::now() < deadline, "the worker never started");
            std::thread::sleep(Duration::from_millis(50));
        };

        child.kill().unwrap();
        child.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while alive(worker) {
            assert!(
                Instant::now() < deadline,
                "the worker {worker} outlived its command"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `--progress` keeps a live line only on a terminal: through a pipe, as here, the lines
    /// are those of a run without it.
    #[test]
    fn progress_adds_nothing_through_a_pipe() {
        let settings = Settings::new();
        let args = |progress: bool| {
            let mut args = vec!["sales-by-category"];
            args.extend_from_slice(&OCTOBER);
            if progress {
                args.push("--progress");
            }
            args
        };
        let without = settings.run(&args(false));
        let with = settings.run(&args(true));
        assert_eq!(with.status.code(), Some(0), "{}", describe(&with));
        let lines = |output: &Output| {
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .map(|line| line.split(" in ").next().unwrap_or(line).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(lines(&with), lines(&without));
        assert!(!String::from_utf8_lossy(&with.stderr).contains('\r'));
    }

    /// `report list` names each report, the one with problems among them.
    #[test]
    fn lists_the_reports_and_what_keeps_one_from_running() {
        let settings = Settings::new();
        settings.write_report(
            "broken",
            &one_source("", "query = \"SELECT 1\"")
                .replace("data_source = \"shop\"", "data_source = \"nowhere\""),
        );
        let output = settings.subcommand("list", &[]).output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stdout.lines().collect();
        assert!(
            lines[0].starts_with("broken  ") && lines[0].ends_with("can't run, 1 problem:"),
            "{stdout}"
        );
        assert!(lines[1].contains("nowhere"), "{stdout}");
        assert_eq!(
            lines[2],
            "sales-by-category  Sales by category (since: date, until: date)"
        );

        let json = settings.subcommand("list", &["--json"]).output().unwrap();
        let reports: Vec<serde_json::Value> = String::from_utf8_lossy(&json.stdout)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[1]["name"], "sales-by-category");
        assert_eq!(reports[1]["problems"].as_array().unwrap().len(), 0);
    }

    /// A limit the file leaves to Settings › Reports says so, and where to set it instead.
    #[test]
    fn says_a_limit_came_from_settings() {
        let settings = Settings::new();
        std::fs::write(
            settings.config().join("settings.toml"),
            "[reports]\nmax_rows = 2\n",
        )
        .unwrap();
        settings.write_report(
            "products",
            &one_source("", "query = \"SELECT * FROM products\""),
        );
        let output = settings.run(&["products"]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(5), "{}", describe(&output));
        assert!(
            stderr.contains(
                "It stopped at max_rows = 2 rows, the default in Settings › Reports, since the \
                 report's file doesn't set it."
            ),
            "{stderr}"
        );
    }

    /// What a terminal's Ctrl-C, cron's or a CI runner's stop, and a closed terminal send:
    /// each cancels the run, leaves the output folder as it was, and exits with 6. The
    /// interrupt goes to the command's whole process group, as a terminal sends it.
    #[test]
    fn a_stop_signal_cancels_the_run() {
        use std::io::{BufRead, BufReader, Read};
        use std::os::unix::process::CommandExt as _;
        use std::process::Stdio;
        use std::time::{Duration, Instant};

        /// Far longer than a cancelled run takes to end; a run that isn't cancelled counts
        /// for minutes.
        const EXIT_WAIT: Duration = Duration::from_secs(60);

        let cases = [("INT", true), ("TERM", false), ("HUP", false)];
        for (signal, to_group) in cases {
            let settings = Settings::new();
            settings.write_report("products", &one_source("", SLOW_QUERY));
            let out = settings.out().display().to_string();
            let mut child = settings
                .command(&["products", "-o", &out])
                .process_group(0)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stderr = BufReader::new(child.stderr.take().unwrap());
            let mut started = String::new();
            stderr.read_line(&mut started).unwrap();
            assert_eq!(started, "Products\n", "{signal}");

            let pid = child.id().to_string();
            let target = if to_group { format!("-{pid}") } else { pid };
            let sent = Command::new("kill")
                .args([&format!("-{signal}"), "--", &target])
                .status()
                .unwrap();
            assert!(sent.success(), "{signal}");

            let deadline = Instant::now() + EXIT_WAIT;
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() > deadline {
                    child.kill().unwrap();
                    panic!("SIG{signal} didn't stop the run within {EXIT_WAIT:?}");
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            let mut rest = String::new();
            stderr.read_to_string(&mut rest).unwrap();
            assert_eq!(status.code(), Some(6), "SIG{signal}: {rest}");
            assert_eq!(rest, "Cancelled. No file was written.\n", "SIG{signal}");
            let runs = settings.runs("products");
            assert_eq!(runs.len(), 1, "SIG{signal}");
            assert_eq!(runs[0].outcome, RunOutcome::Cancelled, "SIG{signal}");
            assert_eq!(
                files_in(&settings.out()),
                Vec::<String>::new(),
                "SIG{signal}"
            );
        }
    }

    /// With `--json`, standard output is the worker's events, which read back as the app reads
    /// them, ending with how the run ended, under the exit code it has without `--json`.
    #[test]
    fn prints_the_runs_events_as_json_lines() {
        use dbrover_report::{RunEvent, Step};

        let products = one_source("", "query = \"SELECT * FROM products\"\nmax_rows = 2");
        let cases: [(Vec<&str>, i32); 3] = [
            ([&["sales-by-category", "--json"][..], &OCTOBER].concat(), 0),
            (
                vec!["sales-by-category", "--json", "-p", "since=2026-10-01"],
                3,
            ),
            (vec!["products", "--json"], 5),
        ];
        for (args, code) in cases {
            let settings = Settings::new();
            settings.write_report("products", &products);
            let output = settings.run(&args);
            assert_eq!(output.status.code(), Some(code), "{}", describe(&output));
            assert!(output.stderr.is_empty(), "{}", describe(&output));
            let events: Vec<RunEvent> = String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let last = events.last().unwrap();
            assert!(last.ends_run(), "{args:?}: {events:#?}");
            match (code, last) {
                (0, RunEvent::Finished { .. }) => {
                    let written = events
                        .iter()
                        .filter(|event| matches!(event, RunEvent::OutputWritten(_)))
                        .count();
                    assert_eq!(written, 2);
                    assert!(matches!(events.first(), Some(RunEvent::Started { .. })));
                }
                (3, RunEvent::Failed(failure)) => {
                    assert_eq!(events.len(), 1);
                    assert_eq!(failure.step, Step::Report);
                    assert!(failure.message.contains("until needs a value"));
                }
                (5, RunEvent::Failed(failure)) => {
                    let key = failure.limit.as_ref().map(|limit| limit.key.as_str());
                    assert_eq!(key, Some("max_rows"));
                }
                _ => panic!("{args:?} ended with {last:?}"),
            }
        }
    }

    /// A run of a report in `reports/` is kept as the app keeps its runs, whichever way it
    /// ends; one that couldn't start, or of a file elsewhere, isn't.
    #[test]
    fn keeps_a_run_of_a_report_in_the_reports_folder() {
        let settings = Settings::new();
        let products = one_source("", "query = \"SELECT * FROM products\"\nmax_rows = 2");
        settings.write_report("products", &products);

        let finished = settings.run(&[&["sales-by-category"][..], &OCTOBER].concat());
        assert_eq!(finished.status.code(), Some(0), "{}", describe(&finished));
        let runs = settings.runs("sales-by-category");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].outcome, RunOutcome::Finished);
        assert_eq!(runs[0].origin, dbrover_store::RunOrigin::Command);
        assert_eq!(
            runs[0].params.get("since").map(String::as_str),
            Some("2026-10-01")
        );
        let steps: Vec<&str> = runs[0]
            .steps
            .iter()
            .map(|step| match step {
                StepMetrics::Source(_) => "source",
                StepMetrics::Attached { .. } => "attached",
                StepMetrics::Combine(_) => "combine",
                StepMetrics::Output(_) => "output",
            })
            .collect();
        assert_eq!(steps, ["source", "combine", "output", "output"]);

        let failed = settings.run(&["products"]);
        assert_eq!(failed.status.code(), Some(5), "{}", describe(&failed));
        let runs = settings.runs("products");
        assert_eq!(runs.len(), 1);
        assert!(
            matches!(&runs[0].outcome, RunOutcome::Failed(error) if error.message.contains("max_rows"))
        );

        let not_started = settings.run(&["sales-by-category", "-p", "since=2026-10-01"]);
        assert_eq!(
            not_started.status.code(),
            Some(3),
            "{}",
            describe(&not_started)
        );
        let elsewhere = examples().join("sales-by-category.toml");
        let elsewhere = settings.run(&[&[elsewhere.to_str().unwrap()][..], &OCTOBER].concat());
        assert_eq!(elsewhere.status.code(), Some(0), "{}", describe(&elsewhere));
        assert_eq!(settings.runs("sales-by-category").len(), 1);
    }
}
