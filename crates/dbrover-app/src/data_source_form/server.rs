//! The server section of the data source form: host, port, database, user, password (or the
//! command that prints it, or the cloud account that signs in), TLS and SSH, shared by every
//! network engine. An engine embeds [`ServerFields`] in its own fields and
//! adds only what is particular to it.

use std::path::PathBuf;

use dbrover_core::{
    AzureTool, CloudProvider, CloudSignIn, ManagedService, PasswordCommand, ServerConfig, TlsMode,
    has_placeholder,
};

use super::{FormError, SshFields, parse_port, required};

/// What a new data source's server section starts with, and the hints its fields show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerDefaults {
    pub host: &'static str,
    /// The port of unencrypted connections, and of every connection where TLS is
    /// [`TlsOffer::Negotiated`].
    pub port: u16,
    pub database: &'static str,
    pub database_field: DatabaseField,
    pub user: &'static str,
    pub user_field: UserField,
    pub tls: TlsOffer,
    /// The cloud accounts the engine's managed services sign in with, offered in place of a
    /// password; empty hides the choice.
    pub cloud: &'static [CloudProvider],
}

/// What a server's Database field names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseField {
    /// A database by its name.
    Name,
    /// One of Redis's numbered databases.
    Number,
    /// None: an Elasticsearch cluster's indices belong to no database. The form has no
    /// Database row, and the setting is left empty.
    Absent,
    /// The keyspace a Cassandra console starts in, which may be left empty.
    Keyspace,
}

impl DatabaseField {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Database",
            Self::Number => "Database number",
            Self::Absent => "",
            Self::Keyspace => "Keyspace",
        }
    }

    fn parse(self, text: &str) -> Result<String, FormError> {
        match self {
            Self::Absent => return Ok(String::new()),
            Self::Keyspace => return Ok(text.trim().to_owned()),
            Self::Name | Self::Number => {}
        }
        let database = required(text, FormError::DatabaseMissing)?;
        match self {
            Self::Name => Ok(database),
            Self::Number if has_placeholder(&database) || database.parse::<u32>().is_ok() => {
                Ok(database)
            }
            Self::Number => Err(FormError::InvalidDatabaseNumber),
            Self::Absent | Self::Keyspace => Ok(database),
        }
    }
}

/// Whether a server needs a user name to sign in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserField {
    Required,
    /// Left empty, the server's default user signs in, as Redis's `default` does.
    Optional,
}

impl UserField {
    fn parse(self, text: &str) -> Result<String, FormError> {
        match self {
            Self::Required => required(text, FormError::UserMissing),
            Self::Optional => Ok(text.trim().to_owned()),
        }
    }
}

/// How a server offers TLS, which decides the modes the form lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsOffer {
    /// Agreed on the server's one port: every mode, `prefer` first.
    Negotiated,
    /// On a port of its own (ClickHouse's HTTPS). No `prefer`, which needs one port that
    /// may or may not encrypt; `disable` first.
    OwnPort(u16),
}

impl TlsOffer {
    fn choices(self) -> &'static [TlsChoice] {
        match self {
            Self::Negotiated => &TlsChoice::ALL,
            Self::OwnPort(_) => &[
                TlsChoice::Disable,
                TlsChoice::Require,
                TlsChoice::VerifyFull,
            ],
        }
    }

    fn initial_choice(self) -> TlsChoice {
        match self {
            Self::Negotiated => TlsChoice::from(&TlsMode::default()),
            Self::OwnPort(_) => TlsChoice::Disable,
        }
    }
}

/// Text as the user typed it, plus the TLS choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFields {
    pub host: String,
    pub port: String,
    pub database: String,
    pub user: String,
    /// Empty means "keep the saved password" (or none, for a new data source).
    pub password: String,
    /// Whether the password comes from [`Self::password_command`] rather than the keychain.
    pub password_from_command: bool,
    /// The program, then each argument, one per line. Kept while the keychain is chosen, so
    /// switching back doesn't lose it.
    pub password_command: String,
    /// The cloud account that signs in instead of a password, when one is chosen: it wins over
    /// the password and its command, which are kept for switching back.
    pub cloud_provider: Option<CloudProvider>,
    pub cloud: CloudFields,
    pub tls: TlsChoice,
    /// Only used by [`TlsChoice::VerifyFull`], but kept while another mode is chosen so
    /// switching back doesn't lose it. Empty trusts the public web authorities.
    pub ca_file: String,
    pub ssh: SshFields,
    pub defaults: ServerDefaults,
    /// The managed service last chosen in the form (C1f), which names the service while the
    /// host doesn't name another.
    pub chosen_service: Option<ManagedService>,
}

