//! Dump and restore through the engines' own programs (W17, `docs/dump-restore.md`): where
//! `pg_dump`, `mysqldump` and the rest are installed, which copy fits a server, and the
//! arguments and password files they are run with.
//!
//! Nothing here starts a program or writes a file: the engine does, so all of this is tested
//! on its own, against each program's real output and each system's install folders.

mod arguments;
mod choice;
mod connection;
mod discovery;
mod program;
mod progress;
mod secret;
mod version;

pub use arguments::{
    ArgumentError, DumpCommand, DumpSpec, RunFiles, Stdout, command_line, dump_command,
};
pub use choice::{Choice, FoundProgram, Need, NoFit, choose, install_hint};
pub use connection::{APPLICATION_NAME, ConnectionError, Target};
pub use discovery::{Folders, RealFolders, SearchContext, find};
pub use program::{Os, Program, Role};
pub use progress::dumped_table;
pub use secret::{SecretError, password_file};
pub use version::{ProgramVersion, Version};
