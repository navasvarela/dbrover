//! What `dbrover schema …` was asked, read strictly, as `dbrover report` reads its own: a CI
//! step is typed by hand, and a typo should fail loudly.

use std::path::{Path, PathBuf};

use crate::version::parse_config_dir;

pub const USAGE: &str = "\
Usage: dbrover schema snapshot <data source> <schema> [-o <file>] [--config-dir <folder>]
       dbrover schema compare <left> <right> [--script] [--config-dir <folder>]

snapshot writes a schema's tables, indexes and views as a TOML file that is the same, byte for
byte, each time it is taken of an unchanged schema, to commit to Git and compare with later.
It connects to the data source as the app does: its saved password or password command, and
its SSH tunnel.

  <data source>               A data source's name, as the app lists it.
  <schema>                    The schema to write, in the data source's own database.
  -o, --output <file>         Where to write it, through a file beside it renamed into
                              place. Standard output without it.
      --config-dir <folder>   The settings folder whose data sources it uses, as for the app.

compare compares two schemas' tables, indexes and views, each side a live schema or a
snapshot file, and prints what differs, or with --script the script that turns the left into
the right, to read and run by hand: it runs nothing.

  <left>, <right>             <data source>:<schema>, split at the last colon, or a snapshot
                              file: a side ending in .toml, or naming a file that exists.
      --script                Prints the script rather than the differences.

Exit codes:
  0  snapshot: written; compare: the schemas are the same
  1  compare: the schemas differ
  2  something went wrong: the command line, the settings folder, a data source, a connection,
     a schema or a snapshot file, with the reason on standard error";

/// What the command line asks of `dbrover schema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Snapshot(SnapshotArgs),
    Compare(CompareArgs),
    Help,
    /// Arguments that can't be followed, with what was wrong.
    Invalid(String),
}

/// `dbrover schema snapshot`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotArgs {
    pub data_source: String,
    pub schema: String,
    /// `None` for standard output.
    pub output: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
}

/// `dbrover schema compare`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareArgs {
    pub left: SideArg,
    pub right: SideArg,
    /// Print the script rather than the differences.
    pub script: bool,
    pub config_dir: Option<PathBuf>,
}

/// One side of a comparison, as typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideArg {
    Live { data_source: String, schema: String },
    Snapshot(PathBuf),
}

impl SideArg {
    /// A snapshot file when `arg` ends in `.toml` or names a file; `<data source>:<schema>`
    /// otherwise, split at the last colon so a data source's name may hold one.
    fn parse(arg: &str) -> Result<Self, String> {
        if arg.ends_with(".toml") || Path::new(arg).is_file() {
            return Ok(Self::Snapshot(PathBuf::from(arg)));
        }
        match arg.rsplit_once(':') {
            Some((data_source, schema)) if !data_source.is_empty() && !schema.is_empty() => {
                Ok(Self::Live {
                    data_source: data_source.to_owned(),
                    schema: schema.to_owned(),
                })
            }
            _ => Err(format!(
                "{arg:?} is neither <data source>:<schema> nor a snapshot file"
            )),
        }
    }
}

impl Request {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Self {
        let mut args = args.into_iter();
        match args.next().as_deref() {
            Some("snapshot") => match SnapshotArgs::parse(args) {
                Ok(Some(snapshot)) => Self::Snapshot(snapshot),
                Ok(None) => Self::Help,
                Err(problem) => Self::Invalid(problem),
            },
            Some("compare") => match CompareArgs::parse(args) {
                Ok(Some(compare)) => Self::Compare(compare),
                Ok(None) => Self::Help,
                Err(problem) => Self::Invalid(problem),
            },
            Some("--help" | "-h") => Self::Help,
            Some(other) => Self::Invalid(format!("unknown command {other:?}")),
            None => Self::Invalid("dbrover schema needs a command: snapshot or compare".to_owned()),
        }
    }
}