impl ServerFields {
    pub fn new(defaults: ServerDefaults) -> Self {
        Self {
            host: defaults.host.to_owned(),
            port: defaults.port.to_string(),
            database: defaults.database.to_owned(),
            user: defaults.user.to_owned(),
            password: String::new(),
            password_from_command: false,
            password_command: String::new(),
            cloud_provider: None,
            cloud: CloudFields::default(),
            tls: defaults.tls.initial_choice(),
            ca_file: String::new(),
            ssh: SshFields::default(),
            defaults,
            chosen_service: None,
        }
    }

    /// The TLS modes this server can be connected with.
    pub fn tls_choices(&self) -> &'static [TlsChoice] {
        self.defaults.tls.choices()
    }

    /// The port a connection with the chosen TLS mode goes to unless told otherwise.
    pub fn default_port(&self) -> u16 {
        match self.defaults.tls {
            TlsOffer::OwnPort(tls_port) if self.tls.encrypts() => tls_port,
            _ => self.defaults.port,
        }
    }

    /// Chooses a TLS mode, moving the port along to the new mode's default if it was on the
    /// old one's, so choosing `require` for ClickHouse goes from 8123 to 8443.
    pub fn choose_tls(&mut self, choice: TlsChoice) {
        let on_default_port = self.port.trim() == self.default_port().to_string();
        self.tls = choice;
        if on_default_port {
            self.port = self.default_port().to_string();
        }
    }

    pub fn from_config(config: &ServerConfig, defaults: ServerDefaults) -> Self {
        let ca_file = match &config.tls {
            TlsMode::VerifyFull {
                ca_file: Some(path),
            } => path.display().to_string(),
            _ => String::new(),
        };
        Self {
            host: config.host.clone(),
            port: config.port.to_string(),
            database: config.database.clone(),
            user: config.user.clone(),
            password: String::new(),
            password_from_command: config.password_command.is_some(),
            password_command: config
                .password_command
                .as_ref()
                .map(|command| command.source().join("\n"))
                .unwrap_or_default(),
            cloud_provider: config.cloud_sign_in.as_ref().map(CloudSignIn::provider),
            cloud: config
                .cloud_sign_in
                .as_ref()
                .map(CloudFields::from_sign_in)
                .unwrap_or_default(),
            tls: TlsChoice::from(&config.tls),
            ca_file,
            ssh: SshFields::from_config(config.ssh.as_ref()),
            defaults,
            chosen_service: None,
        }
    }

    pub fn to_config(&self) -> Result<ServerConfig, FormError> {
        Ok(ServerConfig {
            host: required(&self.host, FormError::HostMissing)?,
            port: parse_port(&self.port, FormError::InvalidPort)?,
            database: self.defaults.database_field.parse(&self.database)?,
            user: self.defaults.user_field.parse(&self.user)?,
            tls: self.tls_mode(),
            ssh: self.ssh.to_config()?,
            password_command: match self.cloud_provider {
                Some(_) => None,
                None => self.parsed_password_command()?,
            },
            cloud_sign_in: self
                .cloud_provider
                .map(|provider| self.cloud.sign_in(provider)),
        })
    }

    /// Whether the password typed in the form counts: not when a command prints it or a cloud
    /// account signs in.
    pub fn typed_password(&self) -> Option<&str> {
        let typed = !self.password_from_command && self.cloud_provider.is_none();
        (typed && !self.password.is_empty()).then_some(self.password.as_str())
    }

    /// One item per line, each trimmed, blank lines skipped: a trailing newline or a stray
    /// space would otherwise become an argument no one can see.
    fn parsed_password_command(&self) -> Result<Option<PasswordCommand>, FormError> {
        if !self.password_from_command {
            return Ok(None);
        }
        let items: Vec<String> = self
            .password_command
            .lines()
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect();
        PasswordCommand::parse(&items)
            .map(Some)
            .map_err(FormError::PasswordCommand)
    }

    fn tls_mode(&self) -> TlsMode {
        match self.tls {
            TlsChoice::Disable => TlsMode::Disable,
            TlsChoice::Prefer => TlsMode::Prefer,
            TlsChoice::Require => TlsMode::Require,
            TlsChoice::VerifyFull => TlsMode::VerifyFull {
                ca_file: Some(self.ca_file.trim())
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from),
            },
        }
    }
}

