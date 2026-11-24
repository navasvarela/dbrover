//! Running a provider's CLI for a token, as Azure's and Databricks's own libraries do: no
//! shell in between, a time limit, no console window on Windows, and Homebrew's folders on a
//! Mac, whose apps started from the Finder don't get the terminal's `PATH`.

use std::io;
use std::process::{Output, Stdio};
use std::time::Duration;

/// How long a CLI may take to answer. It refreshes its own token over the network when it
/// needs to, which takes a few seconds at most; one that hangs holds the connection up.
const PROGRAM_TIMEOUT: Duration = Duration::from_secs(30);

/// Hides the console window Windows would otherwise open for a console program started from a
/// GUI app (`CREATE_NO_WINDOW` in the Win32 process creation flags).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where Homebrew puts programs on a Mac: Apple silicon's folder, then Intel's.
#[cfg(target_os = "macos")]
const MAC_INSTALL_FOLDERS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// A program to run, and its arguments, with no shell in between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invocation {
    pub program: &'static str,
    pub arguments: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProgramError {
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
    #[error("{program} gave no token within {} seconds", PROGRAM_TIMEOUT.as_secs())]
    TimedOut { program: &'static str },
}

/// What `invocation` printed and how it ended; `sign_in` is the command that signs its program
/// in, which the error for a program that isn't installed names.
///
/// # Errors
///
/// [`ProgramError`] when the program isn't found, can't be started, or runs past
/// [`PROGRAM_TIMEOUT`].
pub(crate) async fn output(
    invocation: &Invocation,
    sign_in: &'static str,
) -> Result<Output, ProgramError> {
    let program = invocation.program;
    let child = match spawn_found(program, &invocation.arguments) {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ProgramError::NotInstalled { program, sign_in });
        }
        Err(source) => return Err(ProgramError::CouldNotRun { program, source }),
    };
    tokio::time::timeout(PROGRAM_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| ProgramError::TimedOut { program })?
        .map_err(|source| ProgramError::CouldNotRun { program, source })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Cargo stands in for a CLI: it is on every machine the tests run on, and with an unknown
    /// subcommand it fails as a CLI does when it isn't signed in.
    #[tokio::test]
    async fn a_program_that_fails_or_is_missing_says_so() {
        let failing = Invocation {
            program: env!("CARGO"),
            arguments: vec!["no-such-subcommand-for-dbrover".into()],
        };
        let ended = output(&failing, "az login").await.unwrap();
        assert!(!ended.status.success());

        let missing = Invocation {
            program: "dbrover-no-such-program",
            arguments: Vec::new(),
        };
        let error = output(&missing, "az login").await.unwrap_err().to_string();
        assert!(
            error.contains("dbrover-no-such-program was not found"),
            "{error}"
        );
        assert!(error.contains("az login"), "{error}");
    }
}