impl SnapshotArgs {
    /// `None` when `--help` was asked for.
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = args.peekable();
        let mut positional = Vec::new();
        let mut output = None;
        let mut config_dir = None;
        while let Some(arg) = args.next() {
            if let Some(folder) = parse_config_dir(&arg, &mut args) {
                config_dir = Some(folder?);
                continue;
            }
            match arg.as_str() {
                "--help" | "-h" => return Ok(None),
                "--output" | "-o" => {
                    let file = args
                        .next()
                        .filter(|file| !file.is_empty())
                        .ok_or("--output needs a file")?;
                    output = Some(PathBuf::from(file));
                }
                option if option.starts_with('-') && option.len() > 1 => {
                    return Err(format!("unknown argument {option:?}"));
                }
                value => positional.push(value.to_owned()),
            }
        }
        let mut positional = positional.into_iter();
        let (Some(data_source), Some(schema), None) =
            (positional.next(), positional.next(), positional.next())
        else {
            return Err("dbrover schema snapshot needs a data source and a schema".to_owned());
        };
        Ok(Some(Self {
            data_source,
            schema,
            output,
            config_dir,
        }))
    }
}

impl CompareArgs {
    /// `None` when `--help` was asked for.
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = args.peekable();
        let mut sides = Vec::new();
        let mut script = false;
        let mut config_dir = None;
        while let Some(arg) = args.next() {
            if let Some(folder) = parse_config_dir(&arg, &mut args) {
                config_dir = Some(folder?);
                continue;
            }
            match arg.as_str() {
                "--help" | "-h" => return Ok(None),
                "--script" => script = true,
                option if option.starts_with('-') && option.len() > 1 => {
                    return Err(format!("unknown argument {option:?}"));
                }
                side => sides.push(side.to_owned()),
            }
        }
        let [left, right] = sides.as_slice() else {
            return Err("dbrover schema compare needs two sides, a left and a right".to_owned());
        };
        Ok(Some(Self {
            left: SideArg::parse(left)?,
            right: SideArg::parse(right)?,
            script,
            config_dir,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Request {
        Request::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn reads_a_snapshot_request() {
        assert_eq!(
            parse(&["snapshot", "shop", "public", "-o", "public.schema.toml"]),
            Request::Snapshot(SnapshotArgs {
                data_source: "shop".to_owned(),
                schema: "public".to_owned(),
                output: Some(PathBuf::from("public.schema.toml")),
                config_dir: None,
            })
        );
        assert_eq!(
            parse(&["snapshot", "--config-dir=/tmp/s", "shop db", "public"]),
            Request::Snapshot(SnapshotArgs {
                data_source: "shop db".to_owned(),
                schema: "public".to_owned(),
                output: None,
                config_dir: Some(PathBuf::from("/tmp/s")),
            })
        );
        assert_eq!(parse(&["snapshot", "--help"]), Request::Help);
        assert_eq!(parse(&["--help"]), Request::Help);
    }

    #[test]
    fn reads_a_compare_request() {
        assert_eq!(
            parse(&[
                "compare",
                "shop:db:public",
                "public.schema.toml",
                "--script"
            ]),
            Request::Compare(CompareArgs {
                left: SideArg::Live {
                    data_source: "shop:db".to_owned(),
                    schema: "public".to_owned(),
                },
                right: SideArg::Snapshot(PathBuf::from("public.schema.toml")),
                script: true,
                config_dir: None,
            })
        );
    }

    #[test]
    fn refuses_what_it_cant_follow() {
        let cases: [(&[&str], &str); 8] = [
            (&[], "dbrover schema needs a command: snapshot or compare"),
            (&["compare", "shop:public"], "needs two sides"),
            (
                &["compare", "shop", "shop:public"],
                "neither <data source>:<schema>",
            ),
            (
                &["compare", "shop:", "shop:public"],
                "neither <data source>:<schema>",
            ),
            (&["snap"], "unknown command \"snap\""),
            (&["snapshot", "shop"], "needs a data source and a schema"),
            (
                &["snapshot", "shop", "public", "extra"],
                "needs a data source and a schema",
            ),
            (
                &["snapshot", "shop", "public", "--out", "x"],
                "unknown argument \"--out\"",
            ),
        ];
        for (args, problem) in cases {
            match parse(args) {
                Request::Invalid(message) => {
                    assert!(message.contains(problem), "{args:?}: {message}")
                }
                other => panic!("{args:?}: expected a refusal, got {other:?}"),
            }
        }
    }
}