/// The TLS modes, as the form lists them: [`TlsMode`] without its CA file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsChoice {
    Disable,
    Prefer,
    Require,
    VerifyFull,
}

impl TlsChoice {
    pub const ALL: [Self; 4] = [Self::Disable, Self::Prefer, Self::Require, Self::VerifyFull];

    /// Whether the connection is always encrypted.
    fn encrypts(self) -> bool {
        matches!(self, Self::Require | Self::VerifyFull)
    }

    /// libpq's `sslmode` names, which PostgreSQL users and hosting providers' guides use.
    pub fn label(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyFull => "verify-full",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Disable => "Never encrypt.",
            Self::Prefer => "Encrypt if the server offers it, without checking its certificate.",
            Self::Require => "Always encrypt, without checking the server's certificate.",
            Self::VerifyFull => {
                "Always encrypt, and check that the certificate is trusted and names the host."
            }
        }
    }
}

impl From<&TlsMode> for TlsChoice {
    fn from(tls: &TlsMode) -> Self {
        match tls {
            TlsMode::Disable => Self::Disable,
            TlsMode::Prefer => Self::Prefer,
            TlsMode::Require => Self::Require,
            TlsMode::VerifyFull { .. } => Self::VerifyFull,
        }
    }
}

/// The settings of every cloud sign-in, as typed, kept while another is chosen so switching
/// back doesn't lose them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudFields {
    /// AWS: empty for the default chain.
    pub profile: String,
    /// AWS: empty for the profile's.
    pub region: String,
    /// Google Cloud: empty for Application Default Credentials.
    pub credentials_file: String,
    /// Entra ID.
    pub tool: AzureTool,
    /// Entra ID: empty for the CLI's default directory.
    pub tenant: String,
}

impl CloudFields {
    fn from_sign_in(sign_in: &CloudSignIn) -> Self {
        match sign_in {
            CloudSignIn::Aws { profile, region } => Self {
                profile: profile.clone(),
                region: region.clone(),
                ..Self::default()
            },
            CloudSignIn::Google { credentials_file } => Self {
                credentials_file: credentials_file
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                ..Self::default()
            },
            CloudSignIn::Entra { tool, tenant } => Self {
                tool: *tool,
                tenant: tenant.clone(),
                ..Self::default()
            },
        }
    }

