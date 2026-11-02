//! Signing in with Microsoft Entra ID through the sign-in the Azure CLI or the Azure Developer
//! CLI already has on the machine (`docs/cloud-data.md`, C0c): the program is asked for a
//! token, as Azure's own libraries ask it, rather than its token cache read, which is
//! encrypted on Windows and macOS and whose format is the CLI's own.

use std::io;
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use dbrover_core::Password;
use serde::Deserialize;

use crate::token::AccessToken;

/// How long the CLI may take to answer. It refreshes its own token over the network when
/// it needs to, which takes a few seconds at most; one that hangs holds the connection up.
const PROGRAM_TIMEOUT: Duration = Duration::from_secs(30);

/// Hides the console window Windows would otherwise open for a console program started from a
/// GUI app (`CREATE_NO_WINDOW` in the Win32 process creation flags).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where Homebrew puts `az` and `azd` on a Mac. An app started from the Finder doesn't get the
/// terminal's `PATH`, so a program installed there isn't found by its name alone.
#[cfg(target_os = "macos")]
const MAC_INSTALL_FOLDERS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// The program whose sign-in a data source uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EntraTool {
    /// `az login`'s sign-in.
    #[default]
    AzureCli,
    /// `azd auth login`'s sign-in.
    AzureDeveloperCli,
}

/// What the token is for: each kind of Azure database accepts tokens for its own resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntraResource {
    /// Azure SQL, Synapse's SQL pools and Fabric's SQL endpoints, over SQL Server's protocol.
    AzureSql,
    /// Azure Database for PostgreSQL and for MySQL.
    AzureDatabase,
}

impl EntraResource {
    /// The OAuth 2.0 scope of the resource's tokens, as Azure documents it for each.
    pub fn scope(self) -> &'static str {
        match self {
            Self::AzureSql => "https://database.windows.net/.default",
            Self::AzureDatabase => "https://ossrdbms-aad.database.windows.net/.default",
        }
    }
}

/// How an Entra ID data source signs in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntraSignIn {
    pub tool: EntraTool,
    /// The directory to sign in to, when the CLI's default isn't the one: a tenant id or a
    /// domain.
    pub tenant: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EntraError {
    #[error("The tenant {0:?} isn't a tenant id or a domain")]
    BadTenant(String),
    #[error("{program} was not found in DbRover's PATH; install it, and sign in with `{sign_in}`")]
    NotInstalled {
        program: &'static str,
        sign_in: &'static str,
    },
    #[error("Couldn't run {program}: {source}")]
    CouldNotRun {
        program: &'static str,
        source: io::Error,
    },
    #[error("{program} gave no token ({status}): {stderr}")]
    Failed {
        program: &'static str,
        status: String,
        stderr: String,
    },
    #[error("{program} gave no token within {} seconds", PROGRAM_TIMEOUT.as_secs())]
    TimedOut { program: &'static str },
    #[error("{program}'s answer can't be read: {reason}")]
    Unreadable {
        program: &'static str,
        reason: String,
    },
}

/// A program to run, and its arguments, with no shell in between.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Invocation {
    program: &'static str,
    arguments: Vec<String>,
}

impl EntraTool {
    /// The program's name: `az` is a batch file on Windows, which only runs by its full name.
    fn program(self) -> &'static str {
        match (self, cfg!(windows)) {
            (Self::AzureCli, true) => "az.cmd",
            (Self::AzureCli, false) => "az",
            (Self::AzureDeveloperCli, _) => "azd",
        }
    }

    fn sign_in_command(self) -> &'static str {
        match self {
            Self::AzureCli => "az login",
            Self::AzureDeveloperCli => "azd auth login",
        }
    }
}

