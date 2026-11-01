//! Which build this is, and the few command-line flags DbRover understands.

use std::path::PathBuf;

use dbrover_store::CONFIG_DIR_FLAG;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Set by `build.rs`: the commit the binary was built from, or "unknown".
pub const COMMIT: &str = env!("DBROVER_COMMIT");

/// `dbrover 0.1.0 (1a2b3c4d5e)`: what `--version` prints and bug reports should quote.
pub fn describe() -> String {
    format!("dbrover {VERSION} ({COMMIT})")
}

const USAGE: &str = "\
Usage: dbrover [--config-dir <folder>] [--version | --help] [FILE.sql …]
       dbrover mcp --project <slug> [--config-dir <folder>]

Starts the DbRover desktop app, opening each FILE.sql given in a console. With --version (-V),
prints the version and exits.

`dbrover mcp --project <slug>` runs an MCP server for the project with that slug, read-only,
over standard input and output, without opening a window. An MCP client starts it; see
\"Connecting an AI assistant\" in the manual.

--config-dir <folder> keeps the settings in that folder instead of ~/.config/dbrover or the
folder chosen in Settings, as the DBROVER_CONFIG_DIR environment variable does. The folder
must be an absolute path or start with ~.";

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliRequest {
    Version,
    Help,
    /// Serve a project to an MCP client, headless.
    Mcp {
        project: String,
        config_dir: Option<PathBuf>,
    },
    /// Arguments that can't be followed, with what was wrong.
    Invalid(String),
    /// Open the app, and in it the files named. Options DbRover doesn't know are ignored,
    /// because launchers add their own: older macOS passes `-psn_…` to apps started from
    /// Finder.
    Run {
        files: Vec<PathBuf>,
        config_dir: Option<PathBuf>,
    },
}

impl CliRequest {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Self {
        let mut args = args.into_iter().peekable();
        if args.peek().is_some_and(|first| first == "mcp") {
            return Self::parse_mcp(args.skip(1));
        }
        let mut help = false;
        let mut files = Vec::new();
        let mut config_dir = None;
        while let Some(arg) = args.next() {
            match parse_config_dir(&arg, &mut args) {
                Some(Ok(folder)) => {
                    config_dir = Some(folder);
                    continue;
                }
                Some(Err(problem)) => return Self::Invalid(problem),
                None => {}
            }
            match arg.as_str() {
                "--version" | "-V" => return Self::Version,
                "--help" | "-h" => help = true,
                option if option.starts_with('-') => {}
                file => files.push(PathBuf::from(file)),
            }
        }
        if help {
            Self::Help
        } else {
            Self::Run { files, config_dir }
        }
    }

    /// Unlike the app, the subcommand refuses arguments it doesn't know: an MCP client's
    /// configuration is typed by hand, and a typo should fail loudly, not be ignored.
    fn parse_mcp(args: impl Iterator<Item = String>) -> Self {
        let mut args = args.peekable();
        let mut project = None;
        let mut config_dir = None;
        while let Some(arg) = args.next() {
            match parse_config_dir(&arg, &mut args) {
                Some(Ok(folder)) => {
                    config_dir = Some(folder);
                    continue;
                }
                Some(Err(problem)) => return Self::Invalid(problem),
                None => {}
            }
            match arg.as_str() {
                "--project" | "-p" => match args.next() {
                    Some(slug) => project = Some(slug),
                    None => return Self::Invalid("--project needs a project's slug".to_owned()),
                },
                "--help" | "-h" => return Self::Help,
                "--version" | "-V" => return Self::Version,
                other => match other.strip_prefix("--project=") {
                    Some(slug) => project = Some(slug.to_owned()),
                    None => return Self::Invalid(format!("unknown argument {other:?}")),
                },
            }
        }
        match project {
            Some(project) if !project.is_empty() => Self::Mcp {
                project,
                config_dir,
            },
            _ => Self::Invalid("dbrover mcp needs --project <slug>".to_owned()),
        }
    }

    /// Prints what was asked for, if it is only something to print. Returns `true` if
    /// nothing else should happen.
    pub fn answer(&self) -> bool {
        match self {
            Self::Version => println!("{}", describe()),
            Self::Help => println!("{}\n\n{USAGE}", describe()),
            Self::Mcp { .. } | Self::Invalid(_) | Self::Run { .. } => return false,
        }
        true
    }

    pub fn usage() -> &'static str {
        USAGE
    }
}

