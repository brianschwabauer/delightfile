//! The command line: `delightfile [path]`, `--cwd-file=<path>`,
//! `--chooser-file=<path>` and the three `--chooser-*` switches that say what
//! kind of dialog it is standing in for (PLAN §3).
//!
//! Hand-rolled rather than clap, for the same reason the TOML parser is
//! hand-rolled: there are a handful of flags, and a dependency that parses them
//! would be larger than the program's whole config layer.
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
//!
//! The three switches beside it are delightfile's own. yazi is told nothing
//! about the dialog and gets away with it because it is a terminal program the
//! wrapper can drive around the edges — a folder dialog there is a cwd-file
//! promoted to the answer after the fact, which is also why `q` in yazi can
//! never cancel one. A window that draws its own "Select" button has to know
//! what the button is selecting, so the wrapper passes the portal's
//! `multiple`, `directory` and `save` straight through.

use std::path::PathBuf;

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    /// Where to start. `None` means the process's current directory.
    pub start: Option<PathBuf>,
    /// Where to write the final directory on a `q` quit.
    pub cwd_file: Option<PathBuf>,
    /// The dialog this session is standing in for. `Some` is what puts the
    /// session in *chooser* mode at all: `Enter` on a file stops meaning "open
    /// it with the opener rules" and starts meaning "this is the one" (see
    /// `App::choose`).
    pub chooser: Option<Chooser>,
}

/// A file dialog, as the portal described it (`--chooser-file` and its three
/// switches).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chooser {
    /// Where to write the chosen paths, one per line, when something is
    /// picked. Written only then: see [`write_chooser_file`].
    pub out: PathBuf,
    /// `--chooser-multiple`: the dialog takes more than one path. Without it
    /// the selection is held to one row.
    pub multiple: bool,
    /// `--chooser-directory`: the answer is a folder, not a file.
    pub directory: bool,
    /// `--chooser-save`: the answer is a name to save *to*, which need not
    /// exist yet.
    pub save: bool,
    /// What the calling program called the dialog ("Open File", "Upload"),
    /// when it said. The window's title; `None` is the plain picker.
    pub title: Option<String>,
    /// The verb the caller put on its button ("Upload", "Attach"), with any
    /// GTK mnemonic underscore already stripped. `None` is the mode's own
    /// word: Select, Choose folder, Save.
    pub accept: Option<String>,
    /// A save's suggested file name, when the caller suggested one and no file
    /// by that name exists yet — the `Save as:` prompt's prefill.
    pub name: Option<String>,
    /// The file-type filters the caller offers, in its order. Empty means the
    /// dialog shows every file.
    pub filters: Vec<TypeFilter>,
    /// Which of `filters` starts active: the caller's `current_filter` when it
    /// names one, else the first. Meaningless when `filters` is empty.
    pub current_filter: usize,
}

/// One named file-type filter, as the file-chooser portal describes it: a
/// label for the menu and the patterns a file has to match to be shown.
///
/// Defined in df-core, where it is applied: a filter narrows a listing's view
/// the way the hidden toggle does, and the view is df-core's. The matching
/// rules are documented there.
pub use df_core::fs::TypeFilter;

impl Chooser {
    /// A plain dialog writing to `out`: one file, no title, no filters. The
    /// switches and the request file fill in the rest.
    pub fn new(out: PathBuf) -> Chooser {
        Chooser {
            out,
            multiple: false,
            directory: false,
            save: false,
            title: None,
            accept: None,
            name: None,
            filters: Vec::new(),
            current_filter: 0,
        }
    }
}

/// What a [`Chooser`] is choosing, read off its switches in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    /// One existing file.
    File,
    /// One or more existing files.
    Files,
    /// A folder (or several, with `--chooser-multiple`).
    Folder,
    /// A name to save to.
    Save,
}

