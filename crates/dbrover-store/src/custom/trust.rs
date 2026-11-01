//! `trusted_commands.toml`: the commands the user chose to run, command actions', data
//! sources' password commands (W9) and export programs (W8d), each by its exact definition, so
//! a command that changes asks again. See `docs/hackability.md`.
//!
//! It lives in the data folder, not beside `actions.toml`: a config folder synced from another
//! machine, or copied from a colleague, brings its actions but none of this file's trust. An
//! imported data source likewise brings its password command but not the trust to run it.

use std::path::{Path, PathBuf};
use std::{fs, io};

use dbrover_core::{DataSourceId, PasswordCommand};
use dbrover_export::{CustomFormatId, ProgramFormat};
use serde::{Deserialize, Serialize};

use super::actions::ActionId;
use super::commands::{CommandAction, StdinFormat};
use crate::atomic::write_atomically;

const HEADER: &str = "\
# Commands DbRover runs without asking first, each as it was when you chose Run: command
# actions from actions.toml, data sources' password commands, and programs from exports.toml.
# DbRover writes this file; remove an entry to be asked again.

";

/// What the user saw and agreed to run: whose command it is, and the command as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedCommand {
    #[serde(flatten)]
    owner: Owner,
    command: Vec<String>,
}

/// Whose command was trusted. Untagged, so each is told apart by its own keys, and the entries
/// written before password commands existed read as actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum Owner {
    /// A command action, with what it reads on stdin. Where its output goes isn't part of it,
    /// since that can't do harm.
    Action { id: String, stdin: StdinFormat },
    /// A data source's password command, which reads nothing.
    DataSource { data_source: DataSourceId },
    /// A program from `exports.toml`, which reads rows as JSON Lines.
    Export { export: CustomFormatId },
}

impl Owner {
    /// Whether both name the same action or data source, whatever else they say.
    fn is(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Action { id, .. }, Self::Action { id: other, .. }) => id == other,
            (Self::DataSource { data_source }, Self::DataSource { data_source: other }) => {
                data_source == other
            }
            (Self::Export { export }, Self::Export { export: other }) => export == other,
            (Self::Action { .. }, _) | (Self::DataSource { .. }, _) | (Self::Export { .. }, _) => {
                false
            }
        }
    }
}

impl TrustedCommand {
    pub fn of(id: &ActionId, action: &CommandAction) -> Self {
        Self {
            owner: Owner::Action {
                id: id.as_str().to_owned(),
                stdin: action.stdin,
            },
            command: action.command.source().to_vec(),
        }
    }

    pub fn export(id: &CustomFormatId, program: &ProgramFormat) -> Self {
        Self {
            owner: Owner::Export { export: id.clone() },
            command: program.command.source().to_vec(),
        }
    }