/// The folder `--config-dir <folder>` or `--config-dir=<folder>` names, if `arg` is the flag,
/// taking the folder from `rest` in the first form. It is checked later, with the variable.
fn parse_config_dir(
    arg: &str,
    rest: &mut impl Iterator<Item = String>,
) -> Option<Result<PathBuf, String>> {
    if arg == CONFIG_DIR_FLAG {
        return Some(
            rest.next()
                .filter(|folder| !folder.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| format!("{CONFIG_DIR_FLAG} needs a folder")),
        );
    }
    let folder = arg.strip_prefix(CONFIG_DIR_FLAG)?.strip_prefix('=')?;
    Some(if folder.is_empty() {
        Err(format!("{CONFIG_DIR_FLAG} needs a folder"))
    } else {
        Ok(PathBuf::from(folder))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_mcp_subcommand_strictly() {
        let mcp = |project: &str| CliRequest::Mcp {
            project: project.to_owned(),
            config_dir: None,
        };
        let cases: [(&[&str], CliRequest); 11] = [
            (&["mcp", "--project", "analytics"], mcp("analytics")),
            (&["mcp", "--project=shop-eu"], mcp("shop-eu")),
            (&["mcp", "-p", "shop"], mcp("shop")),
            (
                &["mcp", "--config-dir", "~/work", "--project", "shop"],
                CliRequest::Mcp {
                    project: "shop".to_owned(),
                    config_dir: Some("~/work".into()),
                },
            ),
            (
                &["mcp", "--project", "shop", "--config-dir=/srv/dbrover"],
                CliRequest::Mcp {
                    project: "shop".to_owned(),
                    config_dir: Some("/srv/dbrover".into()),
                },
            ),
            (
                &["mcp", "--project", "shop", "--config-dir"],
                CliRequest::Invalid("--config-dir needs a folder".to_owned()),
            ),
            (&["mcp", "--help"], CliRequest::Help),
            (
                &["mcp"],
                CliRequest::Invalid("dbrover mcp needs --project <slug>".to_owned()),
            ),
            (
                &["mcp", "--project"],
                CliRequest::Invalid("--project needs a project's slug".to_owned()),
            ),
            (
                &["mcp", "--projcet", "shop"],
                CliRequest::Invalid("unknown argument \"--projcet\"".to_owned()),
            ),
            // Only as the first argument; elsewhere it names a file to open.
            (
                &["-psn_0_1", "mcp"],
                CliRequest::Run {
                    files: vec!["mcp".into()],
                    config_dir: None,
                },
            ),
        ];
        for (args, expected) in cases {
            let parsed = CliRequest::parse(args.iter().map(|arg| (*arg).to_owned()));
            assert_eq!(parsed, expected, "{args:?}");
        }
    }

    #[test]
    fn version_wins_files_are_opened_and_unknown_options_ignored() {
        let run = |files: &[&str]| CliRequest::Run {
            files: files.iter().map(PathBuf::from).collect(),
            config_dir: None,
        };
        let cases: [(&[&str], CliRequest); 9] = [
            (&[], run(&[])),
            (&["-psn_0_12345"], run(&[])),
            (
                &["-psn_0_1", "a.sql", "../b.sql"],
                run(&["a.sql", "../b.sql"]),
            ),
            (&["--version"], CliRequest::Version),
            (&["--help", "-V"], CliRequest::Version),
            (&["-h"], CliRequest::Help),
            (
                &["--config-dir", "/srv/dbrover", "a.sql"],
                CliRequest::Run {
                    files: vec!["a.sql".into()],
                    config_dir: Some("/srv/dbrover".into()),
                },
            ),
            (
                &["a.sql", "--config-dir=~/work"],
                CliRequest::Run {
                    files: vec!["a.sql".into()],
                    config_dir: Some("~/work".into()),
                },
            ),
            (
                &["--config-dir="],
                CliRequest::Invalid("--config-dir needs a folder".to_owned()),
            ),
        ];
        for (args, expected) in cases {
            let parsed = CliRequest::parse(args.iter().map(|arg| (*arg).to_owned()));
            assert_eq!(parsed, expected, "{args:?}");
        }
    }

    #[test]
    fn describes_the_version_and_commit() {
        let description = describe();
        assert!(description.starts_with(&format!("dbrover {VERSION} (")));
        assert!(!COMMIT.is_empty());
    }
}