impl Chooser {
    /// Which of the four dialogs this is.
    ///
    /// `save` outranks `directory`, the order every shipped wrapper tests them
    /// in. The portal never sends both — it implements `SaveFile` and refuses a
    /// directory as its answer — so the order only has to be *an* order, and
    /// the one the other wrappers already use is the least surprising.
    pub fn mode(&self) -> PickMode {
        if self.save {
            PickMode::Save
        } else if self.directory {
            PickMode::Folder
        } else if self.multiple {
            PickMode::Files
        } else {
            PickMode::File
        }
    }
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
  --chooser-file=<path>  pick rather than open: `Enter` or the Select button
                         (`Ctrl+Enter`) writes the picked paths here, one per
                         line, and quits. Quitting any other way writes
                         nothing, which is a cancel.
  --chooser-multiple     the dialog takes several files (default: one)
  --chooser-directory    the dialog wants a folder: Choose folder picks the
                         selected folders, or the one you are in
  --chooser-save         the dialog is a save: pick a file to replace, or
                         Save to type a new name
                         (the three above need --chooser-file)
  -h, --help             show this
  -V, --version          show the version
";

/// Parse everything after the program name.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Outcome {
    let mut out = Args::default();
    let mut rest_are_paths = false;
    // The switches are collected on their own and folded into a `Chooser`
    // at the end, because they may come before `--chooser-file` as well as
    // after it — the order a person types flags in is not a meaning.
    let mut chooser_file: Option<PathBuf> = None;
    let (mut multiple, mut directory, mut save) = (false, false, false);
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
            "--chooser-multiple" => multiple = true,
            "--chooser-directory" => directory = true,
            "--chooser-save" => save = true,
            _ => match flag(&arg, "--cwd-file") {
                Flag::Value(path) => out.cwd_file = Some(PathBuf::from(path)),
                Flag::Empty => return Outcome::Fail("--cwd-file needs a path".to_string()),
                Flag::Absent => match flag(&arg, "--chooser-file") {
                    Flag::Value(path) => chooser_file = Some(PathBuf::from(path)),
                    Flag::Empty => return Outcome::Fail("--chooser-file needs a path".to_string()),
                    Flag::Absent => {
                        return Outcome::Fail(format!("unknown option `{arg}`\n\n{USAGE}"))
                    }
                },
            },
        }
    }
    match chooser_file {
        Some(out_path) => {
            out.chooser = Some(Chooser {
                multiple,
                directory,
                save,
                ..Chooser::new(out_path)
            })
        }
        // A switch with nothing to write its answer to is a wrapper that has
        // lost an argument. Refused rather than ignored: the window would open
        // as a plain file manager, and the dialog behind it would wait on a
        // pick that has nowhere to go.
        None if multiple || directory || save => {
            let which = [
                (multiple, "--chooser-multiple"),
                (directory, "--chooser-directory"),
                (save, "--chooser-save"),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join(", ");
            return Outcome::Fail(format!("{which}: only meaningful with --chooser-file"));
        }
        None => {}
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
                chooser: None,
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
                chooser: None,
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

    /// A chooser writing to `/tmp/out`, with the three switches as given.
    fn chooser(multiple: bool, directory: bool, save: bool) -> Option<Chooser> {
        Some(Chooser {
            multiple,
            directory,
            save,
            ..Chooser::new(PathBuf::from("/tmp/out"))
        })
    }

    /// The shapes `build/delightfile-wrapper.sh` invokes, one per dialog the
    /// portal has: pick a file, pick several, pick a folder, and save (which
    /// is aimed at a suggested name the portal has already created).
    #[test]
    fn the_portal_invocations_parse() {
        assert_eq!(
            parse_str(&["--chooser-file=/tmp/out", "/home/brian"]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/home/brian")),
                cwd_file: None,
                chooser: chooser(false, false, false),
            })
        );
        assert_eq!(
            parse_str(&[
                "--chooser-file=/tmp/out",
                "--chooser-multiple",
                "/home/brian"
            ]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/home/brian")),
                cwd_file: None,
                chooser: chooser(true, false, false),
            })
        );
        // A folder dialog no longer needs a cwd-file: the pick is written by
        // the window, so `q` can cancel it like any other.
        assert_eq!(
            parse_str(&[
                "--chooser-file=/tmp/out",
                "--chooser-directory",
                "/home/brian"
            ]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/home/brian")),
                cwd_file: None,
                chooser: chooser(false, true, false),
            })
        );
        // Save: the suggested destination is a *file*, and `start_directory`
        // in `app.rs` is what turns it into "its directory, cursor on it".
        assert_eq!(
            parse_str(&[
                "--chooser-file=/tmp/out",
                "--chooser-save",
                "/home/brian/Downloads/photo.jpg",
            ]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/home/brian/Downloads/photo.jpg")),
                cwd_file: None,
                chooser: chooser(false, false, true),
            })
        );
    }

    /// The switches are switches wherever they stand: before the file they
    /// qualify, after it, or all at once.
    #[test]
    fn the_chooser_switches_parse_in_any_order() {
        assert_eq!(
            parse_str(&[
                "--chooser-save",
                "--chooser-directory",
                "/home/brian",
                "--chooser-multiple",
                "--chooser-file=/tmp/out",
            ]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/home/brian")),
                cwd_file: None,
                chooser: chooser(true, true, true),
            })
        );
    }

    /// A switch without `--chooser-file` has nowhere to send its answer, and
    /// says which switch it was rather than guessing at a mode.
    #[test]
    fn a_chooser_switch_needs_a_chooser_file() {
        for switch in [
            "--chooser-multiple",
            "--chooser-directory",
            "--chooser-save",
        ] {
            match parse_str(&[switch, "/home/brian"]) {
                Outcome::Fail(message) => assert!(message.contains(switch), "{message}"),
                other => panic!("{switch} alone parsed as {other:?}"),
            }
        }
        // A value is not how a switch is spelled.
        assert!(matches!(
            parse_str(&["--chooser-file=/tmp/out", "--chooser-save=1"]),
            Outcome::Fail(_)
        ));
    }

    /// Which dialog the switches describe, in the order the shipped wrappers
    /// test them: save before directory before multiple.
    #[test]
    fn the_switches_name_one_dialog() {
        let mode = |multiple, directory, save| {
            chooser(multiple, directory, save)
                .map(|c| c.mode())
                .expect("a chooser")
        };
        assert_eq!(mode(false, false, false), PickMode::File);
        assert_eq!(mode(true, false, false), PickMode::Files);
        assert_eq!(mode(false, true, false), PickMode::Folder);
        assert_eq!(mode(true, true, false), PickMode::Folder);
        assert_eq!(mode(false, false, true), PickMode::Save);
        assert_eq!(mode(false, true, true), PickMode::Save);
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
                chooser: None,
            })
        );
    }
}