    pub fn password(data_source: DataSourceId, command: &PasswordCommand) -> Self {
        Self {
            owner: Owner::DataSource { data_source },
            command: command.source().to_vec(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedCommands(Vec<TrustedCommand>);

impl TrustedCommands {
    pub fn contains(&self, command: &TrustedCommand) -> bool {
        self.0.contains(command)
    }

    /// Trusts `command` from now on, in place of whatever its action or data source was
    /// trusted to run before, which no longer exists in that form.
    pub fn trust(&mut self, command: TrustedCommand) {
        self.0.retain(|trusted| !trusted.owner.is(&command.owner));
        self.0.push(command);
    }
}

#[derive(Serialize, Deserialize)]
struct TrustFile {
    #[serde(default, rename = "command")]
    commands: Vec<TrustedCommand>,
}

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not a list of trusted commands", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not serialize the trusted commands")]
    Serialize(#[source] toml::ser::Error),
    #[error("could not write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Where the trusted commands are kept.
#[derive(Debug, Clone)]
pub struct TrustedCommandsFile {
    path: PathBuf,
}

impl TrustedCommandsFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// No commands if there is no file yet.
    ///
    /// # Errors
    ///
    /// [`TrustError`] if the file exists but can't be read or isn't in this format.
    pub fn load(&self) -> Result<TrustedCommands, TrustError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(TrustedCommands::default());
            }
            Err(source) => {
                return Err(TrustError::Read {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let file: TrustFile = toml::from_str(&text).map_err(|source| TrustError::Parse {
            path: self.path.clone(),
            source,
        })?;
        Ok(TrustedCommands(file.commands))
    }

    /// Replaces the file, atomically, creating its folder.
    ///
    /// # Errors
    ///
    /// [`TrustError`] if the file can't be written.
    pub fn save(&self, commands: &TrustedCommands) -> Result<(), TrustError> {
        let file = TrustFile {
            commands: commands.0.clone(),
        };
        let text = toml::to_string(&file).map_err(TrustError::Serialize)?;
        let failed = |source| TrustError::Write {
            path: self.path.clone(),
            source,
        };
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(failed)?;
        }
        write_atomically(&self.path, &format!("{HEADER}{text}")).map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionBody, parse_actions};

    fn command(id: &str, command: &str, stdin: &str) -> TrustedCommand {
        let checked = parse_actions(&format!(
            "[[action]]\nid = {id:?}\nname = \"N\"\napplies_to = [\"grid.selection\"]\n\
             command = {command}\nstdin = {stdin:?}\n"
        ));
        let Some(action) = checked.value.first() else {
            panic!("{:?}", checked.problems);
        };
        let ActionBody::Command(body) = &action.body else {
            panic!("{action:?}");
        };
        TrustedCommand::of(&action.id, body)
    }

    #[test]
    fn keeps_what_was_trusted_and_forgets_an_older_definition() {
        let dir = tempfile::tempdir().unwrap();
        let file = TrustedCommandsFile::new(dir.path().join("data").join("trusted_commands.toml"));
        assert_eq!(file.load().unwrap(), TrustedCommands::default());

        let jq = command("jq", r#"["jq", "."]"#, "json");
        let mut trusted = TrustedCommands::default();
        trusted.trust(jq.clone());
        trusted.trust(command("wc", r#"["wc", "-l"]"#, "csv"));
        file.save(&trusted).unwrap();
        let loaded = file.load().unwrap();
        assert_eq!(loaded, trusted);
        assert!(loaded.contains(&jq));

        let changed = [
            command("jq", r#"["jq", "-r", "."]"#, "json"),
            command("jq", r#"["jq", "."]"#, "csv"),
            command("other", r#"["jq", "."]"#, "json"),
        ];
        for definition in &changed {
            assert!(!loaded.contains(definition), "{definition:?}");
        }

        let mut retrusted = loaded;
        retrusted.trust(changed[0].clone());
        assert!(!retrusted.contains(&jq));
        assert!(retrusted.contains(&changed[0]));
    }

    fn password(data_source: DataSourceId, command: &[&str]) -> TrustedCommand {
        let command: Vec<String> = command.iter().map(|&item| item.to_owned()).collect();
        TrustedCommand::password(data_source, &PasswordCommand::parse(&command).unwrap())
    }

    #[test]
    fn keeps_password_commands_beside_actions_each_by_its_owner() {
        let dir = tempfile::tempdir().unwrap();
        let file = TrustedCommandsFile::new(dir.path().join("trusted_commands.toml"));
        let shop = DataSourceId::generate();
        let printf = password(shop, &["printf", "dbrover"]);
        let mut trusted = TrustedCommands::default();
        trusted.trust(command("jq", r#"["jq", "."]"#, "json"));
        trusted.trust(printf.clone());
        file.save(&trusted).unwrap();
        let text = fs::read_to_string(file.path()).unwrap();
        assert!(
            text.contains(&format!("data_source = \"{shop}\"")),
            "{text}"
        );
        let loaded = file.load().unwrap();
        assert_eq!(loaded, trusted);
        assert!(loaded.contains(&printf));

        let elsewhere = password(DataSourceId::generate(), &["printf", "dbrover"]);
        let changed = password(shop, &["printf", "{user}"]);
        assert!(!loaded.contains(&elsewhere), "another data source asks");
        assert!(!loaded.contains(&changed), "a changed command asks");

        let mut retrusted = loaded;
        retrusted.trust(changed.clone());
        assert!(retrusted.contains(&changed));
        assert!(!retrusted.contains(&printf));
        assert!(retrusted.contains(&command("jq", r#"["jq", "."]"#, "json")));
    }

    #[test]
    fn keeps_export_programs_beside_the_others_each_by_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let file = TrustedCommandsFile::new(dir.path().join("trusted_commands.toml"));
        let program = |id: &str, command: &str| {
            let checked = crate::parse_exports(&format!(
                "[[export]]\nid = {id:?}\nname = \"N\"\nextension = \"pdf\"\ncommand = {command}\n"
            ));
            let Some(format) = checked.value.first() else {
                panic!("{:?}", checked.problems);
            };
            let dbrover_export::CustomFormatKind::Program(program) = &format.kind else {
                panic!("{format:?}");
            };
            TrustedCommand::export(&format.id, program)
        };
        let pdf = program("pdf", r#"["rows-to-pdf", "{table}"]"#);
        let mut trusted = TrustedCommands::default();
        trusted.trust(command("pdf", r#"["jq", "."]"#, "json"));
        trusted.trust(pdf.clone());
        file.save(&trusted).unwrap();
        let text = fs::read_to_string(file.path()).unwrap();
        assert!(text.contains("export = \"pdf\""), "{text}");
        let loaded = file.load().unwrap();
        assert_eq!(loaded, trusted);
        assert!(loaded.contains(&pdf));
        assert!(
            loaded.contains(&command("pdf", r#"["jq", "."]"#, "json")),
            "an action with the same id is someone else"
        );
        assert!(!loaded.contains(&program("pdf", r#"["rows-to-pdf"]"#)));
        assert!(!loaded.contains(&program("other", r#"["rows-to-pdf", "{table}"]"#)));
    }

    #[test]
    fn reads_the_actions_trusted_before_password_commands_existed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted_commands.toml");
        fs::write(
            &path,
            "[[command]]\nid = \"jq\"\ncommand = [\"jq\", \".\"]\nstdin = \"json\"\n",
        )
        .unwrap();
        let loaded = TrustedCommandsFile::new(&path).load().unwrap();
        assert!(loaded.contains(&command("jq", r#"["jq", "."]"#, "json")));
    }

    #[test]
    fn says_which_file_it_cant_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted_commands.toml");
        fs::write(&path, "[[command]]\nid = 3\n").unwrap();
        let error = TrustedCommandsFile::new(&path).load().unwrap_err();
        assert!(
            error
                .to_string()
                .ends_with("is not a list of trusted commands"),
            "{error}"
        );
    }
}
