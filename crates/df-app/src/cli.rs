//! The command line: `delightfile [path]`, `--cwd-file=<path>`,
//! `--chooser-file=<path>` (PLAN §3).
//!
//! Hand-rolled rather than clap, for the same reason the TOML parser is
//! hand-rolled: there are four flags, and a dependency that parses them would
//! be larger than the program's whole config layer.
//!
//! The one flag that matters is `--cwd-file`. Brian's Hyprland `Super+F` runs
//! yazi with it and `cd`s the shell to whatever came back, so swapping `yazi`
//! for `delightfile` in that binding has to keep working on the first day —
//! `q` writes the final directory, `Q` deliberately does not (PLAN §4.1).
//!
//! `--chooser-file` is the same idea one step along: it is what makes this a
//! *system* file picker. `xdg-desktop-portal-termfilechooser` hands its wrapper
//! an output path and runs a file manager against it; whatever that manager
//! writes there, newline-separated, is what the browser's upload dialog gets
//! back. The flag is spelled and behaves exactly as yazi's does, because the
//! portal's wrapper contract is written against yazi and a picker that needs
//! its own wrapper is a picker nobody can drop in.

use std::path::PathBuf;

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    /// Where to start. `None` means the process's current directory.
    pub start: Option<PathBuf>,
    /// Where to write the final directory on a `q` quit.
    pub cwd_file: Option<PathBuf>,
    /// Where to write the chosen paths, one per line, when `Enter` picks
    /// something. `Some` is what puts the session in *chooser* mode at all:
    /// `Enter` on a file stops meaning "open it with the opener rules" and
    /// starts meaning "this is the one" (see `App::choose`).
    pub chooser_file: Option<PathBuf>,
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

  path                   the directory to open (default: the current one);
                         a file opens its directory with the cursor on it
  --cwd-file=<path>      write the final directory here when quitting with `q`
                         (`Q` quits without writing it)
  --chooser-file=<path>  pick rather than open: `Enter` writes the selected
                         paths here, one per line, and quits. Quitting any
                         other way writes nothing, which is a cancel.
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
            _ => match flag(&arg, "--cwd-file") {
                Flag::Value(path) => out.cwd_file = Some(PathBuf::from(path)),
                Flag::Empty => return Outcome::Fail("--cwd-file needs a path".to_string()),
                Flag::Absent => match flag(&arg, "--chooser-file") {
                    Flag::Value(path) => out.chooser_file = Some(PathBuf::from(path)),
                    Flag::Empty => return Outcome::Fail("--chooser-file needs a path".to_string()),
                    Flag::Absent => {
                        return Outcome::Fail(format!("unknown option `{arg}`\n\n{USAGE}"))
                    }
                },
            },
        }
    }
    Outcome::Run(out)
}

/// What `--name=value` had to say about itself.
enum Flag<'a> {
    /// This is not that flag.
    Absent,
    /// This is that flag, and it was given nothing.
    Empty,
    Value(&'a str),
}

/// Read `--name=value` out of one argument.
///
/// Two flags take a path and both reject an empty one, so the reading and the
/// rejecting are one function rather than the same three lines twice.
fn flag<'a>(arg: &'a str, name: &str) -> Flag<'a> {
    match arg
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('='))
    {
        Some(value) if !value.is_empty() => Flag::Value(value),
        Some(_) => Flag::Empty,
        None => Flag::Absent,
    }
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

/// Write the paths a chooser session picked, one per line, for the portal's
/// wrapper to read.
///
/// **Only ever called when something was picked.** An empty file is how every
/// consumer of this contract spells *cancelled*, so a session that ended any
/// other way must leave the file exactly as it found it — writing an empty
/// string would be indistinguishable, but truncating a file the portal had
/// put something in would not be.
pub fn write_chooser_file(path: &std::path::Path, paths: &[std::path::PathBuf]) {
    let mut out = Vec::new();
    for picked in paths {
        out.extend_from_slice(picked.as_os_str().as_encoded_bytes());
        out.push(b'\n');
    }
    if let Err(e) = std::fs::write(path, out) {
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
                chooser_file: None,
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
                chooser_file: None,
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
        assert!(matches!(parse_str(&["--chooser-file="]), Outcome::Fail(_)));
        assert!(matches!(parse_str(&["/tmp", "/var"]), Outcome::Fail(_)));
    }

    /// The shape `xdg-desktop-portal-termfilechooser`'s wrapper invokes, in
    /// all three of the modes it has: pick a file, pick several, pick a
    /// directory (which needs the cwd-file as well), and save (which is a pick
    /// aimed at a suggested name the portal has already created).
    #[test]
    fn the_portal_invocations_parse() {
        assert_eq!(
            parse_str(&["--chooser-file=/tmp/out", "/home/brian"]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/home/brian")),
                cwd_file: None,
                chooser_file: Some(PathBuf::from("/tmp/out")),
            })
        );
        assert_eq!(
            parse_str(&[
                "--chooser-file=/tmp/out",
                "--cwd-file=/tmp/out.1",
                "/home/brian",
            ]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/home/brian")),
                cwd_file: Some(PathBuf::from("/tmp/out.1")),
                chooser_file: Some(PathBuf::from("/tmp/out")),
            })
        );
        // Save: the suggested destination is a *file*, and `start_directory`
        // in `app.rs` is what turns it into "its directory, cursor on it".
        assert_eq!(
            parse_str(&["--chooser-file=/tmp/out", "/home/brian/Downloads/photo.jpg"]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/home/brian/Downloads/photo.jpg")),
                cwd_file: None,
                chooser_file: Some(PathBuf::from("/tmp/out")),
            })
        );
    }

    /// Nothing is written unless something was picked: an empty file is how
    /// the portal is told the dialog was cancelled, and a cancel that had
    /// truncated a file the portal wrote would be a different bug entirely.
    #[test]
    fn the_chooser_file_is_the_lines_that_were_picked() {
        let dir = std::env::temp_dir().join(format!("df-chooser-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let out = dir.join("out");
        write_chooser_file(
            &out,
            &[PathBuf::from("/tmp/one.txt"), PathBuf::from("/tmp/two.txt")],
        );
        assert_eq!(
            std::fs::read_to_string(&out).expect("written"),
            "/tmp/one.txt\n/tmp/two.txt\n"
        );
        // A name a `str` cannot hold still round-trips: the portal reads bytes.
        write_chooser_file(&out, &[PathBuf::from("/tmp/a b\tc")]);
        assert_eq!(
            std::fs::read_to_string(&out).expect("written"),
            "/tmp/a b\tc\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A directory really can be called `--help`.
    #[test]
    fn double_dash_ends_the_options() {
        assert_eq!(
            parse_str(&["--", "--help"]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("--help")),
                cwd_file: None,
                chooser_file: None,
            })
        );
    }
}