    fn sign_in(&self, provider: CloudProvider) -> CloudSignIn {
        let trimmed = |text: &str| text.trim().to_owned();
        match provider {
            CloudProvider::Aws => CloudSignIn::Aws {
                profile: trimmed(&self.profile),
                region: trimmed(&self.region),
            },
            CloudProvider::Google => CloudSignIn::Google {
                credentials_file: Some(self.credentials_file.trim())
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from),
            },
            CloudProvider::Entra => CloudSignIn::Entra {
                tool: self.tool,
                tenant: trimmed(&self.tenant),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use dbrover_core::Port;

    use super::*;

    const DEFAULTS: ServerDefaults = ServerDefaults {
        host: "localhost",
        port: 5432,
        database: "postgres",
        database_field: DatabaseField::Name,
        user: "postgres",
        user_field: UserField::Required,
        tls: TlsOffer::Negotiated,
        cloud: &[],
    };

    fn fields(edit: impl FnOnce(&mut ServerFields)) -> ServerFields {
        let mut fields = ServerFields::new(DEFAULTS);
        edit(&mut fields);
        fields
    }

    type FieldEdit = fn(&mut ServerFields);

    #[test]
    fn rejects_invalid_fields() {
        let cases: [(FieldEdit, FormError); 6] = [
            (|f| f.host = "  ".to_owned(), FormError::HostMissing),
            (|f| f.port = "abc".to_owned(), FormError::InvalidPort),
            (|f| f.port = "0".to_owned(), FormError::InvalidPort),
            (|f| f.port = "70000".to_owned(), FormError::InvalidPort),
            (|f| f.database = String::new(), FormError::DatabaseMissing),
            (|f| f.user = String::new(), FormError::UserMissing),
        ];

        for (edit, expected) in cases {
            assert_eq!(fields(edit).to_config(), Err(expected));
        }
    }

    /// Redis's database is a number, and its user may be left out for the `default` user.
    #[test]
    fn a_numbered_database_and_an_optional_user() {
        let redis = ServerDefaults {
            database: "0",
            database_field: DatabaseField::Number,
            user: "default",
            user_field: UserField::Optional,
            ..DEFAULTS
        };
        let with = |database: &str, user: &str| {
            let mut fields = ServerFields::new(redis);
            database.clone_into(&mut fields.database);
            user.clone_into(&mut fields.user);
            fields
                .to_config()
                .map(|config| (config.database, config.user))
        };
        assert_eq!(with(" 3 ", ""), Ok(("3".to_owned(), String::new())));
        assert_eq!(
            with("${DB}", "app"),
            Ok(("${DB}".to_owned(), "app".to_owned()))
        );
        assert_eq!(with("app", ""), Err(FormError::InvalidDatabaseNumber));
        assert_eq!(with("", ""), Err(FormError::DatabaseMissing));
        assert_eq!(DatabaseField::Number.label(), "Database number");
    }

    #[test]
    fn starts_from_the_engine_defaults_and_trims_what_is_typed() {
        let config = fields(|f| {
            f.host = " db.internal ".to_owned();
            f.port = " 6432 ".to_owned();
        })
        .to_config()
        .unwrap();

        assert_eq!(
            config,
            ServerConfig {
                host: "db.internal".to_owned(),
                port: 6432.into(),
                database: "postgres".to_owned(),
                user: "postgres".to_owned(),
                tls: TlsMode::Prefer,
                ssh: None,
                password_command: None,
                cloud_sign_in: None,
            }
        );
    }

    #[test]
    fn keeps_placeholders_as_typed_to_resolve_when_connecting() {
        let config = fields(|f| {
            f.host = " ${PGHOST} ".to_owned();
            f.port = "${PGPORT}".to_owned();
            f.user = "${USER}".to_owned();
        })
        .to_config()
        .unwrap();

        assert_eq!(config.host, "${PGHOST}");
        assert_eq!(config.port, Port::Placeholder("${PGPORT}".to_owned()));
        assert_eq!(config.user, "${USER}");
        let fields = ServerFields::from_config(&config, DEFAULTS);
        assert_eq!(fields.port, "${PGPORT}");
        assert_eq!(fields.to_config(), Ok(config));
    }

    #[test]
    fn keeps_the_ca_file_for_verify_full_only() {
        let cases = [
            (TlsChoice::Require, " /etc/ca.pem ", TlsMode::Require),
            (
                TlsChoice::VerifyFull,
                " /etc/ca.pem ",
                TlsMode::VerifyFull {
                    ca_file: Some("/etc/ca.pem".into()),
                },
            ),
            (
                TlsChoice::VerifyFull,
                "  ",
                TlsMode::VerifyFull { ca_file: None },
            ),
        ];
        for (choice, ca_file, expected) in cases {
            let config = fields(|f| {
                f.tls = choice;
                f.ca_file = ca_file.to_owned();
            })
            .to_config()
            .unwrap();

            assert_eq!(config.tls, expected, "{choice:?} {ca_file:?}");
        }
    }

    #[test]
    fn every_tls_mode_round_trips_through_the_fields() {
        let modes = [
            TlsMode::Disable,
            TlsMode::Prefer,
            TlsMode::Require,
            TlsMode::VerifyFull { ca_file: None },
            TlsMode::VerifyFull {
                ca_file: Some("/etc/ca.pem".into()),
            },
        ];
        for tls in modes {
            let config = ServerConfig {
                host: "db.internal".to_owned(),
                port: 6432.into(),
                database: "app".to_owned(),
                user: "juan".to_owned(),
                tls,
                ssh: None,
                password_command: None,
                cloud_sign_in: None,
            };

            let fields = ServerFields::from_config(&config, DEFAULTS);

            assert_eq!(fields.to_config(), Ok(config));
        }
    }

    #[test]
    fn reads_the_password_command_one_item_per_line_and_back() {
        let config = fields(|f| {
            f.password = "typed, then left".to_owned();
            f.password_from_command = true;
            f.password_command =
                " aws \nrds\n\ngenerate-db-auth-token\n--hostname\n{host}\n".to_owned();
        })
        .to_config()
        .unwrap();

        let command = config.password_command.as_ref().unwrap();
        assert_eq!(
            command.source(),
            [
                "aws",
                "rds",
                "generate-db-auth-token",
                "--hostname",
                "{host}"
            ]
        );
        let fields = ServerFields::from_config(&config, DEFAULTS);
        assert!(fields.password_from_command);
        assert_eq!(fields.typed_password(), None);
        assert_eq!(fields.to_config(), Ok(config));
    }

    #[test]
    fn keeps_the_command_aside_while_the_keychain_is_chosen() {
        let fields = fields(|f| {
            f.password = "secret".to_owned();
            f.password_command = "printf\ndbrover".to_owned();
        });
        assert_eq!(fields.to_config().unwrap().password_command, None);
        assert_eq!(fields.typed_password(), Some("secret"));
    }

    #[test]
    fn a_cloud_sign_in_wins_over_the_password_and_its_command() {
        let cases = [
            (
                CloudProvider::Aws,
                CloudSignIn::Aws {
                    profile: "analytics".to_owned(),
                    region: "eu-west-1".to_owned(),
                },
            ),
            (
                CloudProvider::Google,
                CloudSignIn::Google {
                    credentials_file: Some(PathBuf::from("~/keys/reader.json")),
                },
            ),
            (
                CloudProvider::Entra,
                CloudSignIn::Entra {
                    tool: AzureTool::AzureDeveloperCli,
                    tenant: "contoso.onmicrosoft.com".to_owned(),
                },
            ),
        ];
        for (provider, expected) in cases {
            let fields = fields(|f| {
                f.password = "typed, then left".to_owned();
                f.password_from_command = true;
                f.password_command = "printf\ndbrover".to_owned();
                f.cloud_provider = Some(provider);
                f.cloud = CloudFields {
                    profile: " analytics ".to_owned(),
                    region: "eu-west-1".to_owned(),
                    credentials_file: "~/keys/reader.json".to_owned(),
                    tool: AzureTool::AzureDeveloperCli,
                    tenant: "contoso.onmicrosoft.com".to_owned(),
                };
            });
            let config = fields.to_config().unwrap();
            assert_eq!(config.cloud_sign_in.as_ref(), Some(&expected));
            assert_eq!(config.password_command, None);
            assert_eq!(fields.typed_password(), None);

            let read_back = ServerFields::from_config(&config, DEFAULTS);
            assert_eq!(read_back.cloud_provider, Some(provider));
            assert_eq!(read_back.to_config(), Ok(config));
        }
    }

    #[test]
    fn says_what_is_wrong_with_the_command() {
        let cases = [
            (
                "",
                "the command is empty; its first item is the program to run",
            ),
            (
                "op\nread\n{table}",
                "{table} can't be in a password command, which can only have {host}, {port}, \
                 {user} and {database}",
            ),
        ];
        for (command, message) in cases {
            let error = fields(|f| {
                f.password_from_command = true;
                f.password_command = command.to_owned();
            })
            .to_config()
            .unwrap_err();
            assert_eq!(error.to_string(), format!("Password command: {message}."));
        }
    }

    #[test]
    fn moves_the_port_with_tls_on_its_own_port() {
        let defaults = ServerDefaults {
            port: 8123,
            tls: TlsOffer::OwnPort(8443),
            ..DEFAULTS
        };
        let mut fields = ServerFields::new(defaults);
        assert_eq!(
            (fields.tls, fields.port.as_str()),
            (TlsChoice::Disable, "8123")
        );
        assert!(!fields.tls_choices().contains(&TlsChoice::Prefer));

        fields.choose_tls(TlsChoice::Require);
        assert_eq!(fields.port, "8443");
        fields.choose_tls(TlsChoice::VerifyFull);
        assert_eq!(fields.port, "8443");
        fields.choose_tls(TlsChoice::Disable);
        assert_eq!(fields.port, "8123");

        fields.port = "9000".to_owned();
        fields.choose_tls(TlsChoice::Require);
        assert_eq!(fields.port, "9000", "a port the user typed stays");

        let mut negotiated = ServerFields::new(DEFAULTS);
        negotiated.choose_tls(TlsChoice::Require);
        assert_eq!(negotiated.port, "5432");
    }
}
