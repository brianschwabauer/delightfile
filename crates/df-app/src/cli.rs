//! The command line: `delightfile [path]`, `--cwd-file=<path>` (PLAN §3).
//!
//! Hand-rolled rather than clap, for the same reason the TOML parser is
//! hand-rolled: there are three flags, and a dependency that parses them would
//! be larger than the program's whole config layer.
//!
//! The one flag that matters is `--cwd-file`. Brian's Hyprland `Super+F` runs
//! yazi with it and `cd`s the shell to whatever came back, so swapping `yazi`
//! for `delightfile` in that binding has to keep working on the first day —
//! `q` writes the final directory, `Q` deliberately does not (PLAN §4.1).

use std::path::PathBuf;

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    /// Where to start. `None` means the process's current directory.
    pub start: Option<PathBuf>,
    /// Where to write the final directory on a `q` quit.
    pub cwd_file: Option<PathBuf>,
}

/// What `main` should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Run(Args),
    /// Print this and exit 0 — `--help`, `--version`.
    Print(String),
    /// Print this to stderr and exit 2.
    Fail(String),
}

/// The `--help` text. Written out rather than generated: it is the one place a
/// person reads what the flags are, so it is prose, not a table dump.
pub const USAGE: &str = "\
delightfile — a keyboard-first file manager for Wayland

usage: delightfile [path] [options]

  path                   the directory to open (default: the current one)
  --cwd-file=<path>      write the final directory here when quitting with `q`
                         (`Q` quits without writing it)
  -h, --help             show this
  -V, --version          show the version
";

/// Parse everything after the program name.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Outcome {
    let mut out = Args::default();
    let mut rest_are_paths = false;
    for arg in args {
        if rest_are_paths || !arg.starts_with('-') {
            if out.start.is_some() {
                return Outcome::Fail(format!("only one path can be opened (got `{arg}` as well)"));
            }
            out.start = Some(PathBuf::from(arg));
            continue;
        }
        match arg.as_str() {
            // The POSIX end-of-options marker, so a directory literally called
            // `--help` is still openable.
            "--" => rest_are_paths = true,
            "-h" | "--help" => return Outcome::Print(USAGE.to_string()),
            "-V" | "--version" => {
                return Outcome::Print(format!("delightfile {}\n", env!("CARGO_PKG_VERSION")))
            }
            _ => match arg.strip_prefix("--cwd-file=") {
                Some(path) if !path.is_empty() => out.cwd_file = Some(PathBuf::from(path)),
                Some(_) => return Outcome::Fail("--cwd-file needs a path".to_string()),
                None => return Outcome::Fail(format!("unknown option `{arg}`\n\n{USAGE}")),
            },
        }
    }
    Outcome::Run(out)
}

/// Write the directory a session ended in, for the shell wrapper to read.
///
/// A failure here is logged and swallowed: the user asked to quit, and refusing
/// to exit because a path could not be written would be a worse answer than a
/// shell that stays where it was.
pub fn write_cwd_file(path: &std::path::Path, cwd: &std::path::Path) {
    if let Err(e) = std::fs::write(path, cwd.as_os_str().as_encoded_bytes()) {
        log::warn!("could not write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(args: &[&str]) -> Outcome {
        parse(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn a_bare_path_is_the_starting_directory() {
        assert_eq!(
            parse_str(&["/tmp"]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/tmp")),
                cwd_file: None,
            })
        );
    }

    #[test]
    fn the_hyprland_invocation_parses() {
        // The shape of the binding this has to keep working (PLAN §3).
        assert_eq!(
            parse_str(&["--cwd-file=/tmp/yazi-cwd", "/home/brian"]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/home/brian")),
                cwd_file: Some(PathBuf::from("/tmp/yazi-cwd")),
            })
        );
    }

    #[test]
    fn help_and_version_print_and_stop() {
        assert!(matches!(parse_str(&["--help"]), Outcome::Print(_)));
        assert!(matches!(parse_str(&["-V"]), Outcome::Print(_)));
    }

    #[test]
    fn nonsense_fails_rather_than_being_guessed_at() {
        assert!(matches!(parse_str(&["--nope"]), Outcome::Fail(_)));
        assert!(matches!(parse_str(&["--cwd-file="]), Outcome::Fail(_)));
        assert!(matches!(parse_str(&["/tmp", "/var"]), Outcome::Fail(_)));
    }

    /// A directory really can be called `--help`.
    #[test]
    fn double_dash_ends_the_options() {
        assert_eq!(
            parse_str(&["--", "--help"]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("--help")),
                cwd_file: None,
            })
        );
    }
}
