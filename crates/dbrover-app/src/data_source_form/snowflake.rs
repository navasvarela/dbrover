//! A Snowflake data source's fields (C5): the account and user, how they sign in, and the
//! context its statements start in. The secret typed here, an encrypted key's passphrase or a
//! token, is the data source's password, kept in the keychain.

use std::path::PathBuf;

use dbrover_core::{CostGuard, PasswordCommand, SnowflakeConfig, SnowflakeSignIn, TimeLimit};

use super::FormError;
use super::time_limit::{Longest, NEW_LIMIT_SECONDS, minutes, parse_minutes};

/// The limit's field in the dialog, which its errors name.
pub const LIMIT_LABEL: &str = "Stop statements after";

/// Snowflake's longest statement timeout: seven days, 604,800 seconds.
const LONGEST: Longest = Longest {
    minutes: 7.0 * 24.0 * 60.0,
    said: "seven days",
};

/// How the data source signs in, as the dialog offers it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SnowflakeSignInChoice {
    #[default]
    KeyPair,
    ProgrammaticAccessToken,
    OAuth,
}

impl SnowflakeSignInChoice {
    /// What the secret row is called for this way of signing in.
    pub fn secret_label(self) -> &'static str {
        match self {
            Self::KeyPair => "Passphrase",
            Self::ProgrammaticAccessToken => "Token",
            Self::OAuth => "OAuth token",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnowflakeFields {
    pub account: String,
    pub user: String,
    pub sign_in: SnowflakeSignInChoice,
    /// Kept while a token is chosen, so switching back doesn't lose it.
    pub private_key_file: String,
    /// The passphrase or token as typed; empty keeps the saved one.
    pub secret: String,
    /// Whether an OAuth token comes from [`Self::token_command`] rather than the keychain.
    pub token_from_command: bool,
    /// The program, then each argument, one per line.
    pub token_command: String,
    pub warehouse: String,
    pub role: String,
    pub database: String,
    pub schema: String,
    pub endpoint: String,
    pub storage_integration: String,
    /// In minutes; empty for no limit of DbRover's. Saved as the cost guard's
    /// `stop_after_seconds`.
    pub limit_minutes: String,
    pub cost_guard: CostGuard,
}

impl Default for SnowflakeFields {
    fn default() -> Self {
        Self::from_config(&SnowflakeConfig {
            cost_guard: CostGuard {
                stop_after: TimeLimit::from_seconds(NEW_LIMIT_SECONDS),
                ..CostGuard::default()
            },
            ..SnowflakeConfig::default()
        })
    }
}

impl SnowflakeFields {
    pub fn from_config(config: &SnowflakeConfig) -> Self {
        let (sign_in, private_key_file) = match &config.sign_in {
            SnowflakeSignIn::KeyPair { private_key_file } => (
                SnowflakeSignInChoice::KeyPair,
                private_key_file.display().to_string(),
            ),
            SnowflakeSignIn::ProgrammaticAccessToken => (
                SnowflakeSignInChoice::ProgrammaticAccessToken,
                String::new(),
            ),
            SnowflakeSignIn::OAuth => (SnowflakeSignInChoice::OAuth, String::new()),
        };
        Self {
            account: config.account.clone(),
            user: config.user.clone(),
            sign_in,
            private_key_file,
            secret: String::new(),
            token_from_command: config.password_command.is_some(),
            token_command: config
                .password_command
                .as_ref()
                .map(|command| command.source().join("\n"))
                .unwrap_or_default(),
            warehouse: config.warehouse.clone(),
            role: config.role.clone(),
            database: config.database.clone(),
            schema: config.schema.clone(),
            endpoint: config.endpoint.clone(),
            storage_integration: config.storage_integration.clone(),
            limit_minutes: config
                .cost_guard
                .stop_after
                .map(minutes)
                .unwrap_or_default(),
            cost_guard: config.cost_guard,
        }
    }

    /// The secret typed in the form, where it counts: not when a command prints the token.
    pub fn typed_secret(&self) -> Option<&str> {
        let typed = !self.uses_command();
        (typed && !self.secret.is_empty()).then_some(self.secret.as_str())
    }

    fn uses_command(&self) -> bool {
        self.sign_in == SnowflakeSignInChoice::OAuth && self.token_from_command
    }

    /// # Errors
    ///
    /// [`FormError::AccountMissing`] and [`FormError::UserMissing`] without them,
    /// [`FormError::PrivateKeyFileMissing`] for a key pair without its file, and
    /// [`FormError::PasswordCommand`] for a token command that can't be read.
    pub fn to_config(&self) -> Result<SnowflakeConfig, FormError> {
        let required = |text: &str, missing| match text.trim() {
            "" => Err(missing),
            text => Ok(text.to_owned()),
        };
        let sign_in = match self.sign_in {
            SnowflakeSignInChoice::KeyPair => SnowflakeSignIn::KeyPair {
                private_key_file: PathBuf::from(required(
                    &self.private_key_file,
                    FormError::PrivateKeyFileMissing,
                )?),
            },
            SnowflakeSignInChoice::ProgrammaticAccessToken => {
                SnowflakeSignIn::ProgrammaticAccessToken
            }
            SnowflakeSignInChoice::OAuth => SnowflakeSignIn::OAuth,
        };
        let stop_after = match self.limit_minutes.trim() {
            "" => None,
            typed => Some(
                parse_minutes(typed, LONGEST)
                    .ok_or(FormError::InvalidTimeLimit(LIMIT_LABEL, LONGEST.said))?,
            ),
        };
        Ok(SnowflakeConfig {
            account: required(&self.account, FormError::AccountMissing)?,
            user: required(&self.user, FormError::UserMissing)?,
            sign_in,
            password_command: self.token_command()?,
            warehouse: self.warehouse.trim().to_owned(),
            role: self.role.trim().to_owned(),
            database: self.database.trim().to_owned(),
            schema: self.schema.trim().to_owned(),
            endpoint: self.endpoint.trim().to_owned(),
            storage_integration: self.storage_integration.trim().to_owned(),
            cost_guard: CostGuard {
                stop_after,
                ..self.cost_guard
            },
        })
    }

    /// One item per line, each trimmed, blank lines skipped, as a server's password command.
    fn token_command(&self) -> Result<Option<PasswordCommand>, FormError> {
        if !self.uses_command() {
            return Ok(None);
        }
        let items: Vec<String> = self
            .token_command
            .lines()
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect();
        PasswordCommand::parse(&items)
            .map(Some)
            .map_err(FormError::PasswordCommand)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_data_source_stops_statements_after_ten_minutes() {
        let fresh = SnowflakeFields::default();
        assert_eq!(fresh.limit_minutes, "10");
        let config = fields().to_config().unwrap();
        assert_eq!(config.cost_guard.stop_after, TimeLimit::from_seconds(600));
    }

    #[test]
    fn the_limit_is_typed_in_minutes() {
        for (typed, seconds) in [("", None), ("0.5", Some(30)), (" 90 ", Some(5400))] {
            let config = SnowflakeFields {
                limit_minutes: typed.into(),
                ..fields()
            }
            .to_config()
            .unwrap();
            assert_eq!(
                config.cost_guard.stop_after.map(TimeLimit::seconds),
                seconds
            );
            let shown = SnowflakeFields::from_config(&config).limit_minutes;
            assert_eq!(shown, typed.trim());
        }
        for typed in ["0", "-1", "ten", "20000"] {
            let refused = SnowflakeFields {
                limit_minutes: typed.into(),
                ..fields()
            }
            .to_config();
            assert!(
                matches!(refused, Err(FormError::InvalidTimeLimit(LIMIT_LABEL, _))),
                "{typed}"
            );
        }
    }

    fn fields() -> SnowflakeFields {
        SnowflakeFields {
            account: " myorg-analytics ".into(),
            user: "jane".into(),
            private_key_file: "~/.snowflake/rsa_key.p8".into(),
            warehouse: "ANALYTICS_WH".into(),
            ..SnowflakeFields::default()
        }
    }

    #[test]
    fn a_key_pair_saves_its_file_and_context() {
        let config = fields().to_config().unwrap();
        assert_eq!(config.account, "myorg-analytics");
        assert_eq!(
            config.sign_in,
            SnowflakeSignIn::KeyPair {
                private_key_file: PathBuf::from("~/.snowflake/rsa_key.p8")
            }
        );
        assert_eq!(config.warehouse, "ANALYTICS_WH");
        assert_eq!(
            SnowflakeFields::from_config(&config).to_config().unwrap(),
            config
        );
    }

    #[test]
    fn what_is_missing_is_named() {
        let no_key = SnowflakeFields {
            private_key_file: " ".into(),
            ..fields()
        };
        assert!(matches!(
            no_key.to_config(),
            Err(FormError::PrivateKeyFileMissing)
        ));
        let no_account = SnowflakeFields {
            account: String::new(),
            ..fields()
        };
        assert!(matches!(
            no_account.to_config(),
            Err(FormError::AccountMissing)
        ));
    }

    #[test]
    fn an_oauth_token_may_come_from_a_command_and_then_isnt_typed() {
        let oauth = SnowflakeFields {
            sign_in: SnowflakeSignInChoice::OAuth,
            secret: "typed".into(),
            token_from_command: true,
            token_command: "okta-token\n--audience\nsnowflake\n".into(),
            ..fields()
        };
        let config = oauth.to_config().unwrap();
        assert_eq!(config.sign_in, SnowflakeSignIn::OAuth);
        assert_eq!(
            config.password_command.unwrap().source(),
            ["okta-token", "--audience", "snowflake"]
        );
        assert_eq!(oauth.typed_secret(), None);
        let typed = SnowflakeFields {
            token_from_command: false,
            ..oauth
        };
        assert_eq!(typed.typed_secret(), Some("typed"));
    }
}
