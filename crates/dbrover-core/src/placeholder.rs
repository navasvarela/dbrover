//! `${NAME}` placeholders in a data source's settings, resolved from the environment when
//! connecting. One shared file can then serve a team whose machines differ: each sets
//! `PGHOST`, say, and the file names `${PGHOST}`.
//!
//! Nothing here reads the environment itself; callers pass [`Variables`], so the rules are
//! testable without changing the process's environment.

use std::fmt;
use std::path::{Path, PathBuf};

/// Where placeholders get their values: the process's environment when connecting, a fixed
/// map in tests.
pub trait Variables {
    /// The variable's value, or `None` if it isn't set (or isn't valid Unicode).
    fn var(&self, name: &str) -> Option<String>;

    /// The user's home folder, which a leading `~` in a path stands for.
    fn home(&self) -> Option<PathBuf>;
}

/// Why a setting's text couldn't be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExpandError {
    #[error("${{{0}}} is not set")]
    Unset(String),
    #[error("a `${{` has no closing `}}`")]
    Unclosed,
    #[error("${{{0}}} is not a variable name: use letters, digits and underscores")]
    InvalidName(String),
    #[error("it starts with ~, and the home folder isn't known")]
    NoHome,
}

/// The settings that may hold placeholders, to say which one couldn't be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Host,
    Port,
    Database,
    User,
    File,
    CaFile,
    SshHost,
    SshPort,
    SshUser,
    SshKeyFile,
    AuthDatabase,
    ConnectionString,
    LocalDatacenter,
    Profile,
    Region,
    Endpoint,
}

impl fmt::Display for Setting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Host => "the host",
            Self::Port => "the port",
            Self::Database => "the database",
            Self::User => "the user",
            Self::File => "the file",
            Self::CaFile => "the CA file",
            Self::SshHost => "the SSH host",
            Self::SshPort => "the SSH port",
            Self::SshUser => "the SSH user",
            Self::SshKeyFile => "the SSH key file",
            Self::AuthDatabase => "the authentication database",
            Self::ConnectionString => "the connection string",
            Self::LocalDatacenter => "the local datacenter",
            Self::Profile => "the AWS profile",
            Self::Region => "the region",
            Self::Endpoint => "the endpoint",
        })
    }
}

/// A data source's settings couldn't be resolved, so it can't connect.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaceholderError {
    #[error("could not resolve {setting}")]
    Expand {
        setting: Setting,
        #[source]
        error: ExpandError,
    },
    #[error("{setting} resolves to {value:?}, which is not a port number from 1 to 65535")]
    InvalidPort { setting: Setting, value: String },
}

impl PlaceholderError {
    pub(crate) fn in_setting(setting: Setting) -> impl FnOnce(ExpandError) -> Self {
        move |error| Self::Expand { setting, error }
    }
}

/// Whether `text` holds a placeholder, so the form knows to show what it resolves to.
pub fn has_placeholder(text: &str) -> bool {
    text.contains("${")
}

/// `text` with every `${NAME}` replaced by the variable's value. A `$` not followed by `{` is
/// kept as it is.
///
/// # Errors
///
/// [`ExpandError`] if a variable isn't set, a name isn't valid, or a `${` isn't closed.
pub fn expand(text: &str, variables: &dyn Variables) -> Result<String, ExpandError> {
    let mut expanded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or(ExpandError::Unclosed)?;
        let name = &after[..end];
        if !is_variable_name(name) {
            return Err(ExpandError::InvalidName(name.to_owned()));
        }
        let value = variables
            .var(name)
            .ok_or_else(|| ExpandError::Unset(name.to_owned()))?;
        expanded.push_str(&value);
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);
    Ok(expanded)
}

/// A path with placeholders expanded, and a leading `~` replaced by the home folder.
///
/// # Errors
///
/// [`ExpandError`] as for [`expand`], or [`ExpandError::NoHome`].
pub fn expand_path(path: &Path, variables: &dyn Variables) -> Result<PathBuf, ExpandError> {
    // A path that isn't valid Unicode can't have been typed with a placeholder in it.
    let Some(text) = path.to_str() else {
        return Ok(path.to_owned());
    };
    let expanded = expand(text, variables)?;
    let home_relative = match expanded.strip_prefix('~') {
        Some("") => Some(""),
        Some(rest) => rest.strip_prefix(['/', std::path::MAIN_SEPARATOR]),
        None => None,
    };
    match home_relative {
        Some(rest) => {
            let home = variables.home().ok_or(ExpandError::NoHome)?;
            Ok(if rest.is_empty() {
                home
            } else {
                home.join(rest)
            })
        }
        None => Ok(PathBuf::from(expanded)),
    }
}

/// The names shells accept, so a variable that can be exported can be named.
fn is_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Variables from a fixed list, with `/home/juan` as the home folder.
    pub(crate) struct Fixed(pub HashMap<&'static str, &'static str>);

    impl Fixed {
        pub(crate) fn new(variables: &[(&'static str, &'static str)]) -> Self {
            Self(variables.iter().copied().collect())
        }
    }

    impl Variables for Fixed {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).map(|value| (*value).to_owned())
        }

        fn home(&self) -> Option<PathBuf> {
            Some("/home/juan".into())
        }
    }

    #[test]
    fn expands_every_placeholder_and_keeps_the_rest() {
        let variables = Fixed::new(&[("ENV", "staging"), ("PGHOST", "db.internal")]);
        let cases = [
            ("localhost", "localhost"),
            ("${PGHOST}", "db.internal"),
            ("db-${ENV}.example.com", "db-staging.example.com"),
            ("${ENV}-${ENV}", "staging-staging"),
            ("cost$5 and $HOME", "cost$5 and $HOME"),
            ("", ""),
        ];
        for (text, expected) in cases {
            assert_eq!(expand(text, &variables).as_deref(), Ok(expected), "{text}");
        }
    }

    #[test]
    fn says_what_is_wrong_with_a_placeholder() {
        let variables = Fixed::new(&[]);
        let cases = [
            ("${PGHOST}", ExpandError::Unset("PGHOST".to_owned())),
            ("db-${ENV", ExpandError::Unclosed),
            ("${}", ExpandError::InvalidName(String::new())),
            ("${1X}", ExpandError::InvalidName("1X".to_owned())),
            ("${A-B}", ExpandError::InvalidName("A-B".to_owned())),
        ];
        for (text, expected) in cases {
            assert_eq!(expand(text, &variables), Err(expected), "{text}");
        }
        assert_eq!(
            ExpandError::Unset("PGHOST".to_owned()).to_string(),
            "${PGHOST} is not set"
        );
    }

    #[test]
    fn expands_the_home_folder_and_placeholders_in_paths() {
        let variables = Fixed::new(&[("DATA", "/srv/data")]);
        let cases = [
            ("~/data/shop.db", "/home/juan/data/shop.db"),
            ("~", "/home/juan"),
            ("${DATA}/shop.db", "/srv/data/shop.db"),
            ("/abs/shop.db", "/abs/shop.db"),
            ("~other/shop.db", "~other/shop.db"),
            ("relative/shop.db", "relative/shop.db"),
        ];
        for (path, expected) in cases {
            assert_eq!(
                expand_path(Path::new(path), &variables),
                Ok(PathBuf::from(expected)),
                "{path}"
            );
        }
    }
}