impl EntraSignIn {
    /// A token for `resource` from the CLI's sign-in.
    ///
    /// # Errors
    ///
    /// [`EntraError`] when the program can't be run, fails (it isn't signed in, say), runs
    /// past the timeout, or answers with something other than a token.
    pub async fn token(&self, resource: EntraResource) -> Result<AccessToken, EntraError> {
        let invocation = self.invocation(resource)?;
        let output = run(&invocation, self.tool.sign_in_command()).await?;
        if !output.status.success() {
            return Err(EntraError::Failed {
                program: invocation.program,
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let unreadable = |reason: String| EntraError::Unreadable {
            program: invocation.program,
            reason,
        };
        match self.tool {
            EntraTool::AzureCli => read_azure_cli(&output.stdout),
            EntraTool::AzureDeveloperCli => read_azure_developer_cli(&output.stdout),
        }
        .map_err(unreadable)
    }

    fn invocation(&self, resource: EntraResource) -> Result<Invocation, EntraError> {
        let tenant_flag = match self.tool {
            EntraTool::AzureCli => "--tenant",
            EntraTool::AzureDeveloperCli => "--tenant-id",
        };
        let mut arguments: Vec<String> = match self.tool {
            EntraTool::AzureCli => vec!["account".into(), "get-access-token".into()],
            EntraTool::AzureDeveloperCli => vec!["auth".into(), "token".into()],
        };
        arguments.extend(["--scope".into(), resource.scope().into()]);
        arguments.extend(["--output".into(), "json".into()]);
        if let Some(tenant) = self.tenant.as_deref().filter(|tenant| !tenant.is_empty()) {
            // A batch file's arguments pass through `cmd.exe` on Windows; a tenant is only
            // ever letters, digits, dots and dashes, so nothing else is let through.
            let is_tenant = tenant
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
            if !is_tenant {
                return Err(EntraError::BadTenant(tenant.to_owned()));
            }
            arguments.extend([tenant_flag.into(), tenant.into()]);
        }
        Ok(Invocation {
            program: self.tool.program(),
            arguments,
        })
    }
}

async fn run(
    invocation: &Invocation,
    sign_in: &'static str,
) -> Result<std::process::Output, EntraError> {
    let program = invocation.program;
    let not_found = || EntraError::NotInstalled { program, sign_in };
    let started = spawn_found(program, &invocation.arguments);
    let child = match started {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(not_found()),
        Err(source) => return Err(EntraError::CouldNotRun { program, source }),
    };
    tokio::time::timeout(PROGRAM_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| EntraError::TimedOut { program })?
        .map_err(|source| EntraError::CouldNotRun { program, source })
}

/// Starts `program` from the `PATH`, or on a Mac from where Homebrew installs it.
fn spawn_found(program: &str, arguments: &[String]) -> io::Result<tokio::process::Child> {
    let started = spawn(program, arguments);
    #[cfg(target_os = "macos")]
    if started
        .as_ref()
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
    {
        let installed = MAC_INSTALL_FOLDERS
            .iter()
            .map(|folder| std::path::Path::new(folder).join(program))
            .find(|path| path.is_file());
        if let Some(path) = installed {
            return spawn(&path.to_string_lossy(), arguments);
        }
    }
    started
}

fn spawn(program: &str, arguments: &[String]) -> io::Result<tokio::process::Child> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command.spawn()
}

/// `az account get-access-token`'s answer.
#[derive(Deserialize)]
struct AzureCliToken {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// Seconds since 1970, from Azure CLI 2.54 on.
    expires_on: Option<i64>,
    /// The local time, without its offset, as older versions write it:
    /// `2026-11-02 13:00:00.000000`.
    #[serde(rename = "expiresOn")]
    expires_on_local: Option<String>,
}

fn read_azure_cli(stdout: &[u8]) -> Result<AccessToken, String> {
    let answer: AzureCliToken =
        serde_json::from_slice(stdout).map_err(|error| error.to_string())?;
    let expires_at = match (answer.expires_on, answer.expires_on_local.as_deref()) {
        (Some(seconds), _) => {
            jiff::Timestamp::from_second(seconds).map_err(|error| error.to_string())?
        }
        (None, Some(local)) => local
            .parse::<jiff::civil::DateTime>()
            .and_then(|time| time.to_zoned(jiff::tz::TimeZone::system()))
            .map_err(|error| format!("its expiry {local:?}: {error}"))?
            .timestamp(),
        (None, None) => return Err("it has no expiry".to_owned()),
    };
    Ok(AccessToken {
        secret: Password::new(answer.access_token),
        expires_at: SystemTime::from(expires_at),
    })
}

/// `azd auth token`'s answer.
#[derive(Deserialize)]
struct AzureDeveloperCliToken {
    token: String,
    /// RFC 3339: `2026-11-02T13:00:00Z`.
    #[serde(rename = "expiresOn")]
    expires_on: String,
}

fn read_azure_developer_cli(stdout: &[u8]) -> Result<AccessToken, String> {
    let answer: AzureDeveloperCliToken =
        serde_json::from_slice(stdout).map_err(|error| error.to_string())?;
    let expires_at: jiff::Timestamp = answer
        .expires_on
        .parse()
        .map_err(|error| format!("its expiry {:?}: {error}", answer.expires_on))?;
    Ok(AccessToken {
        secret: Password::new(answer.token),
        expires_at: SystemTime::from(expires_at),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign_in(tool: EntraTool, tenant: Option<&str>) -> EntraSignIn {
        EntraSignIn {
            tool,
            tenant: tenant.map(str::to_owned),
        }
    }

    #[test]
    fn each_cli_is_asked_for_the_resources_token_without_a_shell() {
        let cases = [
            (
                sign_in(EntraTool::AzureCli, None),
                EntraResource::AzureSql,
                "account get-access-token --scope https://database.windows.net/.default \
                 --output json",
            ),
            (
                sign_in(EntraTool::AzureCli, Some("contoso.onmicrosoft.com")),
                EntraResource::AzureDatabase,
                "account get-access-token --scope \
                 https://ossrdbms-aad.database.windows.net/.default --output json --tenant \
                 contoso.onmicrosoft.com",
            ),
            (
                sign_in(
                    EntraTool::AzureDeveloperCli,
                    Some("72f988bf-86f1-41af-91ab-2d7cd011db47"),
                ),
                EntraResource::AzureSql,
                "auth token --scope https://database.windows.net/.default --output json \
                 --tenant-id 72f988bf-86f1-41af-91ab-2d7cd011db47",
            ),
        ];
        for (sign_in, resource, expected) in cases {
            let invocation = sign_in.invocation(resource).unwrap();
            assert_eq!(invocation.arguments.join(" "), expected);
        }
    }

    #[test]
    fn a_tenant_that_could_be_more_than_one_argument_is_refused() {
        for tenant in ["a b", "x&calc", "\"q\"", "t%PATH%"] {
            let error = sign_in(EntraTool::AzureCli, Some(tenant))
                .invocation(EntraResource::AzureSql)
                .unwrap_err();
            assert!(matches!(error, EntraError::BadTenant(_)), "{tenant}");
        }
    }

    #[test]
    fn the_azure_clis_answer_becomes_a_token() {
        let current = br#"{"accessToken":"eyJ0","expiresOn":"2026-11-02 13:00:00.000000","expires_on":1793624400,"subscription":"s","tenant":"t","tokenType":"Bearer"}"#;
        let token = read_azure_cli(current).unwrap();
        assert_eq!(token.secret.expose(), "eyJ0");
        assert_eq!(
            token.expires_at,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_624_400)
        );

        // Before 2.54, only the local time.
        let older = br#"{"accessToken":"eyJ0","expiresOn":"2026-11-02 13:00:00.000000"}"#;
        let token = read_azure_cli(older).unwrap();
        let expected = "2026-11-02T13:00:00"
            .parse::<jiff::civil::DateTime>()
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::system())
            .unwrap()
            .timestamp();
        assert_eq!(token.expires_at, SystemTime::from(expected));

        for (answer, expected) in [
            (&br#"{"accessToken":"eyJ0"}"#[..], "no expiry"),
            (b"Please run 'az login'", "expected value"),
        ] {
            let error = read_azure_cli(answer).unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn the_azure_developer_clis_answer_becomes_a_token() {
        let answer = br#"{"token":"eyJ0","expiresOn":"2026-11-02T13:00:00Z"}"#;
        let token = read_azure_developer_cli(answer).unwrap();
        assert_eq!(token.secret.expose(), "eyJ0");
        let expected: jiff::Timestamp = "2026-11-02T13:00:00Z".parse().unwrap();
        assert_eq!(token.expires_at, SystemTime::from(expected));
    }

    /// Cargo stands in for the CLI: it is on every machine the tests run on, and with an
    /// unknown subcommand it fails as `az` does when it isn't signed in.
    #[tokio::test]
    async fn a_program_that_fails_or_is_missing_says_so() {
        let failing = Invocation {
            program: env!("CARGO"),
            arguments: vec!["no-such-subcommand-for-dbrover".into()],
        };
        let output = run(&failing, "az login").await.unwrap();
        assert!(!output.status.success());

        let missing = Invocation {
            program: "dbrover-no-such-program",
            arguments: Vec::new(),
        };
        let error = run(&missing, "az login").await.unwrap_err().to_string();
        assert!(
            error.contains("dbrover-no-such-program was not found"),
            "{error}"
        );
        assert!(error.contains("az login"), "{error}");
    }
}
