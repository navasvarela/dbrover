//! A small program the engine's tests run as a command action's program, built from this crate
//! so it behaves the same on every OS, and the tests never depend on `sh` or `cmd`. It isn't
//! shipped: the app is packaged from `dbrover-app`'s binary alone.
//!
//! Its first argument says what to do:
//! - `dump` writes back, as JSON, its other arguments, its environment, its folder and its
//!   stdin;
//! - `print-file` writes the file its second argument names to stdout, as a password command
//!   whose output the test changes between runs;
//! - `copy` writes its stdin to its stdout as it reads it, as an export program would;
//! - `fail` writes to stderr and exits with status 3;
//! - `wait` sleeps until it is killed;
//! - `flood` writes to stdout until it is killed, even once nobody reads it.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::process::ExitCode;
use std::time::Duration;

const FAILED: u8 = 3;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("dbrover-test-program: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<ExitCode> {
    let mut arguments = std::env::args_os().skip(1).map(|argument| {
        argument
            .into_string()
            .unwrap_or_else(|raw| raw.to_string_lossy().into_owned())
    });
    let mode = arguments.next().unwrap_or_default();
    match mode.as_str() {
        "dump" => dump(arguments.collect())?,
        "print-file" => {
            let path = arguments.next().unwrap_or_default();
            io::stdout().write_all(&std::fs::read(path)?)?;
        }
        "copy" => {
            io::copy(&mut io::stdin().lock(), &mut io::stdout().lock())?;
        }
        "fail" => {
            eprint!("it went wrong");
            return Ok(ExitCode::from(FAILED));
        }
        "wait" => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        "flood" => {
            let line = [b'x'; 1024];
            let mut stdout = io::stdout().lock();
            loop {
                // Like a program that ignores a closed pipe: only being killed stops it.
                let _ = stdout.write_all(&line);
            }
        }
        _ => {
            eprintln!("dbrover-test-program: no such mode {mode:?}");
            return Ok(ExitCode::FAILURE);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn dump(arguments: Vec<String>) -> io::Result<()> {
    let mut stdin = String::new();
    io::stdin().read_to_string(&mut stdin)?;
    let environment: BTreeMap<String, String> = std::env::vars_os()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let directory = std::env::current_dir()?;
    let dumped = serde_json::json!({
        "arguments": arguments,
        "environment": environment,
        "directory": directory,
        "stdin": stdin,
    });
    serde_json::to_writer(io::stdout(), &dumped).map_err(io::Error::from)
}
