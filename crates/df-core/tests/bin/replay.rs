//! A stand-in SFTP server for the vfs tests on Windows, where there is no
//! `/bin/sh` to replay bytes with (`src/vfs/tests.rs`, "Fake servers").
//!
//! Built as an example, which `cargo test` builds before it runs a test, so
//! the tests find it beside their own binary, in the target directory's
//! `examples/`. Three ways to be a server that misbehaves:
//!
//! - `replay <file> [<pid file>]` — write the file's bytes to stdout, then
//!   hold the pipes open until the client hangs up (stdin ends); with a pid
//!   file, write this process's id there first, so a test can end it.
//! - `fail <code> <words>` — say the words on stderr and exit with the code,
//!   as `ssh` does when it cannot log in.
//! - `quiet` — exit at once without a word.

use std::io::{Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["replay", file, rest @ ..] => replay(file, rest.first().copied()),
        ["fail", code, words] => {
            eprintln!("{words}");
            ExitCode::from(code.parse::<u8>().unwrap_or(1))
        }
        ["quiet"] => ExitCode::SUCCESS,
        _ => {
            eprintln!("usage: replay <file> [<pid file>] | fail <code> <words> | quiet");
            ExitCode::from(2)
        }
    }
}

fn replay(file: &str, pid_file: Option<&str>) -> ExitCode {
    if let Some(pid_file) = pid_file {
        if std::fs::write(pid_file, std::process::id().to_string()).is_err() {
            return ExitCode::from(3);
        }
    }
    let Ok(bytes) = std::fs::read(file) else {
        return ExitCode::from(4);
    };
    let mut out = std::io::stdout().lock();
    if out.write_all(&bytes).and_then(|()| out.flush()).is_err() {
        return ExitCode::from(5);
    }
    // Hold the pipes open until the client hangs up.
    let mut sink = [0u8; 4096];
    let mut input = std::io::stdin().lock();
    while matches!(input.read(&mut sink), Ok(n) if n > 0) {}
    ExitCode::SUCCESS
}
