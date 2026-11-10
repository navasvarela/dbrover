//! Keeping the machine awake while a long report runs (`docs/reports-at-scale.md`, RS3e), with
//! a program the system already has, which needs no elevated rights and ends with the worker
//! by itself, whatever happens to the app:
//! - macOS: `caffeinate -i -w <worker pid>`, an assertion against idle sleep;
//! - Linux, where systemd is: `systemd-inhibit` holding an idle and sleep inhibitor around
//!   `tail --pid=<worker pid> -f /dev/null`, which returns once the worker has exited.
//!
//! Windows waits for a safe wrapper of `SetThreadExecutionState`, as its Job Object does.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long a run goes before the machine is kept awake for it: a short run finishes long
/// before any machine dozes off, and needs no program started for it.
pub const KEEP_AWAKE_AFTER: Duration = Duration::from_secs(2 * 60);

/// What the worker's process id is written as in a [`KeepAwake`]'s arguments.
const PID: &str = "{pid}";

/// A program that keeps the machine awake for as long as a process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepAwake {
    /// How long a run goes before it is started.
    pub after: Duration,
    program: PathBuf,
    /// Its arguments, `{pid}` standing for the worker's process id.
    args: Vec<String>,
}

impl KeepAwake {
    /// The system's own program, after [`KEEP_AWAKE_AFTER`]; `None` where there is none.
    pub fn system() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::caffeinate())
        } else if cfg!(target_os = "linux") {
            find_program("systemd-inhibit").map(Self::systemd_inhibit)
        } else {
            None
        }
    }

    fn caffeinate() -> Self {
        Self {
            after: KEEP_AWAKE_AFTER,
            program: PathBuf::from("/usr/bin/caffeinate"),
            args: ["-i", "-w", PID].map(str::to_owned).to_vec(),
        }
    }

    fn systemd_inhibit(program: PathBuf) -> Self {
        Self {
            after: KEEP_AWAKE_AFTER,
            program,
            args: [
                "--what=idle:sleep",
                "--who=DbRover",
                "--why=A report is running",
                "--mode=block",
                "tail",
                "--pid={pid}",
                "-f",
                "/dev/null",
            ]
            .map(str::to_owned)
            .to_vec(),
        }
    }

    /// `program` with `args`, `{pid}` in them standing for the worker's process id, started
    /// `after` into a run: what tests use, so that they never keep a machine awake.
    pub fn with_program(
        after: Duration,
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            after,
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// The command that keeps the machine awake while the process `pid` runs, its output
    /// discarded.
    pub fn command(&self, pid: u32) -> Command {
        let pid = pid.to_string();
        let mut command = Command::new(&self.program);
        command
            .args(self.args.iter().map(|arg| arg.replace(PID, &pid)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
}

/// `name` in one of the `PATH`'s folders.
fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|folder| folder.join(name))
        .find(|candidate| is_program(candidate))
}

fn is_program(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|part| part.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn waits_on_the_worker_with_the_systems_own_program() {
        assert_eq!(
            line(&KeepAwake::caffeinate().command(4242)),
            ["/usr/bin/caffeinate", "-i", "-w", "4242"]
        );
        let inhibit = KeepAwake::systemd_inhibit(PathBuf::from("/usr/bin/systemd-inhibit"));
        assert_eq!(
            line(&inhibit.command(7)),
            [
                "/usr/bin/systemd-inhibit",
                "--what=idle:sleep",
                "--who=DbRover",
                "--why=A report is running",
                "--mode=block",
                "tail",
                "--pid=7",
                "-f",
                "/dev/null"
            ]
        );
        assert_eq!(inhibit.after, KEEP_AWAKE_AFTER);
    }

    #[test]
    fn has_a_program_on_macos_and_none_on_windows() {
        let system = KeepAwake::system();
        if cfg!(target_os = "macos") {
            assert_eq!(system, Some(KeepAwake::caffeinate()));
        }
        if cfg!(windows) {
            assert_eq!(system, None);
        }
    }
}
