//! Running a snippet on Linux and macOS: `$SHELL -c '<snippet>' delightfile
//! <path>…`, the mechanism `crate::open`'s essay describes, moved here
//! unchanged. How a child is detached from the window is each target's own:
//! `setsid --fork` in front of the argv on Linux
//! ([`crate::platform::open::detached_argv`]), a process group of its own on
//! macOS ([`crate::platform::open::detach`]), and on macOS a thread that
//! collects the child when it exits ([`crate::platform::open::release`]).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::platform::open::{detach, detached_argv, release};

/// `$0` for every snippet delightfile runs.
const ARGV0: &str = "delightfile";

/// The shell, in `$SHELL` order of preference. POSIX `sh` is the fallback
/// because it is the one binary that is definitely there.
pub fn shell_program() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

/// The full argv for running `snippet` over `paths`.
///
/// Pure, and the reason it is: the argument order *is* the contract with every
/// opener rule in the shipped config (`"$@"`, `"$1"`), and getting `$0` wrong
/// would silently shift every one of them by one.
pub fn shell_argv(shell: &str, snippet: &str, paths: &[PathBuf]) -> Vec<String> {
    let mut argv = vec![
        shell.to_string(),
        "-c".to_string(),
        snippet.to_string(),
        ARGV0.to_string(),
    ];
    argv.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
    argv
}

fn command_from(argv: &[String], cwd: &Path) -> Option<Command> {
    let (program, args) = argv.split_first()?;
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd);
    Some(command)
}

/// Start `snippet` and do not wait for it (`;`, and every non-blocking opener).
pub fn spawn_detached(snippet: &str, paths: &[PathBuf], cwd: &Path) -> std::io::Result<()> {
    let plain = shell_argv(&shell_program(), snippet, paths);
    let argv = detached_argv(plain.clone());
    let detached = argv.len() != plain.len();
    let mut command =
        command_from(&argv, cwd).ok_or_else(|| std::io::Error::other("empty command"))?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut command);
    let mut child = command.spawn()?;
    if detached {
        // `setsid --fork` is gone the instant it has forked, so this reaps a
        // process that has already exited rather than waiting on the editor.
        let _ = child.wait();
    } else {
        release(child);
    }
    Ok(())
}

/// Run `snippet` to completion (`:`, and `block = true` openers). Called on a
/// task worker, never on the UI thread.
pub fn run_blocking(snippet: &str, paths: &[PathBuf], cwd: &Path) -> std::io::Result<i32> {
    let argv = shell_argv(&shell_program(), snippet, paths);
    let mut command =
        command_from(&argv, cwd).ok_or_else(|| std::io::Error::other("empty command"))?;
    let status = command.status()?;
    // What a shell's `$?` would say, a signalled child's 128 + signal included.
    Ok(df_core::platform::process::exit_code(&status))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract every opener rule in the shipped config depends on: the
    /// snippet is `-c`'s argument, `$0` is delightfile, and the paths start at
    /// `$1` — never spliced into the string.
    #[test]
    fn paths_reach_the_shell_as_arguments() {
        let paths = vec![
            PathBuf::from("/home/brian/a b.txt"),
            PathBuf::from("/home/brian/; rm -rf ~"),
        ];
        let argv = shell_argv("/bin/zsh", r#"zeditor "$@""#, &paths);
        assert_eq!(
            argv,
            vec![
                "/bin/zsh".to_string(),
                "-c".to_string(),
                r#"zeditor "$@""#.to_string(),
                "delightfile".to_string(),
                "/home/brian/a b.txt".to_string(),
                "/home/brian/; rm -rf ~".to_string(),
            ],
            "a semicolon in a file name is a character in an argument, not a command"
        );
        // `$1` is the first path, which is what the single-file openers use.
        assert_eq!(argv[4], "/home/brian/a b.txt");
        // …and with no selection the snippet still runs, with no positionals.
        assert_eq!(shell_argv("/bin/sh", "ls", &[]).len(), 4);
    }
}
